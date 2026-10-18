use std::collections::BTreeMap;

use serde::Deserialize;

/// `[jobs]` settings. User jobs live in `~/.config/flockr/jobs/<id>/job.toml`;
/// this section only tunes the jobs flockr ships with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct JobsConfig {
    /// Overrides for flockr's built-in jobs, keyed by name without the
    /// `system.` prefix, for example `[jobs.system.upstream-herdr]`. Each
    /// table takes `every` or `schedule` (exactly one), `enabled` and
    /// `notify`. Default: none.
    pub system: BTreeMap<String, SystemJobConfig>,
}

/// One `[jobs.system.<name>]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SystemJobConfig {
    /// Run on an interval such as "24h" or "90m" (at least one minute).
    pub every: Option<String>,
    /// Run on a 5-field cron expression in local time, such as "0 9 * * 1-5".
    pub schedule: Option<String>,
    /// Turn the job on or off.
    pub enabled: Option<bool>,
    /// Toast policy: never, on_failure, on_change or always.
    pub notify: Option<String>,
}
