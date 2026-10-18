//! The stored feeds, one per job that declares a `feed`. Pure with respect to
//! threads and the clock: the app starts feed tasks from a `TaskPlan`, and
//! results come back through `finish`. Each feed is read lazily from
//! `<state_dir>/jobs/<id>/feed.json` the first time it is needed and written
//! back atomically after every refresh.
//!
//! A refresh that fails keeps the last good items and marks the feed stale,
//! so the sidebar and overlay keep showing something useful with a warning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::format::{ItemRecord, ParsedFeed};
use crate::api::schema::{
    FeedActionInfo, FeedActionResult, FeedField, FeedItemInfo, FeedItemState, FeedSummary,
};

const STORE_VERSION: u32 = 1;
pub(crate) const STORE_FILE: &str = "feed.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredFeed {
    #[serde(default)]
    version: u32,
    /// When the items were last read successfully.
    #[serde(default)]
    pub(crate) fetched_unix_ms: Option<u64>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) items: Vec<ItemRecord>,
    /// The last refresh failed.
    #[serde(default)]
    pub(crate) stale: bool,
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) diagnostics: Vec<String>,
}

impl StoredFeed {
    fn unread(&self) -> u32 {
        self.items
            .iter()
            .filter(|item| item.state == FeedItemState::Unread)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

/// Everything that makes one stored feed look different to a client.
fn visible_part(
    feed: &StoredFeed,
) -> (
    &Option<String>,
    &Vec<ItemRecord>,
    bool,
    &Option<String>,
    &Vec<String>,
) {
    (
        &feed.title,
        &feed.items,
        feed.stale,
        &feed.error,
        &feed.diagnostics,
    )
}

pub(crate) fn store_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STORE_FILE)
}

/// Load a stored feed. Missing means nothing fetched yet; a corrupt file is
/// logged and ignored rather than blocking the feed.
pub(crate) fn load(path: &Path) -> StoredFeed {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return StoredFeed::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), err = %err, "failed to read stored feed");
            return StoredFeed::default();
        }
    };
    match serde_json::from_str::<StoredFeed>(&content) {
        Ok(stored) => stored,
        Err(err) => {
            tracing::warn!(path = %path.display(), err = %err, "stored feed is corrupt; starting fresh");
            StoredFeed::default()
        }
    }
}

pub(crate) fn save(path: &Path, stored: &StoredFeed) -> std::io::Result<()> {
    crate::jobs::state::write_json_atomic(path, stored)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskKind {
    Refresh,
    Action,
}

/// What an item action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionOutcome {
    pub(crate) item_id: String,
    pub(crate) key: String,
    pub(crate) ok: bool,
    pub(crate) summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    Feed(ParsedFeed),
    Failed(String),
}

/// A finished feed task: an optional action followed by a refresh.
#[derive(Debug)]
pub(crate) struct TaskResult {
    pub(crate) job_id: String,
    pub(crate) action: Option<ActionOutcome>,
    pub(crate) refresh: RefreshOutcome,
    pub(crate) finished_unix_ms: u64,
}

/// What changed when a task finished, for events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finished {
    /// The items, title or staleness changed.
    pub(crate) changed: bool,
    pub(crate) revision: u64,
    pub(crate) count: u32,
    pub(crate) unread: u32,
    pub(crate) stale: bool,
    pub(crate) action: Option<ActionOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BusyReason {
    Refreshing,
    RunningAction,
}

impl std::fmt::Display for BusyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Refreshing => "the feed is already refreshing",
            Self::RunningAction => "an item action is already running",
        })
    }
}

struct Entry {
    stored: StoredFeed,
    /// Advances with the visible part of `stored`.
    revision: u64,
    busy: Option<TaskKind>,
    /// A refresh was started for this entry by this process without a request.
    auto_refresh_tried: bool,
    last_action: Option<FeedActionResult>,
}

#[derive(Default)]
pub(crate) struct FeedManager {
    revision: u64,
    entries: BTreeMap<String, Entry>,
}

impl FeedManager {
    pub(crate) fn new() -> Self {
        Self {
            revision: 1,
            entries: BTreeMap::new(),
        }
    }

    /// Advances on every change a client could see, including a task starting.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    fn entry(&mut self, job_id: &str, state_dir: &Path) -> &mut Entry {
        let revision = self.revision;
        self.entries
            .entry(job_id.to_owned())
            .or_insert_with(|| Entry {
                stored: load(&store_path(state_dir)),
                revision,
                busy: None,
                auto_refresh_tried: false,
                last_action: None,
            })
    }

    #[cfg(test)]
    pub(crate) fn is_busy(&self, job_id: &str) -> bool {
        self.entries
            .get(job_id)
            .is_some_and(|entry| entry.busy.is_some())
    }

    /// Mark a task started. Only one task runs per feed at a time.
    pub(crate) fn begin(
        &mut self,
        job_id: &str,
        state_dir: &Path,
        kind: TaskKind,
    ) -> Result<(), BusyReason> {
        let entry = self.entry(job_id, state_dir);
        match entry.busy {
            Some(TaskKind::Refresh) => return Err(BusyReason::Refreshing),
            Some(TaskKind::Action) => return Err(BusyReason::RunningAction),
            None => {}
        }
        entry.busy = Some(kind);
        self.revision += 1;
        Ok(())
    }

    /// The task could not even start (for example no worker thread).
    pub(crate) fn abandon(&mut self, job_id: &str) {
        if let Some(entry) = self.entries.get_mut(job_id) {
            if entry.busy.take().is_some() {
                self.revision += 1;
            }
        }
    }

    /// True once per process for a feed that has never been read, so a new
    /// job's feed fills in without waiting for its first scheduled run.
    pub(crate) fn wants_first_refresh(&mut self, job_id: &str, state_dir: &Path) -> bool {
        let entry = self.entry(job_id, state_dir);
        if entry.auto_refresh_tried
            || entry.stored.fetched_unix_ms.is_some()
            || entry.busy.is_some()
        {
            return false;
        }
        entry.auto_refresh_tried = true;
        true
    }

    /// Record a finished task: update the stored feed, persist it, and say
    /// what changed.
    pub(crate) fn finish(&mut self, result: TaskResult, state_dir: &Path) -> Finished {
        let TaskResult {
            job_id,
            action,
            refresh,
            finished_unix_ms,
        } = result;
        self.revision += 1;
        let revision = self.revision;
        let entry = self.entry(&job_id, state_dir);
        entry.busy = None;
        if let Some(action) = &action {
            entry.last_action = Some(FeedActionResult {
                item_id: action.item_id.clone(),
                key: action.key.clone(),
                ok: action.ok,
                summary: action.summary.clone(),
                finished_unix_ms,
            });
        }
        let mut next = entry.stored.clone();
        next.version = STORE_VERSION;
        match refresh {
            RefreshOutcome::Feed(parsed) => {
                next.fetched_unix_ms = Some(finished_unix_ms);
                next.title = parsed.title;
                next.items = parsed.items;
                next.stale = false;
                next.error = None;
                next.diagnostics = parsed.diagnostics;
            }
            RefreshOutcome::Failed(error) => {
                next.stale = true;
                next.error = Some(error);
            }
        }
        let changed = visible_part(&next) != visible_part(&entry.stored);
        let path = store_path(state_dir);
        entry.stored = next;
        if changed {
            entry.revision = revision;
        }
        if let Err(err) = save(&path, &entry.stored) {
            tracing::warn!(job = %job_id, path = %path.display(), err = %err, "failed to save feed");
        }
        Finished {
            changed,
            revision: entry.revision,
            count: entry.stored.items.len().try_into().unwrap_or(u32::MAX),
            unread: entry.stored.unread(),
            stale: entry.stored.stale && entry.stored.fetched_unix_ms.is_some(),
            action,
        }
    }

    /// The summary of one feed. `feed_title` is the job file's `feed.title`.
    pub(crate) fn summary(
        &mut self,
        job_id: &str,
        state_dir: &Path,
        feed_title: Option<&str>,
        job_title: &str,
    ) -> FeedSummary {
        let entry = self.entry(job_id, state_dir);
        let stored = &entry.stored;
        let loaded = stored.fetched_unix_ms.is_some();
        FeedSummary {
            job_id: job_id.to_owned(),
            title: feed_title
                .map(str::to_owned)
                .or_else(|| stored.title.clone())
                .unwrap_or_else(|| job_title.to_owned()),
            count: stored.items.len().try_into().unwrap_or(u32::MAX),
            unread: stored.unread(),
            revision: entry.revision,
            stale: stored.stale && loaded,
            loaded,
            fetched_unix_ms: stored.fetched_unix_ms,
            error: stored.error.clone(),
            refreshing: entry.busy.is_some(),
            diagnostics: stored.diagnostics.clone(),
            last_action: entry.last_action.clone(),
        }
    }

    /// The items for clients: titles and keys of actions, never their commands.
    pub(crate) fn items(
        &mut self,
        job_id: &str,
        state_dir: &Path,
        limit: Option<usize>,
    ) -> Vec<FeedItemInfo> {
        let entry = self.entry(job_id, state_dir);
        entry
            .stored
            .items
            .iter()
            .take(limit.unwrap_or(usize::MAX))
            .map(|item| FeedItemInfo {
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
                actions: item
                    .actions
                    .iter()
                    .map(|action| FeedActionInfo {
                        key: action.key.clone(),
                        title: action.title.clone(),
                    })
                    .collect(),
            })
            .collect()
    }

    /// The argv of an item's action, from the stored feed.
    pub(crate) fn action_command(
        &mut self,
        job_id: &str,
        state_dir: &Path,
        item_id: &str,
        key: &str,
    ) -> Result<Vec<String>, ActionLookupError> {
        let entry = self.entry(job_id, state_dir);
        let item = entry
            .stored
            .items
            .iter()
            .find(|item| item.id == item_id)
            .ok_or(ActionLookupError::NoItem)?;
        item.actions
            .iter()
            .find(|action| action.key == key)
            .map(|action| action.command.clone())
            .ok_or(ActionLookupError::NoAction)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionLookupError {
    NoItem,
    NoAction,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::format::parse_feed;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "flockr-feed-store-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn feed(text: &str) -> RefreshOutcome {
        RefreshOutcome::Feed(parse_feed(text).unwrap())
    }

    fn result(outcome: RefreshOutcome, at: u64) -> TaskResult {
        TaskResult {
            job_id: "sync".into(),
            action: None,
            refresh: outcome,
            finished_unix_ms: at,
        }
    }

    const TWO_ITEMS: &str = r#"{"version":1,"title":"Today","items":[
        {"id":"a","title":"A","state":"unread","actions":[{"key":"d","title":"done","command":["mark","a","done"]}]},
        {"id":"b","title":"B","state":"read"}]}"#;

    #[test]
    fn a_refresh_stores_the_feed_and_counts_unread() {
        let dir = temp_dir("store");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        assert!(manager.summary("sync", &dir, None, "Sync").refreshing);
        let finished = manager.finish(result(feed(TWO_ITEMS), 1_000), &dir);
        assert!(finished.changed);
        assert_eq!(
            (finished.count, finished.unread, finished.stale),
            (2, 1, false)
        );
        let summary = manager.summary("sync", &dir, None, "Sync");
        assert_eq!(summary.title, "Today");
        assert!(summary.loaded && !summary.refreshing && !summary.stale);
        assert_eq!(summary.fetched_unix_ms, Some(1_000));
        assert!(dir.join("feed.json").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn titles_prefer_the_job_file_then_the_feed_then_the_job() {
        let dir = temp_dir("title");
        let mut manager = FeedManager::new();
        assert_eq!(
            manager.summary("sync", &dir, None, "Job title").title,
            "Job title"
        );
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1), &dir);
        assert_eq!(
            manager.summary("sync", &dir, None, "Job title").title,
            "Today"
        );
        assert_eq!(
            manager
                .summary("sync", &dir, Some("From job.toml"), "Job title")
                .title,
            "From job.toml"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_items_and_marks_them_stale() {
        let dir = temp_dir("stale");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1_000), &dir);
        let good_revision = manager.summary("sync", &dir, None, "S").revision;

        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        let finished = manager.finish(
            result(RefreshOutcome::Failed("exit 1: boom".into()), 2_000),
            &dir,
        );
        assert!(finished.changed && finished.stale);
        let summary = manager.summary("sync", &dir, None, "S");
        assert!(summary.stale);
        assert_eq!(summary.error.as_deref(), Some("exit 1: boom"));
        assert_eq!(summary.count, 2, "the last good items are kept");
        assert_eq!(summary.fetched_unix_ms, Some(1_000));
        assert!(summary.revision > good_revision);
        assert_eq!(manager.items("sync", &dir, None).len(), 2);

        // The same failure again changes nothing a client could see.
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        let again = manager.finish(
            result(RefreshOutcome::Failed("exit 1: boom".into()), 3_000),
            &dir,
        );
        assert!(!again.changed);
        assert_eq!(
            manager.summary("sync", &dir, None, "S").revision,
            summary.revision
        );

        // A good refresh clears the staleness.
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        let recovered = manager.finish(result(feed(TWO_ITEMS), 4_000), &dir);
        assert!(recovered.changed && !recovered.stale);
        let summary = manager.summary("sync", &dir, None, "S");
        assert!(!summary.stale && summary.error.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_feed_that_never_loaded_shows_the_error_without_being_stale() {
        let dir = temp_dir("never");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(
            result(RefreshOutcome::Failed("not valid JSON".into()), 1_000),
            &dir,
        );
        let summary = manager.summary("sync", &dir, None, "S");
        assert!(!summary.loaded && !summary.stale);
        assert_eq!(summary.error.as_deref(), Some("not valid JSON"));
        assert_eq!(summary.count, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_identical_refresh_does_not_advance_the_feed_revision() {
        let dir = temp_dir("same");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1_000), &dir);
        let first = manager.summary("sync", &dir, None, "S").revision;
        let global = manager.revision();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        let finished = manager.finish(result(feed(TWO_ITEMS), 2_000), &dir);
        assert!(!finished.changed);
        assert_eq!(manager.summary("sync", &dir, None, "S").revision, first);
        assert!(manager.revision() > global, "the list revision still moves");
        assert_eq!(
            manager.summary("sync", &dir, None, "S").fetched_unix_ms,
            Some(2_000)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_feed_survives_a_restart() {
        let dir = temp_dir("restart");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1_000), &dir);
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(
            result(RefreshOutcome::Failed("later failure".into()), 2_000),
            &dir,
        );
        drop(manager);

        let mut restarted = FeedManager::new();
        let summary = restarted.summary("sync", &dir, None, "S");
        assert_eq!((summary.count, summary.unread), (2, 1));
        assert!(summary.stale);
        assert_eq!(summary.error.as_deref(), Some("later failure"));
        assert!(
            !restarted.wants_first_refresh("sync", &dir),
            "already has data"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_store_file_starts_fresh() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join("feed.json"), "{ not json").unwrap();
        let mut manager = FeedManager::new();
        let summary = manager.summary("sync", &dir, None, "S");
        assert!(!summary.loaded);
        assert_eq!(summary.count, 0);
        assert!(manager.wants_first_refresh("sync", &dir));
        assert!(
            !manager.wants_first_refresh("sync", &dir),
            "once per process"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_one_task_runs_per_feed() {
        let dir = temp_dir("busy");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        assert_eq!(
            manager.begin("sync", &dir, TaskKind::Action),
            Err(BusyReason::Refreshing)
        );
        assert_eq!(
            manager.begin("sync", &dir, TaskKind::Refresh),
            Err(BusyReason::Refreshing)
        );
        assert!(manager.is_busy("sync"));
        manager.finish(result(feed(TWO_ITEMS), 1), &dir);
        manager.begin("sync", &dir, TaskKind::Action).unwrap();
        assert_eq!(
            manager.begin("sync", &dir, TaskKind::Refresh),
            Err(BusyReason::RunningAction)
        );
        manager.abandon("sync");
        assert!(!manager.is_busy("sync"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn actions_are_looked_up_in_the_stored_feed_and_commands_stay_private() {
        let dir = temp_dir("action");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1), &dir);
        assert_eq!(
            manager.action_command("sync", &dir, "a", "d").unwrap(),
            ["mark", "a", "done"]
        );
        assert_eq!(
            manager.action_command("sync", &dir, "a", "x"),
            Err(ActionLookupError::NoAction)
        );
        assert_eq!(
            manager.action_command("sync", &dir, "zz", "d"),
            Err(ActionLookupError::NoItem)
        );
        let items = manager.items("sync", &dir, None);
        assert_eq!(
            items[0].actions,
            [FeedActionInfo {
                key: "d".into(),
                title: "done".into()
            }]
        );
        assert_eq!(manager.items("sync", &dir, Some(1)).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_action_outcome_is_recorded_even_when_the_feed_is_unchanged() {
        let dir = temp_dir("outcome");
        let mut manager = FeedManager::new();
        manager.begin("sync", &dir, TaskKind::Refresh).unwrap();
        manager.finish(result(feed(TWO_ITEMS), 1), &dir);
        manager.begin("sync", &dir, TaskKind::Action).unwrap();
        let finished = manager.finish(
            TaskResult {
                job_id: "sync".into(),
                action: Some(ActionOutcome {
                    item_id: "a".into(),
                    key: "d".into(),
                    ok: false,
                    summary: "exit 2".into(),
                }),
                refresh: feed(TWO_ITEMS),
                finished_unix_ms: 5,
            },
            &dir,
        );
        assert!(!finished.changed);
        assert_eq!(finished.action.as_ref().map(|a| a.ok), Some(false));
        let last = manager
            .summary("sync", &dir, None, "S")
            .last_action
            .unwrap();
        assert_eq!(
            (last.item_id.as_str(), last.key.as_str(), last.ok),
            ("a", "d", false)
        );
        assert_eq!(last.summary, "exit 2");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
