//! `flockr feed`: inspect and act on job feeds through the socket API, and
//! convert other tools' JSON into the feed format.

use std::io::Read;
use std::time::{Duration, Instant};

use crate::api::schema::{
    FeedGetParams, FeedItemInfo, FeedItemState, FeedListParams, FeedRunActionParams, FeedSummary,
    FeedTarget, Method, Request, ResponseResult,
};
use crate::feed::adapters::workbox::{self, AdaptOptions};
use crate::jobs::display;

use super::job::call;

const USAGE: &str = "usage: flockr feed <list [--json]|show <job> [--json]|refresh <job> [--no-wait]|action <job> <item> <key> [--no-wait]|adapt workbox [--title TITLE] [--bin PROGRAM]>";
const WAIT_POLL: Duration = Duration::from_millis(250);
const WAIT_LIMIT: Duration = Duration::from_secs(150);
/// The largest input `adapt` reads from stdin.
const MAX_ADAPT_INPUT: u64 = 32 * 1024 * 1024;

pub(super) fn run_feed_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    match subcommand {
        "list" => feed_list(rest),
        "show" => feed_show(rest),
        "refresh" => feed_refresh(rest),
        "action" => feed_action(rest),
        "adapt" => feed_adapt(rest),
        "help" | "--help" | "-h" => {
            eprintln!("{USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn state_glyph(state: FeedItemState) -> &'static str {
    match state {
        FeedItemState::Unread => "●",
        FeedItemState::Read => "○",
        FeedItemState::Done => "✓",
        FeedItemState::Warn => "!",
        FeedItemState::Error => "✖",
    }
}

fn feed_status(feed: &FeedSummary, now_ms: u64) -> String {
    if feed.refreshing {
        return "refreshing".into();
    }
    match (&feed.error, feed.fetched_unix_ms) {
        (Some(_), Some(at)) => format!("stale, last good {}", display::ago(now_ms, at)),
        (Some(error), None) => format!("failed: {error}"),
        (None, Some(at)) => format!("updated {}", display::ago(now_ms, at)),
        (None, None) => "not read yet".into(),
    }
}

fn feed_list(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: flockr feed list [--json]");
            return Ok(2);
        }
    };
    let method = || Method::FeedList(FeedListParams::default());
    if json {
        return super::print_response(&super::send_request(&Request {
            id: "cli:feed:list".into(),
            method: method(),
        })?);
    }
    let result = match call("cli:feed:list", method())? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::FeedList { feeds, .. } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    if feeds.is_empty() {
        println!("no feeds; add a feed table to a job.toml (see the Jobs docs)");
        return Ok(0);
    }
    let now = crate::jobs::now_unix_ms();
    let id_width = feeds
        .iter()
        .map(|feed| feed.job_id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    println!(
        "{:<id_width$}  {:>5}  {:>6}  {:<28}  TITLE",
        "JOB", "ITEMS", "UNREAD", "STATUS"
    );
    for feed in &feeds {
        println!(
            "{:<id_width$}  {:>5}  {:>6}  {:<28}  {}",
            feed.job_id,
            feed.count,
            feed.unread,
            feed_status(feed, now).chars().take(28).collect::<String>(),
            feed.title
        );
    }
    Ok(0)
}

fn print_item(item: &FeedItemInfo, now_ms: u64) {
    let mut line = format!("{} ", state_glyph(item.state));
    if let Some(badge) = &item.badge {
        line.push_str(&format!("[{badge}] "));
    }
    line.push_str(&item.title);
    if let Some(age) = item
        .updated_at
        .as_deref()
        .and_then(crate::feed::format::parse_rfc3339_secs)
        .and_then(|secs| u64::try_from(secs).ok())
        .map(|secs| display::ago(now_ms, secs.saturating_mul(1000)))
    {
        line.push_str(&format!("  ({age})"));
    }
    println!("{line}");
    let mut detail = Vec::new();
    if let Some(subtitle) = &item.subtitle {
        detail.push(subtitle.clone());
    }
    if let Some(url) = &item.url {
        detail.push(url.clone());
    }
    if !detail.is_empty() {
        println!("    {}", detail.join("  "));
    }
    if !item.actions.is_empty() {
        let actions = item
            .actions
            .iter()
            .map(|action| format!("{}:{}", action.key, action.title))
            .collect::<Vec<_>>()
            .join("  ");
        println!("    actions: {actions}  (id {})", item.id);
    } else {
        println!("    id {}", item.id);
    }
}

fn feed_show(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr feed show <job> [--json]";
    let (job_id, json) = match args {
        [id] if !id.starts_with('-') => (id.as_str(), false),
        [id, flag] if !id.starts_with('-') && flag == "--json" => (id.as_str(), true),
        [flag, id] if flag == "--json" && !id.starts_with('-') => (id.as_str(), true),
        _ => {
            eprintln!("{usage}");
            return Ok(2);
        }
    };
    let method = || {
        Method::FeedGet(FeedGetParams {
            job_id: job_id.to_owned(),
            since_revision: None,
            limit: None,
        })
    };
    if json {
        return super::print_response(&super::send_request(&Request {
            id: "cli:feed:show".into(),
            method: method(),
        })?);
    }
    let result = match call("cli:feed:show", method())? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::FeedDetail { feed, items, .. } = result else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    let now = crate::jobs::now_unix_ms();
    println!(
        "{} ({}): {} items, {} unread, {}",
        feed.title,
        feed.job_id,
        feed.count,
        feed.unread,
        feed_status(&feed, now)
    );
    if let Some(error) = &feed.error {
        println!("last refresh failed: {error}");
    }
    for item in &items {
        print_item(item, now);
    }
    if !feed.diagnostics.is_empty() {
        println!("feed notes:");
        for line in &feed.diagnostics {
            println!("  {line}");
        }
    }
    Ok(0)
}

/// Poll until the feed's task has finished. Returns the summary.
fn wait_for_feed(job_id: &str) -> std::io::Result<Result<FeedSummary, i32>> {
    let deadline = Instant::now() + WAIT_LIMIT;
    loop {
        std::thread::sleep(WAIT_POLL);
        let result = match call(
            "cli:feed:wait",
            Method::FeedGet(FeedGetParams {
                job_id: job_id.to_owned(),
                since_revision: None,
                limit: Some(0),
            }),
        )? {
            Ok(result) => result,
            Err(code) => return Ok(Err(code)),
        };
        let ResponseResult::FeedDetail { feed, .. } = result else {
            return Ok(Err(1));
        };
        if !feed.refreshing {
            return Ok(Ok(feed));
        }
        if Instant::now() >= deadline {
            eprintln!("still running; check `flockr feed list` later");
            return Ok(Err(1));
        }
    }
}

fn current_summary(job_id: &str) -> std::io::Result<Option<FeedSummary>> {
    Ok(
        match call(
            "cli:feed:before",
            Method::FeedGet(FeedGetParams {
                job_id: job_id.to_owned(),
                since_revision: None,
                limit: Some(0),
            }),
        )? {
            Ok(ResponseResult::FeedDetail { feed, .. }) => Some(feed),
            _ => None,
        },
    )
}

fn split_wait_flag(args: &[String]) -> (Vec<&String>, bool) {
    let wait = !args.iter().any(|arg| arg == "--no-wait");
    (
        args.iter().filter(|arg| *arg != "--no-wait").collect(),
        wait,
    )
}

fn print_outcome(feed: &FeedSummary) -> i32 {
    match (&feed.error, feed.fetched_unix_ms) {
        (None, _) => {
            println!(
                "{}: {} items, {} unread",
                feed.title, feed.count, feed.unread
            );
            0
        }
        (Some(error), Some(_)) => {
            println!(
                "refresh failed; keeping the last good feed ({} items): {error}",
                feed.count
            );
            1
        }
        (Some(error), None) => {
            println!("refresh failed: {error}");
            1
        }
    }
}

fn feed_refresh(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr feed refresh <job> [--no-wait]";
    let (positional, wait) = split_wait_flag(args);
    let [job_id] = positional.as_slice() else {
        eprintln!("{usage}");
        return Ok(2);
    };
    if job_id.starts_with('-') {
        eprintln!("{usage}");
        return Ok(2);
    }
    let result = match call(
        "cli:feed:refresh",
        Method::FeedRefresh(FeedTarget {
            job_id: (*job_id).clone(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::FeedRefreshRequested {
        started, reason, ..
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    if !started {
        println!(
            "not refreshed: {}",
            reason.unwrap_or_else(|| "unknown".into())
        );
        return Ok(1);
    }
    if !wait {
        println!("refreshing {job_id}");
        return Ok(0);
    }
    match wait_for_feed(job_id)? {
        Ok(feed) => Ok(print_outcome(&feed)),
        Err(code) => Ok(code),
    }
}

fn feed_action(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr feed action <job> <item> <key> [--no-wait]";
    let (positional, wait) = split_wait_flag(args);
    let [job_id, item_id, key] = positional.as_slice() else {
        eprintln!("{usage}");
        return Ok(2);
    };
    if job_id.starts_with('-') {
        eprintln!("{usage}");
        return Ok(2);
    }
    let before = current_summary(job_id)?
        .and_then(|feed| feed.last_action.map(|action| action.finished_unix_ms));
    let result = match call(
        "cli:feed:action",
        Method::FeedRunAction(FeedRunActionParams {
            job_id: (*job_id).clone(),
            item_id: (*item_id).clone(),
            key: (*key).clone(),
        }),
    )? {
        Ok(result) => result,
        Err(code) => return Ok(code),
    };
    let ResponseResult::FeedActionRequested {
        started, reason, ..
    } = result
    else {
        eprintln!("unexpected response from the server");
        return Ok(1);
    };
    if !started {
        println!("not run: {}", reason.unwrap_or_else(|| "unknown".into()));
        return Ok(1);
    }
    if !wait {
        println!("running action {key} on {item_id}");
        return Ok(0);
    }
    let feed = match wait_for_feed(job_id)? {
        Ok(feed) => feed,
        Err(code) => return Ok(code),
    };
    let ours = feed
        .last_action
        .as_ref()
        .filter(|action| Some(action.finished_unix_ms) != before);
    let Some(action) = ours else {
        eprintln!("the action's result was not recorded");
        return Ok(1);
    };
    println!(
        "action {} on {}: {}",
        action.key,
        action.item_id,
        if action.ok {
            "ok".to_owned()
        } else {
            format!("failed: {}", action.summary)
        }
    );
    let refresh_code = print_outcome(&feed);
    Ok(if action.ok { refresh_code } else { 1 })
}

fn feed_adapt(args: &[String]) -> std::io::Result<i32> {
    let usage = "usage: flockr feed adapt workbox [--title TITLE] [--bin PROGRAM]  (reads workbox's JSON array on stdin, prints a feed)";
    let Some(adapter) = args.first().map(String::as_str) else {
        eprintln!("{usage}");
        return Ok(2);
    };
    if adapter != "workbox" {
        eprintln!("unknown adapter {adapter:?}; available: workbox");
        return Ok(2);
    }
    let mut options = AdaptOptions::default();
    let mut index = 1;
    while index < args.len() {
        let value = |index: usize| args.get(index + 1).filter(|value| !value.is_empty());
        match args[index].as_str() {
            "--title" => {
                let Some(value) = value(index) else {
                    eprintln!("{usage}");
                    return Ok(2);
                };
                options.title = value.clone();
                index += 2;
            }
            "--bin" => {
                let Some(value) = value(index) else {
                    eprintln!("{usage}");
                    return Ok(2);
                };
                options.bin = value.clone();
                index += 2;
            }
            _ => {
                eprintln!("{usage}");
                return Ok(2);
            }
        }
    }
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .take(MAX_ADAPT_INPUT)
        .read_to_string(&mut input)?;
    match workbox::adapt(&input, &options) {
        Ok((feed, warnings)) => {
            for warning in warnings {
                eprintln!("flockr feed adapt: {warning}");
            }
            println!("{feed}");
            Ok(0)
        }
        Err(message) => {
            eprintln!("flockr feed adapt: {message}");
            Ok(1)
        }
    }
}
