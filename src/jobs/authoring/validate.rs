//! `flockr job validate`: everything that can be checked about a job without
//! waiting for its schedule: the file's own diagnostics, whether the command
//! and the files it names exist, whether the handoff templates only use known
//! variables, and (with `--run-feed`) whether the feed command prints a valid
//! feed. The checks use the same parsers and environment rules as the server.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::program::find_program;
use super::{local_time_text, path_of, run_in_job_env, Scratch};
use crate::api::schema::JobRunStatus;
use crate::feed::format::{parse_feed, MAX_FEED_BYTES};
use crate::jobs::cron::SystemLocal;
use crate::jobs::definition::{parse_job_toml, JobDef, Schedule};
use crate::jobs::env::{flockr_job_vars, resolve_job_env, LoginEnvCache};
use crate::jobs::handoff::{placeholders, CONTEXT_VARIABLE_PREFIX, TEMPLATE_VARIABLES};

/// Programs whose first non-flag argument is a script file.
const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "python",
    "python3",
    "node",
    "deno",
    "ruby",
    "perl",
    "rust-script",
    "php",
    "lua",
];

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct FeedReport {
    /// The feed command was executed (`--run-feed`).
    pub(crate) ran: bool,
    pub(crate) title: Option<String>,
    pub(crate) items: Option<usize>,
    pub(crate) unread: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct JobReport {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) ok: bool,
    pub(crate) errors: Vec<String>,
    pub(crate) warnings: Vec<String>,
    /// Facts that need no action, such as "disabled".
    pub(crate) notes: Vec<String>,
    pub(crate) title: Option<String>,
    pub(crate) enabled: Option<bool>,
    pub(crate) schedule: Option<String>,
    /// The next runs in local time, `YYYY-MM-DD HH:MM`.
    pub(crate) next_runs: Vec<String>,
    /// Where the environment comes from; never any values.
    pub(crate) env: Option<String>,
    /// The program the command would start.
    pub(crate) command: Option<String>,
    pub(crate) feed: Option<FeedReport>,
}

pub(crate) struct Context<'a> {
    pub(crate) cache: &'a LoginEnvCache,
    pub(crate) now_secs: i64,
    /// Execute each feed command and validate its output.
    pub(crate) run_feed: bool,
}

/// Where a validation target lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: String,
    pub(crate) dir: PathBuf,
}

fn dir_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .as_deref()
        .unwrap_or(dir)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A job id or a path to a job directory or its `job.toml`.
pub(crate) fn resolve_target(arg: &str, jobs_dir: &Path) -> Result<Target, String> {
    if crate::jobs::definition::valid_job_id(arg) {
        let dir = jobs_dir.join(arg);
        if dir.join("job.toml").is_file() {
            return Ok(Target {
                id: arg.to_owned(),
                dir,
            });
        }
    }
    let path = crate::jobs::definition::expand_tilde(arg);
    let dir = if path.is_dir() {
        path
    } else if path.is_file() {
        match path.parent() {
            Some(parent) if parent.as_os_str().is_empty() => PathBuf::from("."),
            Some(parent) => parent.to_path_buf(),
            None => PathBuf::from("."),
        }
    } else {
        return Err(format!(
            "no job '{arg}': there is no {}/{arg}/job.toml and no such path",
            jobs_dir.display()
        ));
    };
    if !dir.join("job.toml").is_file() {
        return Err(format!("{} has no job.toml", dir.display()));
    }
    Ok(Target {
        id: dir_name(&dir),
        dir,
    })
}

/// Every directory under `jobs_dir` that holds a `job.toml`, by name.
pub(crate) fn all_targets(jobs_dir: &Path) -> Vec<Target> {
    let mut targets: Vec<Target> = std::fs::read_dir(jobs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("job.toml").is_file())
        .map(|entry| Target {
            id: entry.file_name().to_string_lossy().into_owned(),
            dir: entry.path(),
        })
        .collect();
    targets.sort_by(|a, b| a.id.cmp(&b.id));
    targets
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

fn basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
}

fn install_hint(program: &str) -> &'static str {
    match basename(program) {
        "rust-script" => " Install it with `cargo install rust-script`.",
        "python3" | "python" => " Install Python 3, or change the command.",
        "workbox" => " Install workbox, or point the job at a stub while you try it.",
        _ => "",
    }
}

/// Check that `command[0]` resolves and that an interpreter's script exists.
fn check_command(
    label: &str,
    command: &[String],
    job: &JobDef,
    path_var: Option<&str>,
    env_note: &str,
    report: &mut JobReport,
) -> Option<PathBuf> {
    let program = command.first()?;
    let found = find_program(program, &job.cwd, path_var);
    match &found {
        Some(_) => {}
        None if program.contains('/') => report.errors.push(format!(
            "{label}: no executable file at {} (relative paths start in {}); check the name and `chmod +x`",
            crate::plugin_command::program_for_cwd(program, &job.cwd).display(),
            job.cwd.display()
        )),
        None => report.errors.push(format!(
            "{label}: '{program}' was not found on the job's PATH ({env_note}).{}",
            install_hint(program)
        )),
    }
    if INTERPRETERS.contains(&basename(program))
        && !command.iter().any(|arg| arg == "-c" || arg == "-e")
    {
        if let Some(script) = command[1..].iter().find(|arg| !arg.starts_with('-')) {
            let path = crate::jobs::definition::resolve_path(&job.cwd, script);
            if !path.is_file() {
                report.errors.push(format!(
                    "{label}: the script {script} does not exist at {}",
                    path.display()
                ));
            }
        }
    }
    found
}

fn check_template(
    field: &str,
    text: &str,
    has_feed: bool,
    report: &mut JobReport,
    warned_context: &mut bool,
) {
    for name in placeholders(text) {
        if name.starts_with(CONTEXT_VARIABLE_PREFIX) {
            if !*warned_context {
                *warned_context = true;
                report.warnings.push(format!(
                    "{field}: {{{{{name}}}}} only has a value when the job prints a flockr_handoff line with a context object; otherwise it renders empty"
                ));
            }
        } else if !TEMPLATE_VARIABLES.contains(&name.as_str()) {
            report.errors.push(format!(
                "{field}: unknown template variable {{{{{name}}}}}; use {} or {{{{context.<path>}}}}",
                TEMPLATE_VARIABLES
                    .iter()
                    .map(|known| format!("{{{{{known}}}}}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        } else if !has_feed && (name == "feed_json" || name == "count") {
            report.warnings.push(format!(
                "{field}: {{{{{name}}}}} reads the job's feed, but the job has no [feed]; it renders as {}",
                if name == "count" { "0" } else { "an empty list" }
            ));
        }
    }
}

fn check_handoff(job: &JobDef, path_var: Option<&str>, report: &mut JobReport) {
    let Some(handoff) = &job.handoff else {
        return;
    };
    let has_feed = job.feed.is_some();
    let mut warned_context = false;
    let fields = [
        ("handoff.ask", handoff.ask.as_deref()),
        ("handoff.tab_title", handoff.tab_title.as_deref()),
        ("handoff.cwd", handoff.cwd.as_deref()),
        ("handoff.agent_command", handoff.agent_command.as_deref()),
        ("handoff.prompt", handoff.prompt.as_deref()),
    ];
    for (field, text) in fields {
        if let Some(text) = text {
            check_template(field, text, has_feed, report, &mut warned_context);
        }
    }
    let mut has_prompt = handoff
        .prompt
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    if let Some(file) = &handoff.prompt_file {
        let path = crate::jobs::definition::resolve_path(&job.dir, file);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                has_prompt |= !content.trim().is_empty();
                check_template(
                    &format!("handoff.prompt_file ({file})"),
                    &content,
                    has_feed,
                    report,
                    &mut warned_context,
                );
            }
            Err(err) => report.errors.push(format!(
                "handoff.prompt_file: could not read {}: {err}",
                path.display()
            )),
        }
    }
    let agent = handoff
        .agent_command
        .as_deref()
        .unwrap_or(crate::jobs::handoff::DEFAULT_AGENT_COMMAND);
    if agent.contains("{prompt_file}") && !has_prompt {
        report.errors.push(
            "handoff.agent_command uses {prompt_file} but the handoff has no prompt; set prompt or prompt_file"
                .into(),
        );
    }
    if let Some(program) = agent.split_whitespace().next() {
        if !program.contains("{{")
            && !program.contains('=')
            && find_program(program, &job.cwd, path_var).is_none()
        {
            report.warnings.push(format!(
                "handoff.agent_command: '{program}' was not found on the job's PATH; the new tab starts it with your shell's PATH, so this may be fine"
            ));
        }
    }
    if let Some(cwd) = handoff.cwd.as_deref().filter(|cwd| !cwd.contains("{{")) {
        let path = crate::jobs::definition::resolve_path(&job.dir, cwd);
        if !path.is_dir() {
            report.warnings.push(format!(
                "handoff.cwd: {} does not exist; it is checked again when you start the decision",
                path.display()
            ));
        }
    }
}

fn describe_schedule(job: &JobDef, now_secs: i64, report: &mut JobReport) {
    report.schedule = Some(job.schedule.describe());
    let mut at = now_secs;
    for _ in 0..3 {
        match job.schedule.next_after(at, &SystemLocal) {
            Some(next) => {
                report.next_runs.push(local_time_text(next));
                at = next;
            }
            None => {
                if report.next_runs.is_empty() {
                    report
                        .errors
                        .push("schedule: this cron expression never fires".into());
                }
                break;
            }
        }
    }
    if let Schedule::Every(interval) = &job.schedule {
        if job.timeout >= *interval {
            report.warnings.push(format!(
                "timeout: {} is not shorter than the interval ({}); a slow run makes the next one \"skipped: still running\"",
                crate::jobs::definition::format_duration(job.timeout),
                crate::jobs::definition::format_duration(*interval)
            ));
        }
    }
}

fn check_feed_output(job: &JobDef, ctx: &Context<'_>, report: &mut JobReport) {
    let Some(feed) = &job.feed else {
        return;
    };
    let mut feed_report = FeedReport::default();
    if ctx.run_feed {
        feed_report.ran = true;
        match Scratch::new() {
            Err(err) => report
                .errors
                .push(format!("feed: could not create a scratch directory: {err}")),
            Ok(scratch) => match run_in_job_env(
                job,
                ctx.cache,
                &scratch,
                feed.command.clone(),
                MAX_FEED_BYTES + 1,
            ) {
                Err(err) => report.errors.push(format!("feed: {err}")),
                Ok((result, _)) if result.status != JobRunStatus::Success => {
                    report.errors.push(format!(
                        "feed.command failed: {}",
                        failure_line(&result.summary, &result.stderr)
                    ));
                }
                Ok((result, _)) if result.stdout_truncated => report.errors.push(format!(
                    "feed.command printed more than {} KiB",
                    MAX_FEED_BYTES / 1024
                )),
                Ok((result, _)) => match parse_feed(&result.stdout) {
                    Err(errors) => report.errors.extend(
                        errors
                            .into_iter()
                            .map(|error| format!("feed output: {error}")),
                    ),
                    Ok(parsed) => {
                        feed_report.title = parsed.title.clone();
                        feed_report.items = Some(parsed.items.len());
                        feed_report.unread = Some(
                            parsed
                                .items
                                .iter()
                                .filter(|item| {
                                    item.state == crate::api::schema::FeedItemState::Unread
                                })
                                .count(),
                        );
                        report.warnings.extend(
                            parsed
                                .diagnostics
                                .into_iter()
                                .map(|line| format!("feed output: {line}")),
                        );
                        if !result.stderr.trim().is_empty() {
                            report.notes.push(
                                "feed.command wrote to stderr; stderr is ignored, only stdout is the feed"
                                    .into(),
                            );
                        }
                    }
                },
            },
        }
    } else {
        report.notes.push(
            "the feed command was not run; add --run-feed to execute it and check its output"
                .into(),
        );
    }
    report.feed = Some(feed_report);
}

/// Validate the job in `target.dir`.
pub(crate) fn validate_job(target: &Target, ctx: &Context<'_>) -> JobReport {
    let toml_path = target.dir.join("job.toml");
    let mut report = JobReport {
        id: target.id.clone(),
        path: toml_path.display().to_string(),
        ..JobReport::default()
    };
    let content = match std::fs::read_to_string(&toml_path) {
        Ok(content) => content,
        Err(err) => {
            report
                .errors
                .push(format!("could not read {}: {err}", toml_path.display()));
            return finish(report);
        }
    };
    let job = match parse_job_toml(&content, &target.id, &target.dir) {
        Ok(job) => job,
        Err(diagnostics) => {
            report.errors.extend(diagnostics);
            return finish(report);
        }
    };
    report.title = Some(job.title.clone());
    report.enabled = Some(job.enabled);
    if !job.enabled {
        report.notes.push(format!(
            "disabled: it will not run on its schedule until `flockr job enable {}`",
            job.id
        ));
    }
    describe_schedule(&job, ctx.now_secs, &mut report);

    if !job.cwd.is_dir() {
        report
            .errors
            .push(format!("cwd: {} does not exist", job.cwd.display()));
    }
    let scratch_vars = flockr_job_vars(&job, &target.dir);
    let (path_var, env_note) = match resolve_job_env(&job, ctx.cache, scratch_vars) {
        Ok(env) => {
            let path = path_of(&env.vars, !env.clear);
            (path, env.note)
        }
        Err(err) => {
            report.errors.push(err);
            (path_of(&[], true), "environment unresolved".to_owned())
        }
    };
    report.env = Some(env_note.clone());
    report.command = check_command(
        "command",
        &job.command,
        &job,
        path_var.as_deref(),
        &env_note,
        &mut report,
    )
    .map(|path| path.display().to_string());
    if let Some(feed) = &job.feed {
        check_command(
            "feed.command",
            &feed.command,
            &job,
            path_var.as_deref(),
            &env_note,
            &mut report,
        );
    }
    check_handoff(&job, path_var.as_deref(), &mut report);
    check_feed_output(&job, ctx, &mut report);
    finish(report)
}

fn finish(mut report: JobReport) -> JobReport {
    report.ok = report.errors.is_empty();
    report
}

/// The human text for one report.
pub(crate) fn render_report(report: &JobReport) -> String {
    let mut out = format!("{} ({})\n", report.id, report.path);
    let mut field = |name: &str, value: &str| out.push_str(&format!("  {name:<9} {value}\n"));
    if let Some(title) = &report.title {
        field("title", title);
    }
    if let Some(schedule) = &report.schedule {
        field("schedule", schedule);
    }
    if !report.next_runs.is_empty() {
        field("next", &report.next_runs.join(", "));
    }
    if let Some(env) = &report.env {
        field("env", env);
    }
    if let Some(command) = &report.command {
        field("command", command);
    }
    if let Some(feed) = &report.feed {
        if feed.ran {
            let summary = match (feed.items, feed.unread) {
                (Some(items), Some(unread)) => format!(
                    "ran: {items} items, {unread} unread{}",
                    feed.title
                        .as_deref()
                        .map(|title| format!(", title \"{title}\""))
                        .unwrap_or_default()
                ),
                _ => "ran: no usable output".to_owned(),
            };
            field("feed", &summary);
        }
    }
    for note in &report.notes {
        out.push_str(&format!("  note: {note}\n"));
    }
    for warning in &report.warnings {
        out.push_str(&format!("  warning: {warning}\n"));
    }
    for error in &report.errors {
        out.push_str(&format!("  error: {error}\n"));
    }
    out.push_str(&format!(
        "  result: {}\n",
        if report.errors.is_empty() {
            if report.warnings.is_empty() {
                "ok".to_owned()
            } else {
                format!("ok, {} warning(s)", report.warnings.len())
            }
        } else {
            format!(
                "{} error(s), {} warning(s)",
                report.errors.len(),
                report.warnings.len()
            )
        }
    ));
    out
}
