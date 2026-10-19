use serde::{Deserialize, Serialize};

use super::agents::AgentInfo;
use super::common::{ClientWindowTitleReason, NotificationShowReason};
use super::decision::DecisionInfo;
use super::events::EventEnvelope;
use super::feed::{FeedItemInfo, FeedSummary};
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::jobs::{JobEnvStatus, JobInfo, JobRunInfo};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSwapResult, PaneTextPoint, PaneTextRange, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::server::ServerCapabilities;
use super::session::SessionSnapshot;
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;
use super::worktrees::{WorktreeInfo, WorktreeSourceInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuccessResponse {
    pub id: String,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong {
        version: String,
        protocol: u32,
        #[serde(default)]
        capabilities: Option<ServerCapabilities>,
    },
    SessionSnapshot {
        snapshot: Box<SessionSnapshot>,
    },
    WorkspaceInfo {
        workspace: WorkspaceInfo,
    },
    WorkspaceCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    WorkspaceList {
        workspaces: Vec<WorkspaceInfo>,
    },
    WorktreeList {
        source: WorktreeSourceInfo,
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
    },
    WorktreeOpened {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
        already_open: bool,
    },
    WorktreeRemoved {
        workspace_id: String,
        path: String,
        forced: bool,
    },
    TabInfo {
        tab: TabInfo,
    },
    TabCreated {
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    TabList {
        tabs: Vec<TabInfo>,
    },
    AgentInfo {
        agent: AgentInfo,
    },
    AgentStarted {
        agent: AgentInfo,
        argv: Vec<String>,
    },
    AgentPrompted {
        agent: AgentInfo,
    },
    AgentList {
        agents: Vec<AgentInfo>,
    },
    AgentView {
        active: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    PaneInfo {
        pane: PaneInfo,
    },
    PaneList {
        panes: Vec<PaneInfo>,
    },
    PaneCurrent {
        pane: PaneInfo,
    },
    PaneSwap {
        swap: PaneSwapResult,
    },
    PaneMove {
        move_result: PaneMoveResult,
    },
    PaneZoom {
        zoom: PaneZoomResult,
    },
    PaneLayout {
        layout: PaneLayoutSnapshot,
    },
    PaneProcessInfo {
        process_info: PaneProcessInfo,
    },
    LayoutExport {
        layout: LayoutDescription,
    },
    LayoutApply {
        layout: LayoutDescription,
    },
    LayoutSplitRatioSet {
        layout: LayoutDescription,
    },
    PaneNeighbor {
        neighbor: PaneNeighborResult,
    },
    PaneEdges {
        edges: PaneEdgesResult,
    },
    PaneFocusDirection {
        focus: PaneFocusDirectionResult,
    },
    PaneResize {
        resize: PaneResizeResult,
    },
    PaneRead {
        read: PaneReadResult,
    },
    PaneSelection {
        pane_id: String,
        text: String,
    },
    PaneCopyMotion {
        pane_id: String,
        cursor: PaneTextPoint,
        content_revision: u64,
    },
    PaneCopySearch {
        pane_id: String,
        content_revision: u64,
        matches: Vec<PaneTextRange>,
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_global: Option<u64>,
    },
    AgentExplain {
        explain: serde_json::Value,
    },
    SubscriptionStarted {},
    WaitMatched {
        event: EventEnvelope,
    },
    OutputMatched {
        pane_id: String,
        revision: u64,
        matched_line: Option<String>,
        read: PaneReadResult,
    },
    NotificationShow {
        shown: bool,
        reason: NotificationShowReason,
    },
    ClientWindowTitle {
        changed: bool,
        reason: ClientWindowTitleReason,
    },
    IntegrationList {
        integrations: Vec<super::integrations::IntegrationInfo>,
    },
    IntegrationInstall {
        target: IntegrationTarget,
        details: IntegrationInstallResult,
    },
    IntegrationUninstall {
        target: IntegrationTarget,
        details: IntegrationUninstallResult,
    },
    AgentManifestReload {
        manifests: Vec<AgentManifestInfo>,
    },
    AgentManifestStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_check_unix: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_result: Option<String>,
        manifests: Vec<AgentManifestInfo>,
    },
    PluginLinked {
        plugin: InstalledPluginInfo,
    },
    PluginList {
        plugins: Vec<InstalledPluginInfo>,
    },
    PluginUnlinked {
        plugin_id: String,
        removed: bool,
    },
    PluginEnabled {
        plugin: InstalledPluginInfo,
    },
    PluginDisabled {
        plugin: InstalledPluginInfo,
    },
    PluginActionList {
        actions: Vec<PluginActionInfo>,
    },
    PluginActionInvoked {
        action: PluginActionInfo,
        context: PluginInvocationContext,
        log: PluginCommandLogInfo,
    },
    PaneLinkResolved {
        regions: Vec<super::panes::PaneLinkRegion>,
    },
    PaneLinkActivated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        handled: bool,
    },
    PluginLogList {
        logs: Vec<PluginCommandLogInfo>,
    },
    PluginPaneOpened {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneFocused {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneClosed {
        pane_id: String,
    },
    JobList {
        /// Advances whenever any job's visible state changes.
        revision: u64,
        /// The request's `since_revision` is current; `jobs` is empty.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        #[serde(default)]
        jobs: Vec<JobInfo>,
        env: JobEnvStatus,
    },
    JobDetail {
        job: JobInfo,
        /// Oldest first, output omitted; at most 20 runs.
        history: Vec<JobRunInfo>,
    },
    JobRunRequested {
        job_id: String,
        /// False when a run was already going.
        started: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    JobUpdated {
        job: JobInfo,
    },
    JobLogs {
        job_id: String,
        run: JobRunInfo,
        stdout: String,
        stderr: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stdout_truncated: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stderr_truncated: bool,
    },
    JobEnvRefreshing {
        env: JobEnvStatus,
    },
    FeedList {
        /// Advances whenever any feed's visible state changes.
        revision: u64,
        /// The request's `since_revision` is current; `feeds` is empty.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        #[serde(default)]
        feeds: Vec<FeedSummary>,
    },
    FeedDetail {
        feed: FeedSummary,
        /// The request's `since_revision` is current; `items` is empty.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        #[serde(default)]
        items: Vec<FeedItemInfo>,
    },
    FeedRefreshRequested {
        job_id: String,
        /// False when a refresh or action was already running, or the job is.
        started: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    FeedActionRequested {
        job_id: String,
        item_id: String,
        key: String,
        /// False when the feed was busy.
        started: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    FeedWorkStarted {
        job_id: String,
        /// Items in the prompt.
        count: u32,
        /// The first word of the agent command, such as `claude`.
        agent: String,
        tab_id: String,
        pane_id: String,
        /// The title the tab was given.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        tab_title: String,
        /// Where the rendered prompt was written.
        prompt_file: String,
        /// Ids that were asked for but are no longer in the feed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skipped: Vec<String>,
        /// Things the user may want to know: a missing template variable, a
        /// fallback directory.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    JobScheduleUpdated {
        job: JobInfo,
        /// Where the new schedule was written.
        stored_in: super::jobs::JobScheduleStore,
        /// Says why the schedule went somewhere unusual.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    DecisionList {
        /// Advances whenever any decision changes.
        revision: u64,
        /// The request's `since_revision` is current; `decisions` is empty.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        /// Decisions waiting for an answer (snoozed ones not counted).
        pending: u32,
        #[serde(default)]
        decisions: Vec<DecisionInfo>,
    },
    DecisionUpdated {
        decision: DecisionInfo,
    },
    DecisionStarted {
        decision: DecisionInfo,
        /// The tab the agent was started in.
        tab_id: String,
        pane_id: String,
    },
    ConfigReload {
        status: crate::config::ConfigReloadStatus,
        diagnostics: Vec<String>,
    },
    /// Acknowledgement for the client-shell surface interest lease. This method is new on the
    /// endpoint protocol, so its revision-bearing result can establish an activation floor.
    ClientShellSurfaceSet {
        active: bool,
        projection_revision: u64,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentManifestInfo {
    pub agent: String,
    pub source: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_remote_version: Option<String>,
    pub local_override_shadowing_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_last_checked_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
