//! `flockr job run <id> --dry-run`: run a job once in the foreground with its
//! real environment rules, timeout and working directory, and show what the
//! server would have made of it, without recording a run, touching the feed
//! store, notifying anyone or creating a decision. `FLOCKR_JOB_STATE_DIR`
//! points at a scratch directory that is removed afterwards, so a job that
//! keeps files there does not leave any behind either.

use std::time::Duration;

use super::{run_in_job_env, Scratch};
use crate::api::schema::{
    FeedActionInfo, FeedField, FeedItemInfo, FeedItemState, JobRunInfo, JobRunStatus, JobRunTrigger,
};
use crate::feed::format::{parse_feed, ItemRecord, ParsedFeed, MAX_FEED_BYTES};
use crate::jobs::definition::JobDef;
use crate::jobs::display;
use crate::jobs::env::LoginEnvCache;
use crate::jobs::feed_work;
use crate::jobs::handoff::{self, EvalInput, EvalResult, FeedSnapshot};
use crate::jobs::runner::ExecResult;
use crate::jobs::state::{find_handoff_line, RunRecord, OUTPUT_TAIL_BYTES};

/// Characters of the rendered prompt printed.
const PROMPT_PREVIEW_CHARS: usize = 6_000;
/// Characters of each output stream printed.
const OUTPUT_PREVIEW_CHARS: usize = 8_000;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Options {
    /// Do not run anything: render the handoff for a successful run with empty
    /// stdout.
    pub(crate) no_exec: bool,
}

#[derive(Debug)]
pub(crate) enum FeedOutcome {
    NotConfigured,
    NotRun(String),
    Failed(String),
    Parsed(ParsedFeed),
}

#[derive(Debug)]
pub(crate) struct DryRun {
    pub(crate) options: Options,
    pub(crate) env_note: Option<String>,
    pub(crate) record: RunRecord,
    pub(crate) feed: FeedOutcome,
    pub(crate) feed_snapshot: Option<FeedSnapshot>,
    pub(crate) evaluation: Option<Result<EvalResult, String>>,
}

fn item_info(item: &ItemRecord) -> FeedItemInfo {
    FeedItemInfo {
        id: item.id.clone(),
        title: item.title.clone(),
        subtitle: item.subtitle.clone(),
        state: item.state,
        badge: item.badge.clone(),
        url: item.url.clone(),
        updated_at: item.updated_at.clone(),
        body: item.body.clone(),
        fields: item
            .fields
            .iter()
            .map(|field| FeedField {
                label: field.label.clone(),
                value: field.value.clone(),
            })
            .collect(),
        group: item.group.clone(),
        also_groups: item.also_groups.clone(),
        actions: item
            .actions
            .iter()
            .map(|action| FeedActionInfo {
                key: action.key.clone(),
                title: action.title.clone(),
                confirm: action.confirm.clone(),
            })
            .collect(),
    }
}

/// The feed as a handoff sees it, built from a parsed feed the way the server
/// builds it from its store.
pub(crate) fn snapshot(job: &JobDef, parsed: &ParsedFeed) -> FeedSnapshot {
    let title = job
        .feed
        .as_ref()
        .and_then(|feed| feed.title.clone())
        .or_else(|| parsed.title.clone())
        .unwrap_or_else(|| job.title.clone());
    let items: Vec<FeedItemInfo> = parsed.items.iter().map(item_info).collect();
    let json = handoff::feed_json(&title, &items);
    FeedSnapshot {
        unread: u32::try_from(
            items
                .iter()
                .filter(|item| item.state == FeedItemState::Unread)
                .count(),
        )
        .unwrap_or(u32::MAX),
        fingerprint: handoff::feed_fingerprint(&json),
        json,
    }
}

fn failure_line(summary: &str, stderr: &str) -> String {
    match stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        Some(line) => format!("{summary}: {}", line.chars().take(160).collect::<String>()),
        None => summary.to_owned(),
    }
}

fn record_from(result: ExecResult, started_ms: u64, env_note: Option<String>) -> RunRecord {
    let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);
    let handoff = find_handoff_line(&result.stdout);
    RunRecord {
        info: JobRunInfo {
            run_id: 0,
            trigger: JobRunTrigger::Manual,
            status: result.status,
            started_unix_ms: started_ms,
            finished_unix_ms: started_ms.saturating_add(duration_ms),
            duration_ms,
            exit_code: result.exit_code,
            summary: result.summary,
            error: result.error,
            skipped_count: 0,
            env_note,
        },
        stdout_tail: result.stdout,
        stderr_tail: result.stderr,
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
        handoff,
    }
}

/// Run the job and evaluate its handoff. `Err` means the environment could
/// not be resolved at all (for example an unreadable `env_file`).
pub(crate) fn run(
    job: &JobDef,
    cache: &LoginEnvCache,
    options: Options,
    now_ms: u64,
) -> Result<DryRun, String> {
    let scratch =
        Scratch::new().map_err(|err| format!("could not create a scratch directory: {err}"))?;
    let date = handoff::local_date(now_ms);

    let (record, env_note) = if options.no_exec {
        let pretend = ExecResult {
            status: JobRunStatus::Success,
            exit_code: Some(0),
            summary: "exit 0 (not run: --no-exec)".into(),
            error: None,
            stdout: String::new(),
            stdout_truncated: false,
            stderr: String::new(),
            stderr_truncated: false,
            duration: Duration::ZERO,
        };
        (record_from(pretend, now_ms, None), None)
    } else {
        let (result, note) =
            run_in_job_env(job, cache, &scratch, job.command.clone(), OUTPUT_TAIL_BYTES)?;
        (record_from(result, now_ms, Some(note.clone())), Some(note))
    };

    let (feed, feed_snapshot) = match &job.feed {
        None => (FeedOutcome::NotConfigured, None),
        Some(_) if options.no_exec => (
            FeedOutcome::NotRun(
                "--no-exec skips the feed; {{feed_json}} is empty and {{count}} is 0".into(),
            ),
            None,
        ),
        Some(_) if record.info.status != JobRunStatus::Success => (
            FeedOutcome::NotRun(
                "the job did not succeed, and a feed only refreshes after a successful run".into(),
            ),
            None,
        ),
        Some(feed) => {
            match run_in_job_env(
                job,
                cache,
                &scratch,
                feed.command.clone(),
                MAX_FEED_BYTES + 1,
            ) {
                Err(err) => (FeedOutcome::Failed(err), None),
                Ok((result, _)) if result.status != JobRunStatus::Success => (
                    FeedOutcome::Failed(format!(
                        "feed command failed: {}",
                        failure_line(&result.summary, &result.stderr)
                    )),
                    None,
                ),
                Ok((result, _)) if result.stdout_truncated => (
                    FeedOutcome::Failed(format!(
                        "feed output is larger than {} KiB",
                        MAX_FEED_BYTES / 1024
                    )),
                    None,
                ),
                Ok((result, _)) => match parse_feed(&result.stdout) {
                    Err(errors) => (FeedOutcome::Failed(errors.join("; ")), None),
                    Ok(parsed) => {
                        let snap = snapshot(job, &parsed);
                        (FeedOutcome::Parsed(parsed), Some(snap))
                    }
                },
            }
        }
    };

    let evaluation = (job.handoff.is_some() || record.handoff.is_some()).then(|| {
        handoff::evaluate(&EvalInput {
            job,
            record: &record,
            feed: feed_snapshot.as_ref(),
            previous_fingerprint: None,
            date: &date,
        })
    });
    Ok(DryRun {
        options,
        env_note,
        record,
        feed,
        feed_snapshot,
        evaluation,
    })
}

/// Zero when the command succeeded, the feed (if any) was usable and the
/// handoff (if any) rendered; one otherwise.
pub(crate) fn exit_code(dry: &DryRun) -> i32 {
    let command_ok = dry.record.info.status == JobRunStatus::Success;
    let feed_ok = !matches!(dry.feed, FeedOutcome::Failed(_));
    let handoff_ok = !matches!(dry.evaluation, Some(Err(_)));
    i32::from(!(command_ok && feed_ok && handoff_ok))
}

fn clip_block(text: &str, max: usize) -> String {
    let trimmed = text.trim_end();
    let count = trimmed.chars().count();
    if count <= max {
        return trimmed.to_owned();
    }
    let kept: String = trimmed.chars().take(max).collect();
    format!("{kept}\n... ({} more characters)", count - max)
}

fn indented(text: &str) -> String {
    if text.trim().is_empty() {
        return "    (empty)\n".to_owned();
    }
    text.lines().map(|line| format!("    {line}\n")).collect()
}

/// The text `flockr job run --dry-run` prints.
pub(crate) fn render(job: &JobDef, dry: &DryRun) -> String {
    let info = &dry.record.info;
    let mut out = String::new();
    out.push_str(&format!(
        "dry run of {} ({}): nothing is recorded, notified or decided\n",
        job.id, job.title
    ));
    out.push_str(&format!("command   {}\n", job.command.join(" ")));
    out.push_str(&format!("cwd       {}\n", job.cwd.display()));
    if let Some(note) = &dry.env_note {
        out.push_str(&format!("env       {note}\n"));
    }
    if dry.options.no_exec {
        out.push_str("\n--no-exec: the command was not run. The handoff below assumes a\nsuccessful run with empty stdout.\n");
    } else {
        out.push_str(&format!(
            "\nresult    {} in {}{}\n",
            display::status_word(info.status),
            display::run_duration(info.duration_ms),
            info.exit_code
                .map(|code| format!(" (exit {code})"))
                .unwrap_or_default()
        ));
        if info.status != JobRunStatus::Success {
            out.push_str(&format!("          {}\n", info.summary));
        }
        out.push_str("stdout\n");
        out.push_str(&indented(&clip_block(
            &dry.record.stdout_tail,
            OUTPUT_PREVIEW_CHARS,
        )));
        out.push_str("stderr\n");
        out.push_str(&indented(&clip_block(
            &dry.record.stderr_tail,
            OUTPUT_PREVIEW_CHARS,
        )));
    }

    out.push_str("\nfeed\n");
    match &dry.feed {
        FeedOutcome::NotConfigured => out.push_str("    the job has no [feed]\n"),
        FeedOutcome::NotRun(why) => out.push_str(&format!("    not run: {why}\n")),
        FeedOutcome::Failed(error) => out.push_str(&format!("    failed: {error}\n")),
        FeedOutcome::Parsed(parsed) => {
            let unread = dry.feed_snapshot.as_ref().map_or(0, |snap| snap.unread);
            out.push_str(&format!(
                "    {} items, {unread} unread{}\n",
                parsed.items.len(),
                parsed
                    .title
                    .as_deref()
                    .map(|title| format!(", title \"{title}\""))
                    .unwrap_or_default()
            ));
            for item in parsed.items.iter().take(5) {
                out.push_str(&format!(
                    "    - {} {}{}\n",
                    item.id,
                    item.title,
                    if item.actions.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " [{}]",
                            item.actions
                                .iter()
                                .map(|action| action.key.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                    }
                ));
            }
            if parsed.items.len() > 5 {
                out.push_str(&format!("    ... and {} more\n", parsed.items.len() - 5));
            }
            for diagnostic in &parsed.diagnostics {
                out.push_str(&format!("    diagnostic: {diagnostic}\n"));
            }
        }
    }

    out.push_str("\nhandoff\n");
    match &dry.evaluation {
        None => out.push_str(
            "    none: the job has no [handoff] table and printed no flockr_handoff line\n",
        ),
        Some(Err(error)) => out.push_str(&format!("    error: {error}\n")),
        Some(Ok(result)) => match &result.offer {
            None => {
                let when = job
                    .handoff
                    .as_ref()
                    .map_or("(no table)", |table| table.when.as_str());
                out.push_str(&format!(
                    "    no offer for this run (when = \"{when}\"; a first run counts as changed)\n"
                ));
            }
            Some(offer) => {
                let source = if dry.record.handoff.is_some() {
                    "from the flockr_handoff line"
                } else {
                    "from [handoff]"
                };
                out.push_str(&format!("    would ask ({source}):\n"));
                out.push_str(&format!("    ask        {}\n", offer.ask));
                out.push_str(&format!("    tab title  {}\n", offer.tab_title));
                out.push_str(&format!("    cwd        {}\n", offer.cwd));
                let prompt_path = handoff::prompt_path(std::path::Path::new("<state dir>"), 0);
                let has_prompt = !offer.prompt.is_empty();
                let launch = handoff::agent_launch(
                    &offer.agent_command,
                    has_prompt.then_some(prompt_path.as_path()),
                    offer.submit,
                    offer.prefill,
                );
                match launch {
                    Ok(launch) => {
                        out.push_str(&format!("    agent      {}\n", launch.command));
                        if has_prompt {
                            out.push_str(&format!(
                                "    {}\n",
                                handoff::submit_note(
                                    &offer.agent_command,
                                    offer.submit,
                                    offer.prefill
                                )
                            ));
                        }
                    }
                    Err(error) => out.push_str(&format!(
                        "    agent      {} (cannot launch: {error})\n",
                        offer.agent_command
                    )),
                }
                for warning in &offer.warnings {
                    out.push_str(&format!("    warning: {warning}\n"));
                }
                if offer.prompt.is_empty() {
                    out.push_str("    prompt     (none)\n");
                } else {
                    out.push_str("    prompt\n");
                    out.push_str(&indented(&clip_block(&offer.prompt, PROMPT_PREVIEW_CHARS)));
                }
            }
        },
    }
    render_work(&mut out, job, dry);
    out
}

/// How many feed items the work preview marks.
const WORK_PREVIEW_ITEMS: usize = 2;

/// The `w` preview: what pressing it on the first feed items would start.
fn render_work(out: &mut String, job: &JobDef, dry: &DryRun) {
    let Some(feed) = &job.feed else {
        return;
    };
    out.push_str("\nwork (w in the Feed overlay)\n");
    let FeedOutcome::Parsed(parsed) = &dry.feed else {
        out.push_str("    not rendered: the feed was not read in this run\n");
        return;
    };
    let items: Vec<FeedItemInfo> = parsed
        .items
        .iter()
        .take(WORK_PREVIEW_ITEMS)
        .map(item_info)
        .collect();
    if items.is_empty() {
        out.push_str("    not rendered: the feed has no items\n");
        return;
    }
    let title = feed
        .title
        .clone()
        .or_else(|| parsed.title.clone())
        .unwrap_or_else(|| job.title.clone());
    let date = handoff::local_date(crate::jobs::now_unix_ms());
    let source = if feed.work.is_some() {
        "from [feed.work]"
    } else {
        "built-in defaults; no [feed.work]"
    };
    match feed_work::plan(&feed_work::WorkInput {
        job,
        feed_title: &title,
        items: &items,
        date: &date,
    }) {
        Err(error) => out.push_str(&format!("    error: {error}\n")),
        Ok(plan) => {
            out.push_str(&format!(
                "    marking the first {} item{} ({source}):\n",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            ));
            out.push_str(&format!("    tab title  {}\n", plan.tab_title));
            let cwd = plan
                .cwd
                .clone()
                .unwrap_or_else(|| feed_work::default_cwd(job));
            out.push_str(&format!("    cwd        {}\n", cwd.display()));
            let prompt_path = std::path::Path::new("<state dir>/work/<time>.md");
            match handoff::agent_launch(
                &plan.agent_command,
                Some(prompt_path),
                plan.submit,
                plan.prefill,
            ) {
                Ok(launch) => {
                    out.push_str(&format!("    agent      {}\n", launch.command));
                    out.push_str(&format!(
                        "    {}\n",
                        handoff::submit_note(&plan.agent_command, plan.submit, plan.prefill)
                    ));
                }
                Err(error) => out.push_str(&format!(
                    "    agent      {} (cannot launch: {error})\n",
                    plan.agent_command
                )),
            }
            for warning in &plan.warnings {
                out.push_str(&format!("    warning: {warning}\n"));
            }
            out.push_str("    prompt\n");
            out.push_str(&indented(&clip_block(&plan.prompt, PROMPT_PREVIEW_CHARS)));
        }
    }
}
