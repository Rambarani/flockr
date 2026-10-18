//! Scheduled jobs: commands that flockr runs on a cron schedule or an
//! interval, with the result recorded and optionally announced.
//!
//! Definitions live in `~/.config/flockr/jobs/<id>/job.toml` (and `[[jobs]]`
//! tables in plugin manifests). The registry and scheduler are in `manager`,
//! process execution in `runner`, the login-shell environment in `env`, and
//! persisted history in `state`. The app loop owns one `JobsManager`, ticks
//! it from a single deadline, and runs jobs on named worker threads that
//! report through `AppEvent::JobRunFinished`.

pub(crate) mod authoring;
pub(crate) mod cron;
pub(crate) mod decisions;
pub(crate) mod definition;
pub(crate) mod display;
pub(crate) mod env;
pub(crate) mod handoff;
pub(crate) mod manager;
pub(crate) mod runner;
pub(crate) mod schedule_edit;
pub(crate) mod state;
pub(crate) mod system;

pub(crate) use manager::{now_unix_ms, BeginError, JobsManager, JobsPaths, PluginJobSource};
pub(crate) use runner::JobRunResult;
