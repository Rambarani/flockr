use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde_json::json;

use crate::api::schema::{Method, PaneSendInputParams, Request, TabCreateParams};
use crate::upstream::apply::{describe_plan, execute_apply, plan_apply};
use crate::upstream::check::{check_params, fetch_releases_curl, run_check, unix_now};
use crate::upstream::release::{
    bucket_counts_label, bucket_release_body, parse_tag_version, UpstreamRelease,
};
use crate::upstream::review::{
    parse_choice, render_full, render_summary, ReviewChoice, PROMPT_LINE,
};
use crate::upstream::state::{state_path, UpstreamState};
use crate::upstream::sync::{
    exec_agent, preflight_repo, resolve_repo_path, run_sync, shell_quote_path, validate_tag,
    SyncOptions,
};

const USAGE: &str = "usage: flockr upstream <check [--json]|status [--json]|review|copy [TAG]|skip TAG|sync TAG|apply TAG [--yes]>";

pub(super) fn run_upstream_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    match subcommand {
        "check" => upstream_check(rest),
        "status" => upstream_status(rest),
        "review" => upstream_review(rest),
        "copy" => upstream_copy(rest),
        "skip" => upstream_skip(rest),
        "sync" => upstream_sync(rest),
        "apply" => upstream_apply(rest),
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

fn parse_json_flag(args: &[String], usage: &str) -> Result<bool, i32> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => {
            eprintln!("{usage}");
            Err(2)
        }
    }
}

fn upstream_config() -> crate::config::UpstreamConfig {
    crate::config::Config::load().config.upstream
}

fn tag_label(version: &crate::update::Version) -> String {
    format!("v{version}")
}

fn release_json(release: &UpstreamRelease) -> serde_json::Value {
    let buckets = bucket_release_body(&release.body);
    json!({
        "tag": release.tag,
        "name": release.name,
        "prerelease": release.prerelease,
        "published_at": release.published_at,
        "url": release.url,
        "features": buckets.features,
        "fixes": buckets.fixes,
        "other": buckets.other,
        "body": release.body,
    })
}

fn print_pending_text(base: &str, pending: &[UpstreamRelease]) {
    if pending.is_empty() {
        println!("flockr is synced to herdr {base}; no new upstream releases.");
        return;
    }
    println!(
        "flockr is synced to herdr {base}; {} new upstream release{}:",
        pending.len(),
        if pending.len() == 1 { "" } else { "s" }
    );
    for release in pending {
        let date = release
            .published_at
            .as_deref()
            .and_then(|stamp| stamp.get(..10))
            .unwrap_or("");
        let counts = bucket_counts_label(&bucket_release_body(&release.body))
            .unwrap_or_else(|| "see release notes".to_string());
        println!("  {}  {date}  {counts}", release.tag);
        if !release.url.is_empty() {
            println!("    {}", release.url);
        }
    }
    println!();
    println!("Review and copy updates with `flockr upstream review`.");
}

fn upstream_check(args: &[String]) -> std::io::Result<i32> {
    let json_output = match parse_json_flag(args, "usage: flockr upstream check [--json]") {
        Ok(json_output) => json_output,
        Err(code) => return Ok(code),
    };
    let config = upstream_config();
    let outcome = run_check(
        &state_path(),
        &check_params(&config, true, false),
        &fetch_releases_curl,
    );
    let base = tag_label(&outcome.base);
    if json_output {
        let value = json!({
            "ok": outcome.error.is_none(),
            "base": base,
            "checked": outcome.fetched,
            "error": outcome.error,
            "pending": outcome.pending.iter().map(release_json).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else if let Some(error) = &outcome.error {
        eprintln!("upstream check failed: {error}");
        if !outcome.pending.is_empty() {
            eprintln!("showing cached results from the last successful check:");
            print_pending_text(&base, &outcome.pending);
        }
    } else {
        print_pending_text(&base, &outcome.pending);
    }
    Ok(if outcome.error.is_some() { 1 } else { 0 })
}

fn relative_age(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=89 => format!("{secs}s ago"),
        90..=5399 => format!("{}m ago", secs / 60),
        5400..=172_799 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn upstream_status(args: &[String]) -> std::io::Result<i32> {
    let json_output = match parse_json_flag(args, "usage: flockr upstream status [--json]") {
        Ok(json_output) => json_output,
        Err(code) => return Ok(code),
    };
    let config = upstream_config();
    let state = UpstreamState::load();
    let compiled = crate::upstream::compiled_base();
    let effective = state.effective_base(&compiled);
    let repo = resolve_repo_path(config.repo_path.as_deref());
    let pending: Vec<&UpstreamRelease> = state
        .pending
        .iter()
        .filter(|release| {
            parse_tag_version(&release.tag).is_some_and(|version| version > effective)
                && !state.is_skipped(&release.tag)
                && (config.include_prereleases || !release.prerelease)
        })
        .collect();
    let now = unix_now();

    if json_output {
        let value = json!({
            "upstream": crate::product::UPSTREAM_REPO,
            "enabled": config.enabled,
            "check_interval_hours": config.check_interval_hours,
            "include_prereleases": config.include_prereleases,
            "compiled_base": tag_label(&compiled),
            "base": tag_label(&effective),
            "last_check_unix": state.last_check_unix,
            "last_error": state.last_error,
            "last_notified_tag": state.last_notified_tag,
            "skipped_tags": state.skipped_tags,
            "pending": pending.iter().map(|release| release_json(release)).collect::<Vec<_>>(),
            "repo_path": repo.as_ref().ok().map(|path| path.display().to_string()),
            "repo_path_error": repo.as_ref().err(),
            "agent_command": config.agent_command,
            "review_key": config.review_key,
            "state_file": state_path().display().to_string(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return Ok(0);
    }

    println!("upstream:          {}", crate::product::UPSTREAM_REPO);
    println!(
        "synced base:       {} (built from {})",
        tag_label(&effective),
        tag_label(&compiled)
    );
    println!(
        "watcher:           {} (every {}h{})",
        if config.enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.check_interval_hours,
        if config.include_prereleases {
            ", incl. prereleases"
        } else {
            ""
        }
    );
    match state.last_check_unix {
        Some(last) => println!("last check:        {}", relative_age(now, last)),
        None => println!("last check:        never"),
    }
    println!(
        "last error:        {}",
        state.last_error.as_deref().unwrap_or("none")
    );
    println!(
        "last notified:     {}",
        state.last_notified_tag.as_deref().unwrap_or("none")
    );
    println!(
        "skipped:           {}",
        if state.skipped_tags.is_empty() {
            "none".to_string()
        } else {
            state.skipped_tags.join(", ")
        }
    );
    println!(
        "pending:           {}",
        if pending.is_empty() {
            "none".to_string()
        } else {
            pending
                .iter()
                .map(|release| release.tag.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    match &repo {
        Ok(path) => println!("repo_path:         {}", path.display()),
        Err(error) => println!("repo_path:         unresolved\n{error}"),
    }
    println!("agent_command:     {}", config.agent_command);
    println!(
        "review key:        {}",
        if config.review_key.trim().is_empty() {
            "none"
        } else {
            config.review_key.trim()
        }
    );
    Ok(0)
}

fn upstream_skip(args: &[String]) -> std::io::Result<i32> {
    let [tag] = args else {
        eprintln!("usage: flockr upstream skip TAG");
        return Ok(2);
    };
    if let Err(err) = validate_tag(tag) {
        eprintln!("{err}");
        return Ok(2);
    }
    let mut state = UpstreamState::load();
    state.skip(tag);
    state.save()?;
    println!("skipped upstream release {tag}");
    Ok(0)
}

/// Opens a new flockr tab in the source checkout that runs
/// `flockr upstream sync <tag>`. Used by the review popup and `copy`.
fn open_sync_tab(config: &crate::config::UpstreamConfig, tag: &str) -> Result<String, String> {
    validate_tag(tag)?;
    let repo = resolve_repo_path(config.repo_path.as_deref())?;
    preflight_repo(&repo)?;

    let exe = std::env::var("FLOCKR_BIN_PATH")
        .ok()
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .ok_or("cannot determine the flockr executable path")?;
    let command = format!("{} upstream sync {tag}", shell_quote_path(&exe));

    let created = super::send_request(&Request {
        id: "cli:upstream:copy:tab".into(),
        method: Method::TabCreate(TabCreateParams {
            workspace_id: None,
            cwd: Some(repo.display().to_string()),
            focus: true,
            label: Some(format!("upstream-sync {tag}")),
            env: Default::default(),
        }),
    })
    .map_err(|e| format!("could not reach the flockr server to open a tab: {e}"))?;
    if created.get("error").is_some() {
        return Err(format!("could not open a flockr tab: {created}"));
    }
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .ok_or("the new tab did not report a pane id")?
        .to_string();

    let sent = super::send_request(&Request {
        id: "cli:upstream:copy:run".into(),
        method: Method::PaneSendInput(PaneSendInputParams {
            pane_id,
            text: command.clone(),
            keys: vec!["Enter".into()],
        }),
    })
    .map_err(|e| format!("opened the tab but could not start the sync: {e}"))?;
    if sent.get("error").is_some() {
        return Err(format!(
            "opened the tab but could not start the sync: {sent}"
        ));
    }
    Ok(command)
}

fn upstream_copy(args: &[String]) -> std::io::Result<i32> {
    let config = upstream_config();
    let tag = match args {
        [tag] => tag.clone(),
        [] => match UpstreamState::load()
            .pending
            .iter()
            .filter(|release| !release.prerelease || config.include_prereleases)
            .filter_map(|release| Some((parse_tag_version(&release.tag)?, release.tag.clone())))
            .max()
        {
            Some((_, tag)) => tag,
            None => {
                eprintln!("no pending upstream release; run `flockr upstream check` first");
                return Ok(1);
            }
        },
        _ => {
            eprintln!("usage: flockr upstream copy [TAG]");
            return Ok(2);
        }
    };
    match open_sync_tab(&config, &tag) {
        Ok(command) => {
            println!("opened a flockr tab running: {command}");
            Ok(0)
        }
        Err(err) => {
            eprintln!("copy updates failed: {err}");
            Ok(1)
        }
    }
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.parse().ok())
        .unwrap_or(100)
}

fn upstream_review(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: flockr upstream review");
        return Ok(2);
    }
    let config = upstream_config();
    // Use the cache; only hit the network when nothing was ever fetched.
    let state = UpstreamState::load();
    let outcome = run_check(
        &state_path(),
        &check_params(&config, state.last_check_unix.is_none(), false),
        &fetch_releases_curl,
    );
    let base = tag_label(&outcome.base);
    if let Some(error) = &outcome.error {
        println!("(upstream check failed: {error}; showing cached results)");
    }
    let pending = outcome.pending;

    println!("{}", render_summary(&base, &pending, terminal_width()));
    let Some(latest) = pending.last().map(|release| release.tag.clone()) else {
        pause_before_close();
        return Ok(0);
    };

    let stdin = std::io::stdin();
    loop {
        println!("{PROMPT_LINE}");
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(0);
        }
        match parse_choice(&line) {
            ReviewChoice::CopyUpdates => {
                return match open_sync_tab(&config, &latest) {
                    Ok(command) => {
                        println!("Opened a flockr tab running: {command}");
                        println!(
                            "The sync works in its own worktree and never touches your current checkout."
                        );
                        pause_before_close();
                        Ok(0)
                    }
                    Err(err) => {
                        println!("Could not copy updates: {err}");
                        pause_before_close();
                        Ok(1)
                    }
                };
            }
            ReviewChoice::SkipVersion => {
                let mut state = UpstreamState::load();
                for release in &pending {
                    state.skip(&release.tag);
                }
                state.save()?;
                println!("Skipped {}.", latest);
                pause_before_close();
                return Ok(0);
            }
            ReviewChoice::Later => return Ok(0),
            ReviewChoice::ViewFull => println!("{}", render_full(&pending)),
            ReviewChoice::Unknown => println!("Unrecognized choice."),
        }
    }
}

/// Keeps the popup open long enough to read the result of an action.
fn pause_before_close() {
    println!("Press Enter to close.");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}

fn upstream_sync(args: &[String]) -> std::io::Result<i32> {
    let [tag] = args else {
        eprintln!("usage: flockr upstream sync TAG");
        return Ok(2);
    };
    let config = upstream_config();
    let repo = match resolve_repo_path(config.repo_path.as_deref()) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("upstream sync failed: {err}");
            return Ok(1);
        }
    };
    let state = UpstreamState::load();
    let options = SyncOptions {
        repo: &repo,
        tag,
        remote_url: crate::product::UPSTREAM_REMOTE_URL,
        agent_command: &config.agent_command,
        state: &state,
    };
    crate::platform::begin_cli_output();
    let mut stdout = std::io::stdout();
    match run_sync(
        &options,
        &mut |cwd, command| exec_agent(cwd, command),
        &mut stdout,
    ) {
        Ok(code) => Ok(code),
        Err(err) => {
            eprintln!("upstream sync failed: {err}");
            Ok(1)
        }
    }
}

fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn cargo_build_release(repo: &Path) -> std::io::Result<bool> {
    Ok(std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(repo)
        .status()?
        .success())
}

fn upstream_apply(args: &[String]) -> std::io::Result<i32> {
    let mut yes = false;
    let mut tag = None;
    for arg in args {
        match arg.as_str() {
            "--yes" | "-y" => yes = true,
            other if other.starts_with('-') => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
            other if tag.is_none() => tag = Some(other.to_string()),
            _ => {
                eprintln!("usage: flockr upstream apply TAG [--yes]");
                return Ok(2);
            }
        }
    }
    let Some(tag) = tag else {
        eprintln!("usage: flockr upstream apply TAG [--yes]");
        return Ok(2);
    };

    let config = upstream_config();
    let repo = match resolve_repo_path(config.repo_path.as_deref()) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("upstream apply failed: {err}");
            return Ok(1);
        }
    };
    let plan = match plan_apply(&repo, &tag) {
        Ok(plan) => plan,
        Err(err) => {
            eprintln!("upstream apply refused: {err}");
            return Ok(1);
        }
    };
    for line in describe_plan(&plan) {
        println!("{line}");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("refusing to apply without confirmation; re-run with --yes");
            return Ok(1);
        }
        if !confirm("Apply now?") {
            println!("aborted; nothing was changed");
            return Ok(1);
        }
    }

    crate::platform::begin_cli_output();
    let mut stdout = std::io::stdout();
    match execute_apply(&plan, &state_path(), &mut cargo_build_release, &mut stdout) {
        Ok(()) => {
            println!(
                "done: {} is applied. Restart the flockr server to run the new build (`flockr server stop`, then `flockr`).",
                plan.tag
            );
            Ok(0)
        }
        Err(err) => {
            eprintln!("upstream apply failed: {err}");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_age_picks_a_readable_unit() {
        assert_eq!(relative_age(100, 70), "30s ago");
        assert_eq!(relative_age(10_000, 10_000 - 600), "10m ago");
        assert_eq!(relative_age(100_000, 100_000 - 7_200), "2h ago");
        assert_eq!(relative_age(1_000_000, 1_000_000 - 3 * 86_400), "3d ago");
        assert_eq!(relative_age(5, 10), "0s ago");
    }

    #[test]
    fn json_flag_parsing() {
        assert_eq!(parse_json_flag(&[], "u"), Ok(false));
        assert_eq!(parse_json_flag(&["--json".into()], "u"), Ok(true));
        assert_eq!(parse_json_flag(&["--nope".into()], "u"), Err(2));
        assert_eq!(parse_json_flag(&["--json".into(), "x".into()], "u"), Err(2));
    }

    #[test]
    fn release_json_carries_buckets_and_raw_body() {
        let release = UpstreamRelease {
            tag: "v0.9.7".into(),
            name: "v0.9.7".into(),
            body: "### Added\n- a\n### Fixed\n- b".into(),
            prerelease: false,
            published_at: Some("2026-10-20T00:00:00Z".into()),
            url: "https://example.invalid".into(),
        };
        let value = release_json(&release);
        assert_eq!(value["tag"], "v0.9.7");
        assert_eq!(value["features"], json!(["a"]));
        assert_eq!(value["fixes"], json!(["b"]));
        assert_eq!(value["other"], json!([]));
        assert!(value["body"].as_str().unwrap().contains("### Added"));
    }

    #[test]
    fn usage_errors_exit_two_without_side_effects() {
        for args in [
            vec![],
            vec!["bogus".to_string()],
            vec!["check".to_string(), "--nope".to_string()],
            vec!["skip".to_string()],
            vec!["sync".to_string()],
            vec!["apply".to_string()],
            vec![
                "apply".to_string(),
                "v1.0.0".to_string(),
                "--bogus".to_string(),
            ],
            vec!["review".to_string(), "extra".to_string()],
        ] {
            assert_eq!(run_upstream_command(&args).unwrap(), 2, "{args:?}");
        }
        assert_eq!(run_upstream_command(&["--help".to_string()]).unwrap(), 0);
    }
}
