//! App-side glue for job feeds: starting feed tasks on worker threads,
//! recording their results, and the `feed.*` socket API. The format, the
//! stored feeds and process execution live in `crate::feed`.

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, FeedGetParams, FeedListParams, FeedRunActionParams,
    FeedSummary, FeedTarget, ResponseResult,
};
use crate::feed::manager::{ActionLookupError, BusyReason, TaskKind, TaskResult};
use crate::feed::runner::{ActionPlan, TaskPlan};
use crate::jobs::definition::JobDef;

use super::responses;
use crate::app::App;

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
}
