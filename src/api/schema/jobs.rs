use serde::{Deserialize, Serialize};

/// Jobs are global per user: scheduled commands that flockr runs on a cron
/// schedule or an interval and records the result of. Nothing here carries
/// environment values; only counts and diagnostics are exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobRunStatus {
    Success,
    Failed,
    TimedOut,
    SpawnFailed,
    /// Not run because the previous run was still going.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobRunTrigger {
    Schedule,
    /// Ran once at server start because a scheduled time passed while flockr
    /// was not running.
    CatchUp,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Idle,
    Running,
    Disabled,
    /// The job file has problems; see `diagnostics`.
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobScheduleKind {
    Cron,
    Interval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobRunInfo {
    pub run_id: u64,
    pub trigger: JobRunTrigger,
    pub status: JobRunStatus,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// One short line: the exit code, the timeout, or why it did not start.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Consecutive skipped runs folded into this entry.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_count: u32,
    /// Which environment the run used, without values, for example
    /// `login-shell (214 variables)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_note: Option<String>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobInfo {
    pub job_id: String,
    pub title: String,
    /// `user`, or `plugin:<plugin id>`.
    pub source: String,
    pub command: Vec<String>,
    /// The cron expression, or `every 1h`.
    pub schedule: String,
    pub schedule_kind: JobScheduleKind,
    pub timeout_secs: u64,
    /// Effective: the `enabled_override` wins over the job file.
    pub enabled: bool,
    /// An `enable`/`disable` command overrides the job file's `enabled`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enabled_overridden: bool,
    /// `login-shell` or `server`.
    pub env: String,
    pub notify: String,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_since_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<JobRunInfo>,
    /// The job file declares a `feed` table.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_feed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    /// Directory that holds the job's files.
    pub dir: String,
}

/// State of the cached login-shell environment. Values are never exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobEnvStatus {
    /// `none` before the first capture, `login-shell`, or `server-fallback`
    /// when the capture failed and jobs use the server environment.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_unix_ms: Option<u64>,
    pub variable_count: u32,
    pub refreshing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobListParams {
    /// When it equals the server's current revision the response sets
    /// `unchanged` and omits the job list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobTarget {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobSetEnabledParams {
    pub job_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobLogsParams {
    pub job_id: String,
    /// A run from the job's history; the latest finished run by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    /// Keep only the last N lines of each stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}
