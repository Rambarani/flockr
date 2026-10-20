//! App-side glue for job feeds: starting feed tasks on worker threads,
//! recording their results, and the `feed.*` socket API. The format, the
//! stored feeds and process execution live in `crate::feed`.

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, FeedGetParams, FeedListParams, FeedRunActionParams,
    FeedSummary, FeedTarget, FeedWorkParams, ResponseResult,
};
use crate::feed::manager::{ActionLookupError, BusyReason, TaskKind, TaskResult};
use crate::feed::runner::{ActionPlan, TaskPlan};
use crate::jobs::definition::JobDef;

use super::decisions::{AgentTabErrorCodes, DraftSource};
use super::responses;
use crate::app::App;
use crate::jobs::{feed_work, handoff, now_unix_ms};

/// Why a feed task did not start.
#[derive(Debug)]
pub(crate) enum StartError {
    Busy(BusyReason),
    Spawn(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(reason) => reason.fmt(f),
            Self::Spawn(err) => write!(f, "could not start the feed worker thread: {err}"),
        }
    }
}

impl App {
    /// Start a feed refresh, or an item action followed by a refresh, on a
    /// worker thread. At most one task runs per feed.
    pub(crate) fn start_feed_task(
        &mut self,
        job: JobDef,
        action: Option<ActionPlan>,
    ) -> Result<(), StartError> {
        let state_dir = self.jobs.state_dir_for(&job.id);
        let kind = if action.is_some() {
            TaskKind::Action
        } else {
            TaskKind::Refresh
        };
        self.feeds
            .begin(&job.id, &state_dir, kind)
            .map_err(StartError::Busy)?;
        let job_id = job.id.clone();
        let cache = self.jobs.cache();
        let event_tx = self.event_tx.clone();
        let spawned = crate::thread_spawn::spawn_named_with(
            "flockr-feed-run",
            TaskPlan {
                job,
                state_dir,
                action,
            },
            move |plan| {
                let result = crate::feed::runner::run_task(&plan, &cache);
                let _ = event_tx
                    .blocking_send(crate::events::AppEvent::FeedTaskFinished(Box::new(result)));
            },
        );
        if let Err((_, err)) = spawned {
            tracing::warn!(job = %job_id, err = %err, "failed to spawn feed worker thread");
            self.feeds.abandon(&job_id);
            return Err(StartError::Spawn(err.to_string()));
        }
        Ok(())
    }

    /// Refresh a job's feed after a successful run, if it has one. Returns
    /// whether a refresh was started (its result arrives as a feed task).
    pub(crate) fn refresh_feed_after_run(&mut self, job_id: &str) -> bool {
        let Some(job) = self
            .jobs
            .job_def(job_id)
            .filter(|job| job.feed.is_some())
            .cloned()
        else {
            return false;
        };
        match self.start_feed_task(job, None) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(job = %job_id, err = %err, "feed refresh after run skipped");
                false
            }
        }
    }

    /// Fill in feeds that have never been read, once per process, so a new
    /// job's feed shows up without waiting for its first scheduled run.
    pub(crate) fn start_first_feed_refreshes(&mut self) {
        for feed_job in self.jobs.feed_jobs() {
            if !feed_job.enabled || feed_job.running {
                continue;
            }
            let state_dir = self.jobs.state_dir_for(&feed_job.job_id);
            if !self.feeds.wants_first_refresh(&feed_job.job_id, &state_dir) {
                continue;
            }
            self.refresh_feed_after_run(&feed_job.job_id);
        }
    }

    /// Record a finished feed task and publish `feed.updated`.
    pub(crate) fn finish_feed_task(&mut self, result: TaskResult) -> bool {
        let job_id = result.job_id.clone();
        let state_dir = self.jobs.state_dir_for(&job_id);
        let finished = self.feeds.finish(result, &state_dir);
        if let Some(pending) = self.pending_handoffs.remove(&job_id) {
            self.evaluate_handoff(pending);
        }
        if finished.changed || finished.action.is_some() {
            let title = self.feed_summary(&job_id).map(|summary| summary.title);
            self.emit_event(EventEnvelope {
                event: EventKind::FeedUpdated,
                data: EventData::FeedUpdated {
                    title: title.unwrap_or_else(|| job_id.clone()),
                    job_id,
                    revision: finished.revision,
                    count: finished.count,
                    unread: finished.unread,
                    stale: finished.stale,
                },
            });
        }
        true
    }

    fn feed_summary(&mut self, job_id: &str) -> Option<FeedSummary> {
        let job = self.jobs.job_def(job_id)?;
        let feed_title = job.feed.as_ref()?.title.clone();
        let job_title = job.title.clone();
        let state_dir = self.jobs.state_dir_for(job_id);
        Some(
            self.feeds
                .summary(job_id, &state_dir, feed_title.as_deref(), &job_title),
        )
    }

    /// A valid job that declares a feed, or the error response.
    fn feed_job(&mut self, id: &str, job_id: &str) -> Result<JobDef, String> {
        self.refresh_feed_jobs();
        if !self.jobs.has_job(job_id) {
            return Err(responses::encode_error(
                id.to_owned(),
                "job_not_found",
                format!("no job named {job_id}"),
            ));
        }
        let Some(job) = self.jobs.job_def(job_id) else {
            return Err(responses::encode_error(
                id.to_owned(),
                "job_invalid",
                format!("job {job_id} has an invalid job file; see `flockr job status {job_id}`"),
            ));
        };
        if job.feed.is_none() {
            return Err(responses::encode_error(
                id.to_owned(),
                "feed_not_found",
                format!("job {job_id} has no feed; add a feed table to its job.toml"),
            ));
        }
        Ok(job.clone())
    }

    fn refresh_feed_jobs(&mut self) {
        // Same hot-reload as the job API: a file saved a moment ago is visible.
        self.refresh_jobs_for_api();
    }

    pub(super) fn handle_feed_list(&mut self, id: String, params: FeedListParams) -> String {
        self.refresh_feed_jobs();
        // Both counters only grow, so their sum changes whenever either does.
        let revision = self.feeds.revision() + self.jobs.revision();
        let unchanged = params.since_revision == Some(revision);
        let feeds = if unchanged {
            Vec::new()
        } else {
            self.jobs
                .feed_jobs()
                .into_iter()
                .map(|job| {
                    let state_dir = self.jobs.state_dir_for(&job.job_id);
                    self.feeds.summary(
                        &job.job_id,
                        &state_dir,
                        job.feed_title.as_deref(),
                        &job.job_title,
                    )
                })
                .collect()
        };
        responses::encode_success(
            id,
            ResponseResult::FeedList {
                revision,
                unchanged,
                feeds,
            },
        )
    }

    pub(super) fn handle_feed_get(&mut self, id: String, params: FeedGetParams) -> String {
        if let Err(response) = self.feed_job(&id, &params.job_id) {
            return response;
        }
        let Some(feed) = self.feed_summary(&params.job_id) else {
            return responses::encode_error(id, "feed_not_found", "no such feed".to_owned());
        };
        let unchanged = params.since_revision == Some(feed.revision);
        let items = if unchanged {
            Vec::new()
        } else {
            let state_dir = self.jobs.state_dir_for(&params.job_id);
            self.feeds.items(
                &params.job_id,
                &state_dir,
                params.limit.map(|limit| limit as usize),
            )
        };
        responses::encode_success(
            id,
            ResponseResult::FeedDetail {
                feed,
                unchanged,
                items,
            },
        )
    }

    pub(super) fn handle_feed_refresh(&mut self, id: String, params: FeedTarget) -> String {
        let job = match self.feed_job(&id, &params.job_id) {
            Ok(job) => job,
            Err(response) => return response,
        };
        let refused = |reason: String| {
            responses::encode_success(
                id.clone(),
                ResponseResult::FeedRefreshRequested {
                    job_id: params.job_id.clone(),
                    started: false,
                    reason: Some(reason),
                },
            )
        };
        if self.jobs.is_running(&job.id) {
            return refused("the job is running; its feed refreshes when it finishes".into());
        }
        match self.start_feed_task(job, None) {
            Ok(()) => responses::encode_success(
                id,
                ResponseResult::FeedRefreshRequested {
                    job_id: params.job_id,
                    started: true,
                    reason: None,
                },
            ),
            Err(StartError::Busy(reason)) => refused(reason.to_string()),
            Err(err @ StartError::Spawn(_)) => {
                responses::encode_error(id, "feed_refresh_failed", err.to_string())
            }
        }
    }

    pub(super) fn handle_feed_run_action(
        &mut self,
        id: String,
        params: FeedRunActionParams,
    ) -> String {
        let job = match self.feed_job(&id, &params.job_id) {
            Ok(job) => job,
            Err(response) => return response,
        };
        let state_dir = self.jobs.state_dir_for(&params.job_id);
        let command = match self.feeds.action_command(
            &params.job_id,
            &state_dir,
            &params.item_id,
            &params.key,
        ) {
            Ok(command) => command,
            Err(ActionLookupError::NoItem) => {
                return responses::encode_error(
                    id,
                    "feed_item_not_found",
                    format!(
                        "feed {} has no item {:?}; the feed may have changed",
                        params.job_id, params.item_id
                    ),
                )
            }
            Err(ActionLookupError::NoAction) => {
                return responses::encode_error(
                    id,
                    "feed_action_not_found",
                    format!(
                        "item {:?} has no action with key {:?}",
                        params.item_id, params.key
                    ),
                )
            }
        };
        let started = self.start_feed_task(
            job,
            Some(ActionPlan {
                item_id: params.item_id.clone(),
                key: params.key.clone(),
                command,
            }),
        );
        match started {
            Ok(()) => responses::encode_success(
                id,
                ResponseResult::FeedActionRequested {
                    job_id: params.job_id,
                    item_id: params.item_id,
                    key: params.key,
                    started: true,
                    reason: None,
                },
            ),
            Err(StartError::Busy(reason)) => responses::encode_success(
                id,
                ResponseResult::FeedActionRequested {
                    job_id: params.job_id,
                    item_id: params.item_id,
                    key: params.key,
                    started: false,
                    reason: Some(reason.to_string()),
                },
            ),
            Err(err @ StartError::Spawn(_)) => {
                responses::encode_error(id, "feed_action_failed", err.to_string())
            }
        }
    }
}

impl App {
    /// `feed.work`: render a prompt for the chosen items and start an agent
    /// in a new tab, through the same code as a decision's Start. Nothing is
    /// recorded in the feed; a failure leaves everything as it was.
    pub(super) fn handle_feed_work(&mut self, id: String, params: FeedWorkParams) -> String {
        let job = match self.feed_job(&id, &params.job_id) {
            Ok(job) => job,
            Err(response) => return response,
        };
        if params.item_ids.is_empty() {
            return responses::encode_error(
                id,
                "feed_work_no_items",
                "feed.work needs at least one item id".to_owned(),
            );
        }
        let state_dir = self.jobs.state_dir_for(&params.job_id);
        let stored = self.feeds.items(&params.job_id, &state_dir, None);
        let mut items = Vec::new();
        let mut skipped = Vec::new();
        for item_id in &params.item_ids {
            if items
                .iter()
                .any(|item: &crate::api::schema::FeedItemInfo| item.id == *item_id)
                || skipped.contains(item_id)
            {
                continue;
            }
            match stored.iter().find(|item| item.id == *item_id) {
                Some(item) => items.push(item.clone()),
                None => skipped.push(item_id.clone()),
            }
        }
        if items.is_empty() {
            return responses::encode_error(
                id,
                "feed_item_not_found",
                format!(
                    "feed {} has none of the requested items; the feed may have changed",
                    params.job_id
                ),
            );
        }
        let feed_title = self
            .feed_summary(&params.job_id)
            .map(|summary| summary.title)
            .unwrap_or_else(|| job.title.clone());
        let now_ms = now_unix_ms();
        let date = handoff::local_date(now_ms);
        let mut plan = match feed_work::plan(&feed_work::WorkInput {
            job: &job,
            feed_title: &feed_title,
            items: &items,
            date: &date,
        }) {
            Ok(plan) => plan,
            Err(message) => return responses::encode_error(id, "feed_work_invalid", message),
        };
        let default_cwd = feed_work::default_cwd(&job);
        let cwd = match plan.cwd.take() {
            Some(cwd) if cwd.is_dir() => cwd,
            Some(cwd) => {
                plan.warnings.push(format!(
                    "the working directory {} does not exist; used {}",
                    cwd.display(),
                    default_cwd.display()
                ));
                default_cwd
            }
            None => default_cwd,
        };
        if !cwd.is_dir() {
            return responses::encode_error(
                id,
                "feed_work_cwd_missing",
                format!("the working directory {} does not exist", cwd.display()),
            );
        }
        let prompt_path = match write_work_prompt(&state_dir, now_ms, &plan.prompt) {
            Ok(path) => path,
            Err(err) => {
                return responses::encode_error(
                    id,
                    "feed_work_prompt_failed",
                    format!("cannot write the prompt: {err}"),
                )
            }
        };
        let launch = match handoff::agent_launch(
            &plan.agent_command,
            Some(&prompt_path),
            plan.submit,
            plan.prefill,
        ) {
            Ok(launch) => launch,
            Err(message) => return responses::encode_error(id, "feed_work_invalid", message),
        };
        let program = handoff::agent_program(&plan.agent_command);
        let agent = program.unwrap_or("agent").to_owned();
        let drafted = launch.draft.is_some();
        let draft = launch.draft.map(|prefill| DraftSource {
            prefill,
            prompt_file: prompt_path.clone(),
            known_agent: program
                .is_some_and(|program| crate::detect::parse_agent_label(program).is_some()),
        });
        let label = (!plan.tab_title.is_empty()).then(|| plan.tab_title.clone());
        match self.open_agent_tab(
            &params.job_id,
            &cwd,
            launch.command,
            draft,
            label,
            AgentTabErrorCodes {
                tab_failed: "feed_work_tab_failed",
                launch_failed: "feed_work_launch_failed",
            },
        ) {
            Ok((tab_id, pane_id)) => responses::encode_success(
                id,
                ResponseResult::FeedWorkStarted {
                    job_id: params.job_id,
                    count: u32::try_from(items.len().min(feed_work::MAX_WORK_ITEMS))
                        .unwrap_or(u32::MAX),
                    agent,
                    tab_id,
                    pane_id,
                    tab_title: plan.tab_title,
                    prompt_file: prompt_path.display().to_string(),
                    draft: drafted,
                    skipped,
                    warnings: plan.warnings,
                },
            ),
            Err((code, message)) => responses::encode_error(id, code, message),
        }
    }
}

/// Write the rendered prompt to `<state dir>/work/<unix ms>.md`; a clash with
/// an earlier file in the same millisecond takes the next free number.
fn write_work_prompt(
    state_dir: &std::path::Path,
    now_ms: u64,
    prompt: &str,
) -> std::io::Result<std::path::PathBuf> {
    let dir = state_dir.join("work");
    let mut number = now_ms;
    while dir.join(format!("{number}.md")).exists() {
        number += 1;
    }
    handoff::write_numbered_prompt(&dir, number, prompt)
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::api::schema::{
        ErrorResponse, FeedItemState, JobTarget, Method, Request, SuccessResponse,
    };
    use crate::jobs::env::LoginEnvCache;
    use crate::jobs::{JobsManager, JobsPaths};

    struct Fixture {
        root: PathBuf,
        app: App,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "flockr-app-feed-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(root.join("config")).unwrap();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                crate::api::EventHub::default(),
            );
            app.jobs = JobsManager::new(
                JobsPaths {
                    config_dir: root.join("config"),
                    state_root: root.join("state"),
                },
                Arc::new(LoginEnvCache::new(Box::new(|| {
                    Ok(vec![("PATH".into(), "/usr/bin:/bin".into())])
                }))),
                true,
            );
            Self { root, app }
        }

        fn write_job(&self, id: &str, body: &str) {
            let dir = self.root.join("config").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("job.toml"), body).unwrap();
        }

        fn call(&mut self, method: Method) -> String {
            self.app.handle_api_request(Request {
                id: "test".into(),
                method,
            })
        }

        fn ok(&mut self, method: Method) -> ResponseResult {
            let response = self.call(method);
            serde_json::from_str::<SuccessResponse>(&response)
                .unwrap_or_else(|err| panic!("not a success: {response}: {err}"))
                .result
        }

        fn err(&mut self, method: Method) -> String {
            let response = self.call(method);
            serde_json::from_str::<ErrorResponse>(&response)
                .unwrap_or_else(|err| panic!("not an error: {response}: {err}"))
                .error
                .code
        }

        /// Pump app events until the feed's task has been recorded.
        fn wait_for_feed(&mut self, job_id: &str) {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                self.app.drain_all_internal_events();
                if !self.app.feeds.is_busy(job_id) && !self.app.jobs.is_running(job_id) {
                    return;
                }
                assert!(Instant::now() < deadline, "feed task did not finish");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn summary(&mut self, job_id: &str) -> FeedSummary {
            let ResponseResult::FeedDetail { feed, .. } = self.ok(Method::FeedGet(FeedGetParams {
                job_id: job_id.into(),
                since_revision: None,
                limit: Some(0),
            })) else {
                panic!("expected feed detail");
            };
            feed
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn quote(path: &Path) -> String {
        path.display().to_string()
    }

    /// A job whose feed command prints a fixed feed; `body` is shell text.
    fn feed_job(feed_script: &str) -> String {
        format!(
            "title = \"Sync\"\ncommand = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\nnotify = \"never\"\n\
             feed = {{ command = [\"sh\", \"-c\", {feed_script:?}] }}\n"
        )
    }

    const FEED: &str = r#"{"version":1,"title":"Today","items":[{"id":"a","title":"A","state":"unread","actions":[{"key":"d","title":"done","command":["true"]}]},{"id":"b","title":"B"}]}"#;

    fn target(id: &str) -> FeedTarget {
        FeedTarget { job_id: id.into() }
    }

    #[test]
    fn unknown_jobs_and_jobs_without_a_feed_are_errors() {
        let mut fx = Fixture::new("errors");
        fx.write_job(
            "plain",
            "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\n",
        );
        fx.write_job("broken", "command = []\n");
        assert_eq!(fx.err(Method::FeedRefresh(target("nope"))), "job_not_found");
        assert_eq!(
            fx.err(Method::FeedRefresh(target("plain"))),
            "feed_not_found"
        );
        assert_eq!(fx.err(Method::FeedRefresh(target("broken"))), "job_invalid");
        assert_eq!(
            fx.err(Method::FeedGet(FeedGetParams {
                job_id: "plain".into(),
                since_revision: None,
                limit: None
            })),
            "feed_not_found"
        );
        let ResponseResult::FeedList { feeds, .. } = fx.ok(Method::FeedList(Default::default()))
        else {
            panic!("expected a feed list");
        };
        assert!(feeds.is_empty(), "only jobs with a feed are listed");
    }

    #[test]
    fn feed_get_carries_the_body_and_fields_and_omits_them_when_absent() {
        const DETAILED: &str = r#"{"version":1,"items":[{"id":"a","title":"A","body":"hello\nworld","fields":[{"label":"Source","value":"jira"}]},{"id":"b","title":"B"}]}"#;
        let mut fx = Fixture::new("detail_fields");
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{DETAILED}'")));
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        let ResponseResult::FeedDetail { items, .. } = fx.ok(Method::FeedGet(FeedGetParams {
            job_id: "sync".into(),
            since_revision: None,
            limit: None,
        })) else {
            panic!("expected detail");
        };
        assert_eq!(items[0].body.as_deref(), Some("hello\nworld"));
        assert_eq!(items[0].fields[0].label, "Source");
        let plain = serde_json::to_value(&items[1]).unwrap();
        assert!(plain.get("body").is_none() && plain.get("fields").is_none());
        let rich = serde_json::to_value(&items[0]).unwrap();
        assert_eq!(rich["fields"][0]["value"], "jira");
    }

    #[test]
    fn refresh_reads_the_feed_and_since_revision_answers_unchanged() {
        let mut fx = Fixture::new("refresh");
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{FEED}'")));
        let ResponseResult::FeedList {
            feeds, revision, ..
        } = fx.ok(Method::FeedList(Default::default()))
        else {
            panic!("expected a feed list");
        };
        assert_eq!(feeds.len(), 1);
        assert!(!feeds[0].loaded);

        let ResponseResult::FeedRefreshRequested { started, .. } =
            fx.ok(Method::FeedRefresh(target("sync")))
        else {
            panic!("expected a refresh request");
        };
        assert!(started);
        fx.wait_for_feed("sync");

        let ResponseResult::FeedList {
            feeds,
            revision: after,
            unchanged,
        } = fx.ok(Method::FeedList(Default::default()))
        else {
            panic!("expected a feed list");
        };
        assert!(!unchanged && after > revision);
        let feed = &feeds[0];
        assert_eq!(feed.title, "Today");
        assert_eq!((feed.count, feed.unread), (2, 1));
        assert!(feed.loaded && !feed.stale && !feed.refreshing);

        // The same list revision answers without the feeds.
        let ResponseResult::FeedList {
            unchanged, feeds, ..
        } = fx.ok(Method::FeedList(FeedListParams {
            since_revision: Some(after),
        }))
        else {
            panic!("expected a feed list");
        };
        assert!(unchanged && feeds.is_empty());

        // feed.get: full items, then unchanged for the feed's revision.
        let ResponseResult::FeedDetail {
            feed,
            items,
            unchanged,
        } = fx.ok(Method::FeedGet(FeedGetParams {
            job_id: "sync".into(),
            since_revision: None,
            limit: None,
        }))
        else {
            panic!("expected detail");
        };
        assert!(!unchanged);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].state, FeedItemState::Unread);
        assert_eq!(items[0].actions[0].key, "d");
        let ResponseResult::FeedDetail {
            unchanged,
            items: again,
            feed: same,
        } = fx.ok(Method::FeedGet(FeedGetParams {
            job_id: "sync".into(),
            since_revision: Some(feed.revision),
            limit: None,
        }))
        else {
            panic!("expected detail");
        };
        assert!(unchanged && again.is_empty());
        assert_eq!(same.count, 2, "the summary still comes back");

        // limit trims the items, not the count.
        let ResponseResult::FeedDetail { items, feed, .. } =
            fx.ok(Method::FeedGet(FeedGetParams {
                job_id: "sync".into(),
                since_revision: None,
                limit: Some(1),
            }))
        else {
            panic!("expected detail");
        };
        assert_eq!((items.len(), feed.count), (1, 2));

        // feed.updated reached the event stream once, with the counts.
        let events = fx.app.event_hub.events_after(0);
        let updated: Vec<_> = events
            .iter()
            .filter(|(_, event)| event.event == EventKind::FeedUpdated)
            .collect();
        assert_eq!(updated.len(), 1);
        let EventData::FeedUpdated {
            job_id,
            title,
            count,
            unread,
            stale,
            ..
        } = &updated[0].1.data
        else {
            panic!("wrong payload");
        };
        assert_eq!((job_id.as_str(), title.as_str()), ("sync", "Today"));
        assert_eq!((*count, *unread, *stale), (2, 1, false));
    }

    #[test]
    fn the_job_file_title_wins_over_the_feed_title() {
        let mut fx = Fixture::new("title");
        fx.write_job(
            "sync",
            &format!(
                "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\n\
                 feed = {{ command = [\"sh\", \"-c\", {:?}], title = \"From job.toml\" }}\n",
                format!("printf '%s' '{FEED}'")
            ),
        );
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        assert_eq!(fx.summary("sync").title, "From job.toml");
    }

    #[test]
    fn a_second_refresh_while_one_runs_is_refused() {
        let mut fx = Fixture::new("busy");
        fx.write_job("sync", &feed_job(&format!("sleep 1; printf '%s' '{FEED}'")));
        fx.ok(Method::FeedRefresh(target("sync")));
        let ResponseResult::FeedRefreshRequested {
            started, reason, ..
        } = fx.ok(Method::FeedRefresh(target("sync")))
        else {
            panic!("expected a refresh request");
        };
        assert!(!started);
        assert_eq!(reason.as_deref(), Some("the feed is already refreshing"));
        assert!(fx.summary("sync").refreshing);
        fx.wait_for_feed("sync");
        assert!(!fx.summary("sync").refreshing);
    }

    #[test]
    fn a_successful_job_run_refreshes_its_feed() {
        let mut fx = Fixture::new("after-run");
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{FEED}'")));
        let ResponseResult::JobRunRequested { started, .. } = fx.ok(Method::JobRun(JobTarget {
            job_id: "sync".into(),
        })) else {
            panic!("expected a run request");
        };
        assert!(started);
        // The run finishes, which starts the feed task; pump until both are done.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            fx.app.drain_all_internal_events();
            if fx.summary("sync").loaded {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "feed was not refreshed after the run"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fx.summary("sync").count, 2);
    }

    #[test]
    fn a_failed_job_run_does_not_refresh_the_feed() {
        let mut fx = Fixture::new("failed-run");
        fx.write_job(
            "sync",
            &format!(
                "command = [\"false\"]\nevery = \"1h\"\nenv = \"server\"\nnotify = \"never\"\n\
                 feed = {{ command = [\"sh\", \"-c\", {:?}] }}\n",
                format!("printf '%s' '{FEED}'")
            ),
        );
        fx.ok(Method::JobRun(JobTarget {
            job_id: "sync".into(),
        }));
        let deadline = Instant::now() + Duration::from_secs(15);
        while fx.app.jobs.is_running("sync") {
            fx.app.drain_all_internal_events();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        fx.app.drain_all_internal_events();
        assert!(!fx.app.feeds.is_busy("sync"));
        assert!(!fx.summary("sync").loaded);
    }

    #[test]
    fn a_failing_feed_command_keeps_the_last_good_feed_and_marks_it_stale() {
        let mut fx = Fixture::new("stale");
        let flag = fx.root.join("fail-now");
        fx.write_job(
            "sync",
            &feed_job(&format!(
                "if [ -f {} ]; then echo broken >&2; exit 3; fi; printf '%s' '{FEED}'",
                quote(&flag)
            )),
        );
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        assert!(!fx.summary("sync").stale);

        std::fs::write(&flag, "x").unwrap();
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        let feed = fx.summary("sync");
        assert!(feed.stale);
        assert_eq!(feed.count, 2, "the last good items stay");
        assert_eq!(
            feed.error.as_deref(),
            Some("feed command failed: exit 3: broken")
        );
        let ResponseResult::FeedDetail { items, .. } = fx.ok(Method::FeedGet(FeedGetParams {
            job_id: "sync".into(),
            since_revision: None,
            limit: None,
        })) else {
            panic!("expected detail");
        };
        assert_eq!(items.len(), 2);

        // The failure is persisted: a restarted manager still shows it.
        let mut restarted = crate::feed::FeedManager::new();
        let state_dir = fx.app.jobs.state_dir_for("sync");
        let summary = restarted.summary("sync", &state_dir, None, "Sync");
        assert!(summary.stale);
        assert_eq!(summary.count, 2);

        std::fs::remove_file(&flag).unwrap();
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        assert!(!fx.summary("sync").stale);
    }

    #[test]
    fn running_an_action_executes_its_command_then_refreshes_the_feed() {
        let mut fx = Fixture::new("action");
        let marker = fx.root.join("marker");
        let feed_with_action = format!(
            r#"{{"version":1,"items":[{{"id":"a","title":"A","state":"unread","actions":[{{"key":"d","title":"done","command":["sh","-c","echo \"$FLOCKR_FEED_ITEM_ID:$FLOCKR_FEED_ACTION_KEY\" > {}"]}}]}}]}}"#,
            quote(&marker)
        );
        let script = format!(
            "if [ -f {m} ]; then printf '%s' '{{\"version\":1,\"items\":[]}}'; else printf '%s' '{feed}'; fi",
            m = quote(&marker),
            feed = feed_with_action.replace('\'', "'\\''")
        );
        fx.write_job("sync", &feed_job(&script));
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        assert_eq!(fx.summary("sync").count, 1);

        let ResponseResult::FeedActionRequested { started, .. } =
            fx.ok(Method::FeedRunAction(FeedRunActionParams {
                job_id: "sync".into(),
                item_id: "a".into(),
                key: "d".into(),
            }))
        else {
            panic!("expected an action request");
        };
        assert!(started);
        fx.wait_for_feed("sync");

        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "a:d");
        let feed = fx.summary("sync");
        assert_eq!(feed.count, 0, "the feed refreshed after the action");
        let last = feed.last_action.expect("action outcome recorded");
        assert!(last.ok);
        assert_eq!((last.item_id.as_str(), last.key.as_str()), ("a", "d"));
    }

    #[test]
    fn actions_that_do_not_exist_are_rejected_without_running_anything() {
        let mut fx = Fixture::new("action-missing");
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{FEED}'")));
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        let params = |item: &str, key: &str| {
            Method::FeedRunAction(FeedRunActionParams {
                job_id: "sync".into(),
                item_id: item.into(),
                key: key.into(),
            })
        };
        assert_eq!(fx.err(params("zzz", "d")), "feed_item_not_found");
        assert_eq!(fx.err(params("a", "x")), "feed_action_not_found");
        assert_eq!(fx.err(params("b", "d")), "feed_action_not_found");
        assert!(fx.summary("sync").last_action.is_none());
    }

    #[test]
    fn a_failing_action_is_reported_in_the_summary() {
        let mut fx = Fixture::new("action-fail");
        let feed_text = r#"{"version":1,"items":[{"id":"a","title":"A","actions":[{"key":"d","command":["sh","-c","echo nope >&2; exit 5"]}]}]}"#;
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{feed_text}'")));
        fx.ok(Method::FeedRefresh(target("sync")));
        fx.wait_for_feed("sync");
        fx.ok(Method::FeedRunAction(FeedRunActionParams {
            job_id: "sync".into(),
            item_id: "a".into(),
            key: "d".into(),
        }));
        fx.wait_for_feed("sync");
        let last = fx.summary("sync").last_action.unwrap();
        assert!(!last.ok);
        assert_eq!(last.summary, "exit 5: nope");
    }

    #[test]
    fn a_feed_is_filled_in_once_for_an_enabled_job_that_has_never_loaded() {
        let mut fx = Fixture::new("first");
        fx.write_job("sync", &feed_job(&format!("printf '%s' '{FEED}'")));
        fx.write_job(
            "off",
            &format!(
                "enabled = false\n{}",
                feed_job(&format!("printf '%s' '{FEED}'"))
            ),
        );
        fx.app.run_jobs_tick(Instant::now());
        fx.wait_for_feed("sync");
        assert!(fx.summary("sync").loaded, "enabled job filled in");
        assert!(!fx.summary("off").loaded, "disabled job left alone");
        // Not again within this process.
        let before = fx.app.feeds.revision();
        fx.app.run_jobs_tick(Instant::now());
        assert!(!fx.app.feeds.is_busy("sync"));
        assert_eq!(fx.app.feeds.revision(), before);
    }

    // ---- feed.work -------------------------------------------------------

    use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
    use crate::api::schema::FeedWorkParams;

    const WORK_FEED: &str = r#"{"version":1,"title":"Today","items":[
        {"id":"a","title":"Fix login","badge":"PR-1","url":"https://example.test/a","body":"line one\nline two","state":"unread","fields":[{"label":"Repo","value":"app-one"}]},
        {"id":"b","title":"Add tests","badge":"PR-2","url":"https://example.test/b","fields":[{"label":"Repo","value":"app-two"}]},
        {"id":"c","title":"Third"}]}"#;

    fn work_job(work_dir: &Path, work_table: &str) -> String {
        format!(
            "title = \"Sync\"\ncommand = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\nnotify = \"never\"\ncwd = \"{}\"\n[feed]\ncommand = [\"sh\", \"-c\", {:?}]\n{work_table}",
            work_dir.display(),
            format!("printf '%s' '{WORK_FEED}'")
        )
    }

    fn work_params(ids: &[&str]) -> Method {
        Method::FeedWork(FeedWorkParams {
            job_id: "sync".into(),
            item_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        })
    }

    impl Fixture {
        /// A workspace with a live-shell tab, so a work tab can open.
        fn with_workspace(&mut self) {
            self.app.state.default_shell = exiting_test_command().into();
            self.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
            self.app.state.workspaces = vec![crate::workspace::Workspace::test_new("spaces")];
            self.app.state.active = Some(0);
            self.app.state.selected = 0;
            self.app.state.ensure_test_terminals();
        }

        fn loaded_work_feed(&mut self, work_dir: &Path, work_table: &str) {
            self.write_job("sync", &work_job(work_dir, work_table));
            self.ok(Method::FeedRefresh(target("sync")));
            self.wait_for_feed("sync");
        }

        fn prompt_files(&self) -> Vec<PathBuf> {
            let dir = self.root.join("state").join("sync").join("work");
            let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
                .unwrap_or_default();
            files.sort();
            files
        }
    }

    #[tokio::test]
    async fn work_opens_a_focused_tab_with_the_default_prompt_for_the_chosen_items() {
        let mut fx = Fixture::new("work-default");
        fx.with_workspace();
        let work = fx.root.join("code");
        std::fs::create_dir_all(&work).unwrap();
        fx.app.state.default_shell = "/bin/cat".into();
        fx.loaded_work_feed(&work, "");
        let before = fx.app.state.workspaces[0].tabs.len();

        let ResponseResult::FeedWorkStarted {
            job_id,
            count,
            agent,
            tab_id,
            pane_id,
            tab_title,
            prompt_file,
            draft,
            skipped,
            warnings,
        } = fx.ok(work_params(&["b", "gone", "a", "b"]))
        else {
            panic!("expected a started work request");
        };
        assert_eq!(job_id, "sync");
        assert_eq!(count, 2, "duplicates and unknown ids are not counted");
        assert_eq!(agent, "claude");
        assert!(draft, "feed.work leaves the prompt as a draft by default");
        assert_eq!(skipped, ["gone"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!tab_id.is_empty() && !pane_id.is_empty());
        assert_eq!(tab_title, "Sync · 2 items");

        // A new tab, focused, in the job's cwd, with the title.
        let workspace = &fx.app.state.workspaces[0];
        assert_eq!(workspace.tabs.len(), before + 1);
        assert_eq!(workspace.active_tab, workspace.tabs.len() - 1);
        let tab = workspace.tabs.last().unwrap();
        assert_eq!(tab.custom_name.as_deref(), Some("Sync · 2 items"));
        let terminal_cwd = workspace
            .terminal_id(tab.root_pane)
            .and_then(|terminal_id| fx.app.state.terminals.get(terminal_id))
            .map(|terminal| crate::worktree::canonical_or_original(&terminal.cwd));
        assert_eq!(
            terminal_cwd,
            Some(crate::worktree::canonical_or_original(&work))
        );

        // The prompt is under <state>/work/<ms>.md in the order asked, once each.
        let files = fx.prompt_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].display().to_string(), prompt_file);
        let prompt = std::fs::read_to_string(&files[0]).unwrap();
        assert!(
            prompt.starts_with("Here are 2 items from Today I want to work on:"),
            "{prompt}"
        );
        let b = prompt.find("1. PR-2 — Add tests").expect("b first");
        let a = prompt.find("2. PR-1 — Fix login").expect("a second");
        assert!(b < a);
        assert!(prompt.contains("Link: https://example.test/a"));
        assert!(prompt.contains("     line two"));
        assert!(prompt.contains("Ask before changing code."));
        assert!(!prompt.contains("Third"));
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn work_uses_the_configured_templates_and_agent_command() {
        let mut fx = Fixture::new("work-config");
        fx.with_workspace();
        let work = fx.root.join("code");
        std::fs::create_dir_all(work.join("app-one")).unwrap();
        fx.app.state.default_shell = "/bin/cat".into();
        let code = work.display().to_string();
        let elsewhere = fx.root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        fx.loaded_work_feed(
            &elsewhere,
            &format!(
                "[feed.work]\nagent_command = \"cat {{prompt_file}}\"\ncwd = \"{code}/{{{{item.field.repo}}}}\"\ntab_title = \"{{{{count}}}} for {{{{item.badge}}}}\"\nprompt = \"{{{{count}}}} {{{{job.title}}}} {{{{item.title}}}}\\n{{{{items}}}}\"\n"
            ),
        );
        let ResponseResult::FeedWorkStarted {
            agent,
            tab_title,
            warnings,
            count,
            ..
        } = fx.ok(work_params(&["a", "b"]))
        else {
            panic!("expected a started work request");
        };
        assert_eq!((agent.as_str(), count), ("cat", 2));
        assert_eq!(tab_title, "2 for PR-1");
        assert!(warnings.is_empty(), "{warnings:?}");
        let prompt = std::fs::read_to_string(&fx.prompt_files()[0]).unwrap();
        assert!(prompt.starts_with("2 Sync Fix login\n1. PR-1"), "{prompt}");
        // The tab opened in the first item's repository directory.
        let workspace = &fx.app.state.workspaces[0];
        let tab = workspace.tabs.last().unwrap();
        let terminal_cwd = workspace
            .terminal_id(tab.root_pane)
            .and_then(|terminal_id| fx.app.state.terminals.get(terminal_id))
            .map(|terminal| crate::worktree::canonical_or_original(&terminal.cwd));
        assert_eq!(
            terminal_cwd,
            Some(crate::worktree::canonical_or_original(
                &work.join("app-one")
            ))
        );
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn work_falls_back_when_the_templated_directory_is_missing() {
        let mut fx = Fixture::new("work-fallback");
        fx.with_workspace();
        let default = fx.root.join("default");
        std::fs::create_dir_all(&default).unwrap();
        fx.app.state.default_shell = "/bin/cat".into();
        let table = format!(
            "[feed.work]\ncwd = \"{}/nope/{{{{item.field.repo}}}}\"\n",
            fx.root.display()
        );
        fx.loaded_work_feed(&default, &table);
        let ResponseResult::FeedWorkStarted { warnings, .. } = fx.ok(work_params(&["a"])) else {
            panic!("expected a started work request");
        };
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("does not exist"), "{warnings:?}");
        let workspace = &fx.app.state.workspaces[0];
        let tab = workspace.tabs.last().unwrap();
        let terminal_cwd = workspace
            .terminal_id(tab.root_pane)
            .and_then(|terminal_id| fx.app.state.terminals.get(terminal_id))
            .map(|terminal| crate::worktree::canonical_or_original(&terminal.cwd));
        assert_eq!(
            terminal_cwd,
            Some(crate::worktree::canonical_or_original(&default))
        );
        shutdown_test_runtimes(&mut fx.app);
    }

    #[test]
    fn work_rejects_bad_requests_without_opening_a_tab() {
        let mut fx = Fixture::new("work-errors");
        fx.with_workspace();
        let work = fx.root.join("code");
        std::fs::create_dir_all(&work).unwrap();
        fx.write_job(
            "plain",
            "command = [\"true\"]\nevery = \"1h\"\nenv = \"server\"\n",
        );
        assert_eq!(
            fx.err(Method::FeedWork(FeedWorkParams {
                job_id: "nope".into(),
                item_ids: vec!["a".into()],
            })),
            "job_not_found"
        );
        assert_eq!(
            fx.err(Method::FeedWork(FeedWorkParams {
                job_id: "plain".into(),
                item_ids: vec!["a".into()],
            })),
            "feed_not_found"
        );
        fx.loaded_work_feed(&work, "");
        let before = fx.app.state.workspaces[0].tabs.len();
        assert_eq!(fx.err(work_params(&[])), "feed_work_no_items");
        assert_eq!(fx.err(work_params(&["zzz"])), "feed_item_not_found");

        // A prompt file that cannot be read stops before any tab opens.
        fx.loaded_work_feed(&work, "[feed.work]\nprompt_file = \"missing.md\"\n");
        assert_eq!(fx.err(work_params(&["a"])), "feed_work_invalid");
        assert_eq!(
            fx.app.state.workspaces[0].tabs.len(),
            before,
            "no tab opened"
        );
        assert!(fx.prompt_files().is_empty(), "nothing was written");
    }

    // ---- feed.work drafts --------------------------------------------------

    use super::super::test_support::{
        wait_for_bracketed_paste, wait_for_received, write_stub_shell,
    };
    use crate::app::draft_prompt::{DRAFT_READY_TIMEOUT, DRAFT_UNKNOWN_AGENT_DELAY};

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// Start work on item `a` in a tab whose "shell" records its input in
    /// the returned file. Returns the pane id, the prompt file and that file.
    fn start_stub_work(
        fx: &mut Fixture,
        name: &str,
        bracketed: bool,
        table: &str,
    ) -> (String, PathBuf, PathBuf) {
        fx.with_workspace();
        let work = fx.root.join(name);
        std::fs::create_dir_all(&work).unwrap();
        let (script, out) = write_stub_shell(&work, bracketed);
        fx.app.state.default_shell = script.display().to_string();
        fx.loaded_work_feed(&work, table);
        let ResponseResult::FeedWorkStarted {
            pane_id,
            prompt_file,
            ..
        } = fx.ok(work_params(&["a"]))
        else {
            panic!("expected a started work request");
        };
        (pane_id, PathBuf::from(prompt_file), out)
    }

    #[tokio::test]
    async fn a_draft_starts_the_agent_alone_and_pastes_the_prompt_without_enter() {
        let mut fx = Fixture::new("draft-paste");
        let (pane, prompt_file, out) = start_stub_work(
            &mut fx,
            "paste",
            true,
            "[feed.work]\nagent_command = \"stubagent --flag\"\n",
        );
        let prompt = std::fs::read_to_string(&prompt_file).unwrap();
        assert!(prompt.contains("Fix login"), "{prompt}");

        // Starting only typed the command; the prompt is queued, not sent, and
        // the call returned without waiting for the agent.
        assert_eq!(fx.app.pending_drafts.len(), 1);
        assert!(fx.app.next_draft_deadline().is_some());
        let typed = wait_for_received(&out, |bytes| contains(bytes, b"stubagent --flag"));
        assert!(
            !contains(&typed, b"$(cat"),
            "no prompt argument: {:?}",
            String::from_utf8_lossy(&typed)
        );
        assert!(!contains(&typed, b"Fix login"));

        // Not before the fixed delay for an agent flockr cannot detect.
        wait_for_bracketed_paste(&fx.app, &pane);
        let queued = Instant::now();
        assert!(!fx
            .app
            .service_pending_drafts(queued + Duration::from_millis(200)));
        assert_eq!(fx.app.pending_drafts.len(), 1);

        assert!(fx.app.service_pending_drafts(
            queued + DRAFT_UNKNOWN_AGENT_DELAY + Duration::from_millis(200)
        ));
        assert!(fx.app.pending_drafts.is_empty());
        let expected = format!("\x1b[200~{prompt}\x1b[201~");
        let received = wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        // The paste is the last thing the agent received: no Enter followed it.
        assert!(received.ends_with(expected.as_bytes()), "{received:?}");
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn a_mention_draft_types_one_line_with_the_prompt_file_path() {
        let mut fx = Fixture::new("draft-mention");
        let (pane, prompt_file, out) = start_stub_work(
            &mut fx,
            "mention",
            true,
            "[feed.work]\nagent_command = \"stubagent\"\nprefill = \"mention\"\n",
        );
        wait_for_bracketed_paste(&fx.app, &pane);
        let queued = Instant::now();
        assert!(fx.app.service_pending_drafts(
            queued + DRAFT_UNKNOWN_AGENT_DELAY + Duration::from_millis(200)
        ));
        let expected = format!("\x1b[200~@{} \x1b[201~", prompt_file.display());
        let received = wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        assert!(received.ends_with(expected.as_bytes()), "{received:?}");
        assert!(
            !contains(&received, b"Fix login"),
            "the prompt text stays in the file"
        );
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn a_known_agent_that_never_becomes_ready_gets_the_draft_after_the_timeout() {
        let mut fx = Fixture::new("draft-timeout");
        // `claude` is a known agent, but the stub shell is never detected as one.
        let (pane, prompt_file, out) = start_stub_work(&mut fx, "timeout", true, "");
        let prompt = std::fs::read_to_string(&prompt_file).unwrap();
        wait_for_bracketed_paste(&fx.app, &pane);
        let queued = Instant::now();
        assert!(!fx
            .app
            .service_pending_drafts(queued + DRAFT_READY_TIMEOUT - Duration::from_secs(1)));
        assert_eq!(
            fx.app.pending_drafts.len(),
            1,
            "still waiting for detection"
        );
        assert!(fx
            .app
            .service_pending_drafts(queued + DRAFT_READY_TIMEOUT + Duration::from_millis(200)));
        assert!(fx.app.pending_drafts.is_empty());
        let expected = format!("\x1b[200~{prompt}\x1b[201~");
        let received = wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        assert!(received.ends_with(expected.as_bytes()));
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn without_bracketed_paste_a_multi_line_prompt_falls_back_to_one_line() {
        let mut fx = Fixture::new("draft-plain");
        let (_pane, prompt_file, out) = start_stub_work(
            &mut fx,
            "plain",
            false,
            "[feed.work]\nagent_command = \"stubagent\"\n",
        );
        let queued = Instant::now();
        assert!(fx.app.service_pending_drafts(
            queued + DRAFT_UNKNOWN_AGENT_DELAY + Duration::from_millis(200)
        ));
        let expected = format!("@{} ", prompt_file.display());
        let received = wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        assert!(received.ends_with(expected.as_bytes()), "{received:?}");
        assert!(!contains(&received, b"Fix login"));
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn submit_true_passes_the_prompt_on_the_command_line_and_queues_nothing() {
        let mut fx = Fixture::new("draft-submit");
        let (_pane, prompt_file, out) = start_stub_work(
            &mut fx,
            "submit",
            true,
            "[feed.work]\nagent_command = \"stubagent\"\nsubmit = true\n",
        );
        assert!(fx.app.pending_drafts.is_empty());
        assert!(fx.app.next_draft_deadline().is_none());
        let expected = format!("stubagent \"$(cat '{}')\"", prompt_file.display());
        let received = wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        assert!(!contains(&received, b"\x1b[200~"), "no paste");
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn a_prompt_file_placeholder_keeps_the_explicit_mechanism_whatever_submit_says() {
        let mut fx = Fixture::new("draft-placeholder");
        let (_pane, prompt_file, out) = start_stub_work(
            &mut fx,
            "placeholder",
            true,
            "[feed.work]\nagent_command = \"stubagent --file {prompt_file}\"\nsubmit = false\n",
        );
        assert!(fx.app.pending_drafts.is_empty(), "nothing is pasted");
        let expected = format!("stubagent --file '{}'", prompt_file.display());
        wait_for_received(&out, |bytes| contains(bytes, expected.as_bytes()));
        let ResponseResult::FeedWorkStarted { draft, .. } = fx.ok(work_params(&["a"])) else {
            panic!("expected a started work request");
        };
        assert!(
            !draft,
            "the response says the prompt was not left as a draft"
        );
        shutdown_test_runtimes(&mut fx.app);
    }

    #[tokio::test]
    async fn a_draft_for_a_closed_pane_is_dropped() {
        let mut fx = Fixture::new("draft-closed");
        let (pane, _prompt_file, _out) = start_stub_work(
            &mut fx,
            "closed",
            true,
            "[feed.work]\nagent_command = \"stubagent\"\n",
        );
        assert_eq!(fx.app.pending_drafts.len(), 1);
        fx.ok(Method::PaneClose(crate::api::schema::PaneTarget {
            pane_id: pane,
        }));
        let queued = Instant::now();
        assert!(!fx
            .app
            .service_pending_drafts(queued + DRAFT_UNKNOWN_AGENT_DELAY + Duration::from_secs(1)));
        assert!(fx.app.pending_drafts.is_empty());
        shutdown_test_runtimes(&mut fx.app);
    }
}
