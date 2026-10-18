//! Job definitions: `job.toml` files and plugin manifest `[[jobs]]` tables,
//! parsed and validated into `JobDef` with diagnostics that name the file,
//! the key and the fix.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::cron::{CronExpr, UtcOffset};

pub(crate) const JOB_ID_MAX_CHARS: usize = 120;
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const MIN_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub(crate) const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Schedule {
    Cron(CronExpr),
    Every(Duration),
}

impl Schedule {
    /// The next scheduled Unix second strictly after `after`, or `None` for a
    /// cron expression that never fires.
    pub(crate) fn next_after(&self, after_unix_secs: i64, zone: &dyn UtcOffset) -> Option<i64> {
        match self {
            Self::Cron(expr) => expr.next_after(after_unix_secs, zone),
            Self::Every(interval) => {
                Some(after_unix_secs.saturating_add(i64::try_from(interval.as_secs()).ok()?))
            }
        }
    }

    /// Human text: the cron expression or `every 1h`.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Cron(expr) => expr.as_str().to_owned(),
            Self::Every(interval) => format!("every {}", format_duration(*interval)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EnvMode {
    /// The user's login-shell environment (default).
    #[default]
    LoginShell,
    /// The server process environment.
    Server,
}

impl EnvMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LoginShell => "login-shell",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NotifyPolicy {
    Never,
    #[default]
    OnFailure,
    OnChange,
    Always,
}

impl NotifyPolicy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnFailure => "on_failure",
            Self::OnChange => "on_change",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobSource {
    /// `<config>/jobs/<id>/job.toml`
    User,
    /// Built into flockr, defined in code with the id prefix `system.`.
    System,
    /// A `[[jobs]]` table in a plugin manifest.
    Plugin {
        plugin_id: String,
        plugin_root: PathBuf,
    },
}

/// The optional `feed` table. Phase 1 only validates it; the feed runner
/// reads it later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FeedDef {
    pub(crate) command: Vec<String>,
    pub(crate) title: Option<String>,
}

/// The optional `[handoff]` table: after a run the job may ask the user to
/// start an agent tab. Evaluated by `super::handoff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HandoffDef {
    pub(crate) when: String,
    pub(crate) ask: Option<String>,
    pub(crate) agent_command: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) tab_title: Option<String>,
    /// Prompt template file, relative to the job directory.
    pub(crate) prompt_file: Option<String>,
    /// Inline prompt template (alternative to `prompt_file`).
    pub(crate) prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobDef {
    /// Unique across user and plugin jobs. Plugin jobs are `<plugin_id>:<id>`.
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) command: Vec<String>,
    pub(crate) schedule: Schedule,
    pub(crate) timeout: Duration,
    pub(crate) enabled: bool,
    pub(crate) env: EnvMode,
    pub(crate) env_file: Option<PathBuf>,
    pub(crate) env_overrides: BTreeMap<String, String>,
    pub(crate) notify: NotifyPolicy,
    /// Working directory: the job directory unless `cwd` says otherwise.
    pub(crate) cwd: PathBuf,
    /// Directory holding job files (job.toml for user jobs, the plugin root).
    pub(crate) dir: PathBuf,
    pub(crate) feed: Option<FeedDef>,
    pub(crate) handoff: Option<HandoffDef>,
    pub(crate) source: JobSource,
}

/// Why a job file produced no usable job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvalidJob {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) source: JobSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawJob {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    every: Option<String>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    env_file: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    notify: Option<String>,
    #[serde(default)]
    feed: Option<RawFeed>,
    #[serde(default)]
    handoff: Option<RawHandoff>,
    /// The `[env]` table. It shares its key with `env = "..."`, so it is split
    /// out of the document before deserializing (see `split_env_table`).
    #[serde(skip)]
    env_vars: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHandoff {
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    ask: Option<String>,
    #[serde(default)]
    agent_command: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    tab_title: Option<String>,
    #[serde(default)]
    prompt_file: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

fn valid_handoff_when(when: &str) -> bool {
    matches!(when, "on_success" | "on_change" | "on_failure" | "always")
        || when
            .strip_prefix("exit:")
            .is_some_and(|code| code.parse::<i32>().is_ok())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFeed {
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Valid job id: ASCII letters, digits, `.`, `_`, `-`; starts with a letter or
/// digit (so it is never hidden or `..`); at most 120 characters.
pub(crate) fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= JOB_ID_MAX_CHARS
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && id
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Parse `30s`, `5m`, `1h`, `2d` and combinations such as `1h30m`.
pub(crate) fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("is empty; use a number with a unit such as 30s, 5m, 1h or 1d".into());
    }
    let mut total = 0u64;
    let mut digits = String::new();
    let mut seen_unit = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        let factor = match ch {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            _ => {
                return Err(format!(
                "'{text}' has unknown unit '{ch}'; use s, m, h or d (for example 90s, 5m, 1h30m)"
            ))
            }
        };
        if digits.is_empty() {
            return Err(format!("'{text}' needs a number before '{ch}'"));
        }
        let value: u64 = digits
            .parse()
            .map_err(|_| format!("'{text}' has a number that is too large"))?;
        total = total
            .checked_add(value.saturating_mul(factor))
            .ok_or_else(|| format!("'{text}' is too large"))?;
        digits.clear();
        seen_unit = true;
    }
    if !digits.is_empty() || !seen_unit {
        return Err(format!(
            "'{text}' needs a unit; use s, m, h or d (for example 90s, 5m, 1h)"
        ));
    }
    Ok(Duration::from_secs(total))
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs == 0 {
        return match duration.as_millis() {
            0 => "0s".into(),
            millis => format!("{millis}ms"),
        };
    }
    let mut out = String::new();
    let mut rest = secs;
    for (unit, factor) in [('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)] {
        if rest >= factor {
            out.push_str(&format!("{}{unit}", rest / factor));
            rest %= factor;
        }
    }
    out
}

pub(crate) fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

pub(crate) fn resolve_path(base: &Path, path: &str) -> PathBuf {
    let path = expand_tilde(path);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

/// Validate a raw table into a job. `id_hint` is the directory name for user
/// jobs (the file may omit `id`, and must not contradict it). All problems are
/// returned together so one edit fixes them all.
fn build_job(
    raw: RawJob,
    id_hint: Option<&str>,
    dir: &Path,
    source: JobSource,
) -> Result<JobDef, Vec<String>> {
    let mut diagnostics = Vec::new();

    let local_id = match (raw.id.as_deref().map(str::trim), id_hint) {
        (Some(id), Some(hint)) if id != hint => {
            diagnostics.push(format!(
                "id: '{id}' does not match the directory name '{hint}'; rename one so they agree"
            ));
            id.to_owned()
        }
        (Some(id), _) => id.to_owned(),
        (None, Some(hint)) => hint.to_owned(),
        (None, None) => {
            diagnostics.push("id: required".into());
            String::new()
        }
    };
    if !local_id.is_empty() && !valid_job_id(&local_id) {
        diagnostics.push(format!(
            "id: '{local_id}' is invalid; use letters, digits, '.', '_' and '-' (at most {JOB_ID_MAX_CHARS} characters, starting with a letter or digit)"
        ));
    }
    let id = match &source {
        JobSource::User | JobSource::System => local_id.clone(),
        JobSource::Plugin { plugin_id, .. } => format!("{plugin_id}:{local_id}"),
    };

    let title = raw
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| local_id.clone());

    if raw.command.is_empty() {
        diagnostics.push(
            "command: required; give an argv array such as [\"workbox\", \"sync\"] (use [\"sh\", \"-c\", \"...\"] for a shell command)"
                .into(),
        );
    } else if raw.command[0].trim().is_empty() {
        diagnostics.push("command: the program (first element) must not be empty".into());
    }

    let schedule = match (raw.schedule.as_deref(), raw.every.as_deref()) {
        (Some(_), Some(_)) => {
            diagnostics.push("schedule/every: set exactly one of them, not both".into());
            None
        }
        (None, None) => {
            diagnostics.push(
                "schedule/every: set exactly one, a cron `schedule = \"0 10 * * 1-5\"` or an interval `every = \"1h\"`"
                    .into(),
            );
            None
        }
        (Some(text), None) => match CronExpr::parse(text) {
            Ok(expr) => Some(Schedule::Cron(expr)),
            Err(err) => {
                diagnostics.push(format!("schedule: invalid cron \"{text}\": {err}"));
                None
            }
        },
        (None, Some(text)) => match parse_duration(text) {
            Ok(interval) if interval < MIN_INTERVAL => {
                diagnostics.push(format!(
                    "every: '{text}' is shorter than the 1m minimum interval"
                ));
                None
            }
            Ok(interval) => Some(Schedule::Every(interval)),
            Err(err) => {
                diagnostics.push(format!("every: {err}"));
                None
            }
        },
    };

    let timeout = match raw.timeout.as_deref() {
        None => DEFAULT_TIMEOUT,
        Some(text) => match parse_duration(text) {
            Ok(timeout) if timeout < MIN_TIMEOUT => {
                diagnostics.push(format!("timeout: '{text}' is shorter than 1s"));
                DEFAULT_TIMEOUT
            }
            Ok(timeout) if timeout > MAX_TIMEOUT => {
                diagnostics.push(format!("timeout: '{text}' is longer than the 24h maximum"));
                DEFAULT_TIMEOUT
            }
            Ok(timeout) => timeout,
            Err(err) => {
                diagnostics.push(format!("timeout: {err}"));
                DEFAULT_TIMEOUT
            }
        },
    };

    let env = match raw.env.as_deref() {
        None | Some("login-shell") => EnvMode::LoginShell,
        Some("server") => EnvMode::Server,
        Some(other) => {
            diagnostics.push(format!(
                "env: '{other}' is not valid; use \"login-shell\" (default) or \"server\""
            ));
            EnvMode::LoginShell
        }
    };

    let notify = match raw.notify.as_deref() {
        None | Some("on_failure") => NotifyPolicy::OnFailure,
        Some("never") => NotifyPolicy::Never,
        Some("on_change") => NotifyPolicy::OnChange,
        Some("always") => NotifyPolicy::Always,
        Some(other) => {
            diagnostics.push(format!(
                "notify: '{other}' is not valid; use never, on_failure, on_change or always"
            ));
            NotifyPolicy::OnFailure
        }
    };

    for name in raw.env_vars.keys() {
        if !valid_env_name(name) {
            diagnostics.push(format!(
                "env.{name}: not a valid environment variable name (letters, digits and '_', not starting with a digit)"
            ));
        }
    }

    let feed = raw.feed.and_then(|feed| {
        if feed.command.is_empty() || feed.command[0].trim().is_empty() {
            diagnostics.push("feed.command: required; give an argv array".into());
            return None;
        }
        Some(FeedDef {
            command: feed.command,
            title: feed
                .title
                .map(|title| title.trim().to_owned())
                .filter(|title| !title.is_empty()),
        })
    });

    let handoff = raw.handoff.and_then(|handoff| {
        let when = handoff.when.unwrap_or_else(|| "on_change".into());
        if !valid_handoff_when(&when) {
            diagnostics.push(format!(
                "handoff.when: '{when}' is not valid; use on_success, on_change, on_failure, always or exit:<code>"
            ));
            return None;
        }
        if handoff.prompt.is_some() && handoff.prompt_file.is_some() {
            diagnostics.push(
                "handoff.prompt/prompt_file: set at most one, an inline `prompt` or a `prompt_file`"
                    .into(),
            );
            return None;
        }
        if handoff
            .agent_command
            .as_deref()
            .is_some_and(|command| command.trim().is_empty())
        {
            diagnostics.push("handoff.agent_command: must not be empty".into());
            return None;
        }
        Some(HandoffDef {
            when,
            ask: handoff.ask,
            agent_command: handoff.agent_command,
            cwd: handoff.cwd,
            tab_title: handoff.tab_title,
            prompt_file: handoff.prompt_file,
            prompt: handoff.prompt,
        })
    });

    let cwd = raw
        .cwd
        .as_deref()
        .map(|cwd| resolve_path(dir, cwd))
        .unwrap_or_else(|| dir.to_path_buf());
    let env_file = raw.env_file.as_deref().map(|file| resolve_path(dir, file));

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    let Some(schedule) = schedule else {
        return Err(diagnostics);
    };
    Ok(JobDef {
        id,
        title,
        command: raw.command,
        schedule,
        timeout,
        enabled: raw.enabled.unwrap_or(true),
        env,
        env_file,
        env_overrides: raw.env_vars,
        notify,
        cwd,
        dir: dir.to_path_buf(),
        feed,
        handoff,
        source,
    })
}

type EnvParts = (Option<String>, BTreeMap<String, String>);

/// Take `env` out of a job document. It is either a string (`env = "server"`)
/// or a table (`[env]`) of overrides with an optional reserved `mode` key,
/// since TOML cannot hold both forms under one key. Returns the mode and the
/// overrides.
fn extract_env(document: &mut toml::Table) -> Result<EnvParts, Vec<String>> {
    match document.remove("env") {
        Some(toml::Value::Table(mut table)) => {
            let mut errors = Vec::new();
            let mode = match table.remove("mode") {
                None => None,
                Some(toml::Value::String(mode)) => Some(mode),
                Some(other) => {
                    errors.push(format!(
                        "env.mode: must be a string, found {}",
                        other.type_str()
                    ));
                    None
                }
            };
            let mut vars = BTreeMap::new();
            for (key, value) in table {
                match value {
                    toml::Value::String(value) => {
                        vars.insert(key, value);
                    }
                    other => errors.push(format!(
                        "env.{key}: must be a string, found {}",
                        other.type_str()
                    )),
                }
            }
            if errors.is_empty() {
                Ok((mode, vars))
            } else {
                Err(errors)
            }
        }
        Some(toml::Value::String(mode)) => Ok((Some(mode), BTreeMap::new())),
        Some(other) => Err(vec![format!(
            "env: must be a string (\"login-shell\" or \"server\") or a table of variables, found {}",
            other.type_str()
        )]),
        None => Ok((None, BTreeMap::new())),
    }
}

/// Parse one `job.toml` document. `dir_name` is the job directory's name.
pub(crate) fn parse_job_toml(
    content: &str,
    dir_name: &str,
    dir: &Path,
) -> Result<JobDef, Vec<String>> {
    let mut document: toml::Table =
        toml::from_str(content).map_err(|err| vec![err.to_string().trim_end().to_owned()])?;
    let (mode, env_vars) = extract_env(&mut document)?;
    let mut raw: RawJob = toml::Value::Table(document)
        .try_into()
        .map_err(|err: toml::de::Error| vec![err.to_string().trim_end().to_owned()])?;
    raw.env_vars = env_vars;
    raw.env = mode;
    build_job(raw, Some(dir_name), dir, JobSource::User)
}

#[derive(Debug, Deserialize)]
struct PluginJobsOnly {
    #[serde(default)]
    jobs: Vec<toml::Table>,
}

/// Parse the `[[jobs]]` tables of a plugin manifest. A table that fails
/// validation is reported as an `InvalidJob` instead of hiding its siblings.
pub(crate) fn parse_plugin_manifest_jobs(
    content: &str,
    plugin_id: &str,
    plugin_root: &Path,
    manifest_path: &Path,
) -> Result<Vec<Result<JobDef, InvalidJob>>, String> {
    let manifest: PluginJobsOnly =
        toml::from_str(content).map_err(|err| err.to_string().trim_end().to_owned())?;
    let source = JobSource::Plugin {
        plugin_id: plugin_id.to_owned(),
        plugin_root: plugin_root.to_path_buf(),
    };
    let mut results = Vec::new();
    for (index, mut table) in manifest.jobs.into_iter().enumerate() {
        let label = table
            .get("id")
            .and_then(toml::Value::as_str)
            .map_or_else(|| format!("jobs[{index}]"), str::to_owned);
        let invalid = |diagnostics: Vec<String>| InvalidJob {
            id: format!("{plugin_id}:{label}"),
            path: manifest_path.to_path_buf(),
            diagnostics,
            source: source.clone(),
        };
        let (mode, env_vars) = match extract_env(&mut table) {
            Ok(parts) => parts,
            Err(errors) => {
                results.push(Err(invalid(errors)));
                continue;
            }
        };
        let raw: Result<RawJob, _> = toml::Value::Table(table).try_into();
        let mut raw = match raw {
            Ok(raw) => raw,
            Err(err) => {
                results.push(Err(invalid(vec![err.to_string().trim_end().to_owned()])));
                continue;
            }
        };
        raw.env_vars = env_vars;
        raw.env = mode;
        results.push(build_job(raw, None, plugin_root, source.clone()).map_err(invalid));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC: fn(i64) -> i32 = |_| 0;

    fn parse(content: &str) -> Result<JobDef, Vec<String>> {
        parse_job_toml(content, "sync", Path::new("/jobs/sync"))
    }

    #[test]
    fn minimal_interval_job_gets_defaults() {
        let job = parse("command = [\"echo\", \"hi\"]\nevery = \"5m\"\n").unwrap();
        assert_eq!(job.id, "sync");
        assert_eq!(job.title, "sync");
        assert_eq!(job.schedule, Schedule::Every(Duration::from_secs(300)));
        assert_eq!(job.timeout, DEFAULT_TIMEOUT);
        assert!(job.enabled);
        assert_eq!(job.env, EnvMode::LoginShell);
        assert_eq!(job.notify, NotifyPolicy::OnFailure);
        assert_eq!(job.cwd, PathBuf::from("/jobs/sync"));
        assert!(job.feed.is_none());
        assert_eq!(job.source, JobSource::User);
    }

    #[test]
    fn full_job_parses_every_documented_key() {
        let job = parse(
            r#"
id = "sync"
title = "workbox sync"
command = ["workbox", "sync"]
schedule = "0 10 * * 1-5"
timeout = "90s"
enabled = false
env_file = "secrets.env"
cwd = "work"
notify = "on_change"
feed = { command = ["sh", "-c", "workbox list --json"], title = "Today (workbox)" }

[env]
mode = "server"
LOG_LEVEL = "debug"
"#,
        )
        .unwrap();
        assert_eq!(job.title, "workbox sync");
        assert!(matches!(job.schedule, Schedule::Cron(_)));
        assert_eq!(job.schedule.describe(), "0 10 * * 1-5");
        assert_eq!(job.timeout, Duration::from_secs(90));
        assert!(!job.enabled);
        assert_eq!(job.env, EnvMode::Server);
        assert_eq!(job.env_file, Some(PathBuf::from("/jobs/sync/secrets.env")));
        assert_eq!(job.cwd, PathBuf::from("/jobs/sync/work"));
        assert_eq!(job.notify, NotifyPolicy::OnChange);
        assert_eq!(job.env_overrides.get("LOG_LEVEL").unwrap(), "debug");
        assert!(!job.env_overrides.contains_key("mode"));
        let feed = job.feed.unwrap();
        assert_eq!(feed.title.as_deref(), Some("Today (workbox)"));
        assert_eq!(feed.command[0], "sh");
    }

    #[test]
    fn env_table_without_a_mode_keeps_the_login_shell_default() {
        let job = parse("command = [\"x\"]\nevery = \"1h\"\n[env]\nA = \"1\"\n").unwrap();
        assert_eq!(job.env, EnvMode::LoginShell);
        assert_eq!(job.env_overrides.len(), 1);
    }

    #[test]
    fn exactly_one_of_schedule_and_every() {
        let both =
            parse("command = [\"x\"]\nschedule = \"* * * * *\"\nevery = \"1h\"\n").unwrap_err();
        assert!(both.iter().any(|d| d.contains("not both")), "{both:?}");
        let neither = parse("command = [\"x\"]\n").unwrap_err();
        assert!(
            neither.iter().any(|d| d.contains("set exactly one")),
            "{neither:?}"
        );
    }

    #[test]
    fn cron_errors_name_the_expression_and_the_problem() {
        let errors = parse("command = [\"x\"]\nschedule = \"0 10 * *\"\n").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("schedule: invalid cron \"0 10 * *\""));
        assert!(errors[0].contains("expected 5 fields"));
    }

    #[test]
    fn interval_must_be_at_least_one_minute() {
        let errors = parse("command = [\"x\"]\nevery = \"30s\"\n").unwrap_err();
        assert!(errors[0].contains("1m minimum"), "{errors:?}");
        assert!(parse("command = [\"x\"]\nevery = \"60s\"\n").is_ok());
        assert!(parse("command = [\"x\"]\nevery = \"1m\"\n").is_ok());
    }

    #[test]
    fn timeout_bounds() {
        let ok = parse("command = [\"x\"]\nevery = \"1h\"\ntimeout = \"24h\"\n").unwrap();
        assert_eq!(ok.timeout, MAX_TIMEOUT);
        let too_long = parse("command = [\"x\"]\nevery = \"1h\"\ntimeout = \"25h\"\n").unwrap_err();
        assert!(too_long[0].contains("24h maximum"), "{too_long:?}");
        let too_short = parse("command = [\"x\"]\nevery = \"1h\"\ntimeout = \"0s\"\n").unwrap_err();
        assert!(too_short[0].contains("shorter than 1s"), "{too_short:?}");
    }

    #[test]
    fn all_problems_are_reported_together() {
        let errors = parse(
            "command = []\nevery = \"1h\"\nenv = \"bogus\"\nnotify = \"loud\"\ntimeout = \"x\"\n",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors.iter().any(|d| d.starts_with("command: required")));
        assert!(errors.iter().any(|d| d.starts_with("env: 'bogus'")));
        assert!(errors.iter().any(|d| d.starts_with("notify: 'loud'")));
        assert!(errors.iter().any(|d| d.starts_with("timeout:")));
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_surface() {
        let errors = parse("command = [\"x\"]\nevry = \"1h\"\n").unwrap_err();
        assert!(errors[0].contains("unknown field `evry`"), "{errors:?}");
        assert!(
            !errors[0].ends_with('\n'),
            "no trailing newline: {errors:?}"
        );
    }

    #[test]
    fn toml_syntax_errors_carry_a_location() {
        let errors = parse("command = [\n").unwrap_err();
        assert!(errors[0].contains("line"), "{errors:?}");
    }

    #[test]
    fn id_rules() {
        for good in ["a", "workbox-sync", "a.b_c-1", "9lives"] {
            assert!(valid_job_id(good), "{good}");
        }
        for bad in ["", ".hidden", "..", "-x", "a/b", "a b", "a:b", "é"] {
            assert!(!valid_job_id(bad), "{bad:?}");
        }
        assert!(!valid_job_id(&"a".repeat(121)));
        assert!(valid_job_id(&"a".repeat(120)));
    }

    #[test]
    fn id_in_file_must_match_directory() {
        let errors = parse("id = \"other\"\ncommand = [\"x\"]\nevery = \"1h\"\n").unwrap_err();
        assert!(errors[0].contains("does not match the directory name 'sync'"));
        let invalid = parse_job_toml(
            "command = [\"x\"]\nevery = \"1h\"\n",
            "bad id",
            Path::new("/jobs/bad id"),
        )
        .unwrap_err();
        assert!(invalid[0].starts_with("id: 'bad id' is invalid"));
    }

    #[test]
    fn env_override_names_and_types_are_checked() {
        let errors =
            parse("command = [\"x\"]\nevery = \"1h\"\n[env]\n\"1BAD\" = \"v\"\nGOOD = 3\n")
                .unwrap_err();
        assert!(errors
            .iter()
            .any(|d| d.contains("env.GOOD: must be a string")));
        let errors =
            parse("command = [\"x\"]\nevery = \"1h\"\n[env]\n\"1BAD\" = \"v\"\n").unwrap_err();
        assert!(errors[0].contains("env.1BAD: not a valid environment variable name"));
    }

    #[test]
    fn feed_requires_a_command() {
        let errors =
            parse("command = [\"x\"]\nevery = \"1h\"\nfeed = { title = \"t\" }\n").unwrap_err();
        assert!(
            errors[0].starts_with("feed.command: required"),
            "{errors:?}"
        );
    }

    #[test]
    fn handoff_table_is_validated() {
        let ok = parse(
            "command = [\"x\"]\nevery = \"1h\"\n[handoff]\nwhen = \"exit:2\"\nagent_command = \"claude\"\n",
        )
        .unwrap();
        assert_eq!(ok.handoff.unwrap().when, "exit:2");
        let errors = parse("command = [\"x\"]\nevery = \"1h\"\n[handoff]\nwhen = \"sometimes\"\n")
            .unwrap_err();
        assert!(
            errors[0].starts_with("handoff.when: 'sometimes'"),
            "{errors:?}"
        );
        assert!(parse("command = [\"x\"]\nevery = \"1h\"\n[handoff]\nbogus = 1\n").is_err());
    }

    #[test]
    fn durations_parse_and_format() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5_400));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("").is_err());
        assert_eq!(format_duration(Duration::from_secs(5_400)), "1h30m");
        assert_eq!(format_duration(Duration::from_secs(60)), "1m");
    }

    #[test]
    fn interval_schedule_adds_the_interval() {
        let schedule = Schedule::Every(Duration::from_secs(3_600));
        assert_eq!(schedule.next_after(1_000, &UTC), Some(4_600));
        assert_eq!(schedule.describe(), "every 1h");
    }

    #[test]
    fn plugin_jobs_are_namespaced_and_use_the_plugin_root() {
        let manifest = r#"
id = "acme.tools"
name = "Acme"

[[jobs]]
id = "refresh"
command = ["./refresh.sh"]
every = "1h"

[[jobs]]
id = "broken"
command = ["x"]
"#;
        let results = parse_plugin_manifest_jobs(
            manifest,
            "acme.tools",
            Path::new("/plugins/acme"),
            Path::new("/plugins/acme/flockr-plugin.toml"),
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        let good = results[0].as_ref().unwrap();
        assert_eq!(good.id, "acme.tools:refresh");
        assert_eq!(good.cwd, PathBuf::from("/plugins/acme"));
        assert!(matches!(good.source, JobSource::Plugin { .. }));
        let bad = results[1].as_ref().unwrap_err();
        assert_eq!(bad.id, "acme.tools:broken");
        assert!(bad.diagnostics[0].contains("set exactly one"));
    }

    #[test]
    fn plugin_manifest_without_jobs_yields_none() {
        let results = parse_plugin_manifest_jobs(
            "id = \"p\"\nname = \"n\"\n",
            "p",
            Path::new("/p"),
            Path::new("/p/flockr-plugin.toml"),
        )
        .unwrap();
        assert!(results.is_empty());
    }
}
