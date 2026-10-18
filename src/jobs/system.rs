//! System jobs: jobs flockr ships with, defined in code under the id prefix
//! `system.`. They show in the Jobs overlay and `flockr job list` with source
//! `system`, run through the same scheduler and runner as user jobs, and are
//! tuned from config:
//!
//! ```toml
//! [jobs.system.upstream-herdr]
//! every = "24h"          # or schedule = "0 9 * * *"
//! enabled = true
//! notify = "never"
//! ```
//!
//! To add one, write a `fn(&SystemContext) -> Base` and list it in `SPECS`.
//! Everything else (the config overrides, validation, diagnostics, the
//! overlay, the schedule editor) is shared.

use std::path::PathBuf;
use std::time::Duration;

use super::definition::{
    parse_duration, EnvMode, JobDef, JobSource, NotifyPolicy, Schedule, DEFAULT_TIMEOUT,
    MIN_INTERVAL,
};
use super::schedule_edit::{parse_schedule_input, schedule_from_text};
use crate::config::{JobsConfig, SystemJobConfig, UpstreamConfig};

pub(crate) const SYSTEM_PREFIX: &str = "system.";
pub(crate) const UPSTREAM_JOB_NAME: &str = "upstream-herdr";

/// What system jobs are built from.
pub(crate) struct SystemContext<'a> {
    /// The flockr executable system jobs run themselves with.
    pub(crate) exe: PathBuf,
    pub(crate) upstream: &'a UpstreamConfig,
    pub(crate) jobs: &'a JobsConfig,
    /// The launch policy allows background update checks.
    pub(crate) background_updates: bool,
}

/// A system job before config overrides.
struct Base {
    title: &'static str,
    command: Vec<String>,
    schedule: Schedule,
    enabled: bool,
    notify: NotifyPolicy,
    timeout: Duration,
}

struct Spec {
    /// The id without the `system.` prefix; also the key under
    /// `[jobs.system]`.
    name: &'static str,
    build: fn(&SystemContext<'_>) -> Base,
}

const SPECS: &[Spec] = &[Spec {
    name: UPSTREAM_JOB_NAME,
    build: upstream_herdr,
}];

/// The upstream herdr release watcher. It runs `flockr upstream check --job`
/// (see `cli::upstream`), which updates the watcher's state file and, for a
/// release the user has not been told about, prints a handoff line whose
/// agent command is `flockr upstream sync <tag>`.
///
/// The old `[upstream]` keys still apply as the defaults:
/// `check_interval_hours` is the interval and `enabled` switches the job.
/// `[jobs.system.upstream-herdr]` wins over both.
fn upstream_herdr(ctx: &SystemContext<'_>) -> Base {
    let hours = u64::from(ctx.upstream.check_interval_hours.max(1));
    Base {
        title: "upstream herdr watcher",
        command: vec![
            ctx.exe.display().to_string(),
            "upstream".into(),
            "check".into(),
            "--job".into(),
        ],
        schedule: Schedule::Every(Duration::from_secs(hours * 3600)),
        enabled: ctx.upstream.enabled && ctx.background_updates,
        // The decision toast is the announcement; a failed network check is
        // visible in the Jobs overlay without a toast every day.
        notify: NotifyPolicy::Never,
        timeout: Duration::from_secs(2 * 60),
    }
}

pub(crate) struct SystemJobs {
    pub(crate) jobs: Vec<JobDef>,
    /// Problems with `[jobs.system.*]`, for the config diagnostics.
    pub(crate) diagnostics: Vec<String>,
}

fn apply_overrides(
    name: &str,
    base: &mut Base,
    config: Option<&SystemJobConfig>,
    diagnostics: &mut Vec<String>,
) {
    let Some(config) = config else {
        return;
    };
    let table = format!("[jobs.system.{name}]");
    match (config.every.as_deref(), config.schedule.as_deref()) {
        (Some(_), Some(_)) => diagnostics.push(format!(
            "{table}: set exactly one of every and schedule, not both; keeping the default schedule"
        )),
        (Some(text), None) => match parse_duration(text) {
            Ok(interval) if interval < MIN_INTERVAL => diagnostics.push(format!(
                "{table} every = \"{text}\" is shorter than the 1m minimum; keeping the default schedule"
            )),
            Ok(interval) => base.schedule = Schedule::Every(interval),
            Err(err) => diagnostics.push(format!(
                "{table} every {err}; keeping the default schedule"
            )),
        },
        (None, Some(text)) => match parse_schedule_input(text)
            .ok()
            .and_then(|_| schedule_from_text(text))
        {
            Some(schedule @ Schedule::Cron(_)) => base.schedule = schedule,
            _ => diagnostics.push(format!(
                "{table} schedule = \"{text}\" is not a 5-field cron expression; keeping the default schedule"
            )),
        },
        (None, None) => {}
    }
    if let Some(enabled) = config.enabled {
        base.enabled = enabled;
    }
    if let Some(notify) = config.notify.as_deref() {
        match notify {
            "never" => base.notify = NotifyPolicy::Never,
            "on_failure" => base.notify = NotifyPolicy::OnFailure,
            "on_change" => base.notify = NotifyPolicy::OnChange,
            "always" => base.notify = NotifyPolicy::Always,
            other => diagnostics.push(format!(
                "{table} notify = \"{other}\" is not valid; use never, on_failure, on_change or always"
            )),
        }
    }
}

/// Build every system job from the defaults, the legacy `[upstream]` keys and
/// `[jobs.system.*]`, with a diagnostic for each override that does not apply
/// and for each table that names no system job.
pub(crate) fn build(ctx: &SystemContext<'_>) -> SystemJobs {
    let mut diagnostics = Vec::new();
    for name in ctx.jobs.system.keys() {
        if !SPECS.iter().any(|spec| spec.name == name) {
            let known = SPECS
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(format!(
                "[jobs.system.{name}] names no system job (known: {known})"
            ));
        }
    }
    let state_dir = crate::config::state_dir();
    let jobs = SPECS
        .iter()
        .map(|spec| {
            let mut base = (spec.build)(ctx);
            apply_overrides(
                spec.name,
                &mut base,
                ctx.jobs.system.get(spec.name),
                &mut diagnostics,
            );
            JobDef {
                id: format!("{SYSTEM_PREFIX}{}", spec.name),
                title: base.title.to_owned(),
                command: base.command,
                schedule: base.schedule,
                timeout: if base.timeout.is_zero() {
                    DEFAULT_TIMEOUT
                } else {
                    base.timeout
                },
                enabled: base.enabled,
                env: EnvMode::Server,
                env_file: None,
                env_overrides: Default::default(),
                notify: base.notify,
                cwd: std::env::temp_dir(),
                dir: state_dir.clone(),
                feed: None,
                handoff: None,
                source: JobSource::System,
            }
        })
        .collect();
    SystemJobs { jobs, diagnostics }
}

/// The config key of a system job id, or `None` for any other id.
pub(crate) fn system_job_name(job_id: &str) -> Option<&str> {
    job_id
        .strip_prefix(SYSTEM_PREFIX)
        .filter(|name| SPECS.iter().any(|spec| spec.name == *name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_with(upstream: &UpstreamConfig, jobs: &JobsConfig, bg: bool) -> SystemJobs {
        build(&SystemContext {
            exe: PathBuf::from("/usr/bin/flockr"),
            upstream,
            jobs,
            background_updates: bg,
        })
    }

    fn upstream_job(jobs: &SystemJobs) -> &JobDef {
        jobs.jobs
            .iter()
            .find(|job| job.id == "system.upstream-herdr")
            .expect("upstream job")
    }

    fn overrides(toml_text: &str) -> JobsConfig {
        let config: crate::config::Config = toml::from_str(toml_text).unwrap();
        config.jobs
    }

    #[test]
    fn the_upstream_watcher_is_registered_with_sensible_defaults() {
        let jobs = build_with(&UpstreamConfig::default(), &JobsConfig::default(), true);
        assert!(jobs.diagnostics.is_empty());
        let job = upstream_job(&jobs);
        assert_eq!(job.source, JobSource::System);
        assert_eq!(job.title, "upstream herdr watcher");
        assert_eq!(
            job.schedule,
            Schedule::Every(Duration::from_secs(24 * 3600))
        );
        assert!(job.enabled);
        assert_eq!(job.env, EnvMode::Server);
        assert_eq!(job.notify, NotifyPolicy::Never);
        assert_eq!(
            job.command,
            ["/usr/bin/flockr", "upstream", "check", "--job"]
        );
        assert!(job.cwd.is_dir());
    }

    #[test]
    fn the_old_upstream_keys_map_onto_the_job() {
        let upstream = UpstreamConfig {
            enabled: false,
            check_interval_hours: 6,
            ..UpstreamConfig::default()
        };
        let jobs = build_with(&upstream, &JobsConfig::default(), true);
        let job = upstream_job(&jobs);
        assert_eq!(job.schedule, Schedule::Every(Duration::from_secs(6 * 3600)));
        assert!(!job.enabled, "[upstream] enabled = false disables the job");
        // A zero is floored to the hour like the old watcher did.
        let zero = UpstreamConfig {
            check_interval_hours: 0,
            ..UpstreamConfig::default()
        };
        let jobs = build_with(&zero, &JobsConfig::default(), true);
        assert_eq!(
            upstream_job(&jobs).schedule,
            Schedule::Every(Duration::from_secs(3600))
        );
    }

    #[test]
    fn launch_policy_without_background_updates_keeps_the_job_off() {
        let jobs = build_with(&UpstreamConfig::default(), &JobsConfig::default(), false);
        assert!(!upstream_job(&jobs).enabled);
    }

    #[test]
    fn jobs_system_overrides_win_over_the_old_keys() {
        let upstream = UpstreamConfig {
            enabled: false,
            check_interval_hours: 6,
            ..UpstreamConfig::default()
        };
        let config = overrides(
            "[jobs.system.upstream-herdr]\nevery = \"90m\"\nenabled = true\nnotify = \"on_failure\"\n",
        );
        let jobs = build_with(&upstream, &config, true);
        assert!(jobs.diagnostics.is_empty(), "{:?}", jobs.diagnostics);
        let job = upstream_job(&jobs);
        assert_eq!(job.schedule, Schedule::Every(Duration::from_secs(90 * 60)));
        assert!(job.enabled);
        assert_eq!(job.notify, NotifyPolicy::OnFailure);
    }

    #[test]
    fn a_cron_override_replaces_the_interval() {
        let config = overrides("[jobs.system.upstream-herdr]\nschedule = \"0 9 * * 1-5\"\n");
        let jobs = build_with(&UpstreamConfig::default(), &config, true);
        assert!(matches!(upstream_job(&jobs).schedule, Schedule::Cron(_)));
        assert_eq!(upstream_job(&jobs).schedule.describe(), "0 9 * * 1-5");
    }

    #[test]
    fn bad_overrides_are_reported_and_the_defaults_kept() {
        for (text, expect) in [
            ("every = \"1h\"\nschedule = \"0 9 * * *\"\n", "not both"),
            ("every = \"30s\"\n", "1m minimum"),
            ("every = \"soon\"\n", "every"),
            ("schedule = \"every 2h\"\n", "5-field cron"),
            ("schedule = \"0 9 * *\"\n", "5-field cron"),
            ("notify = \"loud\"\n", "notify"),
        ] {
            let config = overrides(&format!("[jobs.system.upstream-herdr]\n{text}"));
            let jobs = build_with(&UpstreamConfig::default(), &config, true);
            assert_eq!(jobs.diagnostics.len(), 1, "{text}: {:?}", jobs.diagnostics);
            assert!(
                jobs.diagnostics[0].contains(expect),
                "{text}: {:?}",
                jobs.diagnostics
            );
            assert_eq!(
                upstream_job(&jobs).schedule,
                Schedule::Every(Duration::from_secs(24 * 3600)),
                "{text}"
            );
        }
    }

    #[test]
    fn an_unknown_system_job_name_is_reported() {
        let config = overrides("[jobs.system.upstream-herd]\nevery = \"1h\"\n");
        let jobs = build_with(&UpstreamConfig::default(), &config, true);
        assert_eq!(jobs.diagnostics.len(), 1);
        assert!(jobs.diagnostics[0].contains("names no system job"));
        assert!(jobs.diagnostics[0].contains("upstream-herdr"));
    }

    #[test]
    fn system_job_names_are_recognised_by_id() {
        assert_eq!(
            system_job_name("system.upstream-herdr"),
            Some("upstream-herdr")
        );
        assert_eq!(system_job_name("system.nope"), None);
        assert_eq!(system_job_name("upstream-herdr"), None);
    }
}
