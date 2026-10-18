use serde::{Deserialize, Serialize};

/// Feeds are lists of items that a job prints in the feed format (version 1)
/// and flockr stores per job. Action commands stay on the server; clients get
/// only the key and title of each action and ask the server to run it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedItemState {
    Unread,
    #[default]
    Read,
    Done,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedActionInfo {
    /// The single key that runs the action in the Feed overlay.
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedItemInfo {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub state: FeedItemState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// An http or https link; Enter or a click opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The item's own timestamp as the feed gave it (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<FeedActionInfo>,
}

/// The outcome of the last item action run for a feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedActionResult {
    pub item_id: String,
    pub key: String,
    pub ok: bool,
    /// One short line: `ok`, or why it failed.
    pub summary: String,
    pub finished_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedSummary {
    /// The job that produces the feed.
    pub job_id: String,
    /// The `feed.title` from the job file, else the feed's own title, else the job title.
    pub title: String,
    pub count: u32,
    pub unread: u32,
    /// Advances when the feed's items, title or staleness change.
    pub revision: u64,
    /// The last refresh failed; the items are from the last good one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    /// A feed has been read from the job's command at least once.
    pub loaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_unix_ms: Option<u64>,
    /// Why the last refresh failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A refresh or an item action is running.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refreshing: bool,
    /// Problems found while reading the feed that did not stop it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action: Option<FeedActionResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedListParams {
    /// When it equals the server's current revision the response sets
    /// `unchanged` and omits the feed list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedGetParams {
    pub job_id: String,
    /// When it equals the feed's `revision` the response sets `unchanged` and
    /// omits the items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_revision: Option<u64>,
    /// Return at most this many items; `count` in the summary stays the total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedTarget {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FeedRunActionParams {
    pub job_id: String,
    pub item_id: String,
    /// The action's key as listed on the item.
    pub key: String,
}
