use serde::{Deserialize, Serialize};

/// Pending decisions: a job asked whether to start an agent tab (its
/// `[handoff]`), and nothing starts until the user answers. Decisions are
/// global per user and persist across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// Waiting for an answer.
    Pending,
    /// Hidden until `snoozed_until_unix_ms`, then pending again.
    Snoozed,
    /// The agent tab was started.
    Started,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionInfo {
    /// `d1`, `d2`, ...
    pub decision_id: String,
    pub job_id: String,
    pub job_title: String,
    /// The run whose output produced the offer.
    pub run_id: u64,
    /// The question shown to the user.
    pub ask: String,
    pub tab_title: String,
    /// Working directory of the tab that Start opens.
    pub cwd: String,
    /// The agent command, variables filled in.
    pub agent_command: String,
    pub status: DecisionStatus,
    pub created_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until_unix_ms: Option<u64>,
    /// When it was started or dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_unix_ms: Option<u64>,
    /// What happened when it was started or dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Why the last Start failed; the decision stays pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The rendered prompt file, when the handoff has a prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_file: Option<String>,
    /// The dynamic handoff's `context` object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
    /// The rendered prompt, only when `decision.list` asked for it
    /// (`include_prompt`), cut to 64 KiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionListParams {
    /// When it equals the server's current revision the response sets
    /// `unchanged` and omits the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
    /// Also list started and dismissed decisions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_resolved: bool,
    /// Return only this decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// Fill in `prompt` (for a preview).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_prompt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionTarget {
    pub decision_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DecisionSnoozeParams {
    pub decision_id: String,
    /// Seconds to hide the decision for; 3600 by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_secs: Option<u64>,
}
