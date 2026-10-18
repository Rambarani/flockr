//! App-side glue for scheduled jobs: the scheduler tick, worker threads,
//! toasts, events and the `job.*` socket API. The registry, scheduling rules
//! and process execution live in `crate::jobs`.

use std::time::{Duration, Instant};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, JobListParams, JobLogsParams, JobRunStatus, JobRunTrigger,
    JobScheduleStore, JobSetEnabledParams, JobSetScheduleParams, JobTarget, ResponseResult,
};
use crate::jobs::manager::Notice;
use crate::jobs::runner::JobRunResult;
use crate::jobs::state::{last_lines, RunRecord};
use crate::jobs::{now_unix_ms, BeginError, PluginJobSource};

use super::plugins::plugin_manifest_available;
use super::responses;
use crate::app::state::{ToastKind, ToastNotification};
use crate::app::App;

/// Longest wait between scheduler passes: bounds how long a new or edited job
/// file takes to be noticed.
const MAX_TICK_INTERVAL: Duration = Duration::from_secs(30);
/// Never spin: a due time in the past is handled by the pass that sees it.
const MIN_TICK_INTERVAL: Duration = Duration::from_millis(250);
/// Retry delay while a queued job toast waits for the toast slot.
const NOTICE_RETRY: Duration = Duration::from_secs(1);
const MAX_PENDING_NOTICES: usize = 16;
/// How long a toast that asks the user something stays up, longer than a
/// status toast.
const ACTIONABLE_TOAST_DURATION: Duration = Duration::from_secs(12);
/// Lines of each stream returned by `job.logs` when the caller sets no limit.
const DEFAULT_LOG_LINES: usize = 2_000;

impl App {
    fn job_plugin_sources(&self) -> Vec<PluginJobSource> {
        let mut sources = self
            .state
            .installed_plugins
            .values()
            .filter(|plugin| plugin.enabled && plugin_manifest_available(plugin))
            .map(|plugin| PluginJobSource {
                plugin_id: plugin.plugin_id.clone(),
                plugin_root: std::path::PathBuf::from(&plugin.plugin_root),
                manifest_path: std::path::PathBuf::from(&plugin.manifest_path),
            })
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        sources
    }

    /// Re-read job files if they changed. Called before answering job API
    /// requests so a file saved a moment ago is already visible.
    pub(super) fn refresh_jobs_for_api(&mut self) {
        self.refresh_jobs(false);
    }

    fn refresh_jobs(&mut self, force: bool) {
        if !self.jobs.is_active() {
            return;
        }
        let plugins = self.job_plugin_sources();
        self.jobs.reload(now_unix_ms(), &plugins, force);
    }

    /// `flockr server reload-config`: re-read every job file now.
    pub(crate) fn reload_jobs_now(&mut self) {
        if !self.jobs.is_active() {
            return;
        }
        self.refresh_jobs(true);
        self.next_jobs_tick = Some(Instant::now());
    }

    /// Build the system jobs (see `crate::jobs::system`) from `[upstream]` and
    /// `[jobs.system.*]` and hand them to the registry. Returns the problems
    /// found in the overrides, for the config diagnostics.
    pub(crate) fn apply_system_jobs(&mut self) -> Vec<String> {
        if !self.jobs.is_active() {
            return Vec::new();
        }
        let exe = crate::platform::launch_executable().unwrap_or_else(|err| {
            tracing::warn!(err = %err, "cannot find the flockr executable for system jobs");
            std::path::PathBuf::from("flockr")
        });
        let built = crate::jobs::system::build(&crate::jobs::system::SystemContext {
            exe,
            upstream: &self.upstream_config,
            jobs: &self.jobs_config,
            background_updates: super::super::auto_updates_enabled(self.policy.background_updates),
        });
        if self.jobs.set_system_jobs(built.jobs) {
            self.next_jobs_tick = Some(Instant::now());
        }
        built.diagnostics
    }

    /// One scheduler pass: pick up edited job files, start due jobs, deliver
    /// queued toasts and arm the timer for the next pass. Returns whether the
    /// frame changed.
    pub(crate) fn run_jobs_tick(&mut self, now: Instant) -> bool {
        if !self.jobs.is_active() {
            self.next_jobs_tick = None;
            return false;
        }
        self.refresh_jobs(false);
        let now_ms = now_unix_ms();
        let mut changed = false;
        for (job_id, trigger) in self.jobs.take_due(now_ms) {
            changed |= self.start_job_run(&job_id, trigger, now_ms).is_ok();
        }
        self.start_first_feed_refreshes();
        self.wake_snoozed_decisions(now_ms);
        changed |= self.deliver_job_notices();
        let mut wait = MAX_TICK_INTERVAL;
        if let Some(due_ms) = self.jobs.next_deadline_ms() {
            wait = wait.min(Duration::from_millis(due_ms.saturating_sub(now_ms)));
        }
        if let Some(wake_ms) = self.decisions.next_snooze_expiry_ms() {
            wait = wait.min(Duration::from_millis(wake_ms.saturating_sub(now_ms)));
        }
        if !self.pending_job_notices.is_empty() {
            wait = wait.min(NOTICE_RETRY);
        }
        self.next_jobs_tick = Some(now + wait.max(MIN_TICK_INTERVAL));
        changed
    }

    /// Mark the run started and hand it to a worker thread. A scheduled
    /// trigger that finds the job running is recorded as skipped inside
    /// `begin_run`.
    fn start_job_run(
        &mut self,
        job_id: &str,
        trigger: JobRunTrigger,
        now_ms: u64,
    ) -> Result<u64, BeginError> {
        let plan = self.jobs.begin_run(job_id, trigger, now_ms)?;
        let run_id = plan.run_id;
        let cache = self.jobs.cache();
        let event_tx = self.event_tx.clone();
        let spawned = crate::thread_spawn::spawn_named_with("flockr-job-run", plan, move |plan| {
            let result = crate::jobs::runner::run_plan(&plan, &cache);
            let _ =
                event_tx.blocking_send(crate::events::AppEvent::JobRunFinished(Box::new(result)));
        });
        if let Err((plan, err)) = spawned {
            tracing::warn!(job = %plan.job.id, err = %err, "failed to spawn job run thread");
            let message = format!("could not start the job worker thread: {err}");
            let now = now_unix_ms();
            let record = RunRecord {
                info: crate::api::schema::JobRunInfo {
                    run_id: plan.run_id,
                    trigger: plan.trigger,
                    status: JobRunStatus::SpawnFailed,
                    started_unix_ms: plan.started_unix_ms,
                    finished_unix_ms: now.max(plan.started_unix_ms),
                    duration_ms: 0,
                    exit_code: None,
                    summary: message.clone(),
                    error: Some(message),
                    skipped_count: 0,
                    env_note: None,
                },
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                handoff: None,
            };
            self.finish_job_run(JobRunResult {
                job_id: plan.job.id,
                record,
            });
        }
        Ok(run_id)
    }

    /// Record a finished run, publish `job.run_finished` and queue its toast.
    pub(crate) fn finish_job_run(&mut self, result: JobRunResult) -> bool {
        let Some(finished) = self.jobs.finish_run(result) else {
            return false;
        };
        let feed_refreshing = finished.info.status == JobRunStatus::Success
            && self.refresh_feed_after_run(&finished.job_id);
        if let Some(def) = finished.def.clone() {
            if def.handoff.is_some() || finished.record.handoff.is_some() {
                let pending = crate::jobs::handoff::PendingHandoff {
                    record: finished.record.clone(),
                    def,
                    previous_fingerprint: finished.previous_fingerprint.clone(),
                };
                if feed_refreshing {
                    // Wait for the refresh so the prompt sees the new items.
                    self.pending_handoffs
                        .insert(finished.job_id.clone(), pending);
                } else {
                    self.evaluate_handoff(pending);
                }
            }
        }
        self.emit_event(EventEnvelope {
            event: EventKind::JobRunFinished,
            data: EventData::JobRunFinished {
                job_id: finished.job_id.clone(),
                title: finished.title.clone(),
                run: finished.info,
            },
        });
        if let Some(notice) = finished.notice {
            if self.state.toast_config.delivery != crate::config::ToastDelivery::Off {
                // Client shells and other attached clients get every notice
                // (they apply `delivery` themselves); the in-frame toast below
                // is for the app's own rendering.
                self.job_client_notices.push(notice.clone());
            }
            self.queue_job_notice(notice);
        }
        self.deliver_job_notices();
        true
    }

    /// Queue a toast for the in-frame slot.
    pub(crate) fn queue_job_notice(&mut self, notice: Notice) {
        if self.pending_job_notices.len() >= MAX_PENDING_NOTICES {
            self.pending_job_notices.pop_front();
        }
        self.pending_job_notices.push_back(notice);
    }

    /// Show queued job results as in-frame toasts, one at a time when the
    /// slot is free. Terminal and system delivery are forwarded to clients by
    /// the server (`take_job_client_notices`); `off` shows nothing.
    pub(crate) fn deliver_job_notices(&mut self) -> bool {
        use crate::config::ToastDelivery;

        let mut changed = false;
        match self.state.toast_config.delivery {
            // A question for the user shows in the frame even when toasts are
            // off, like the upstream notice always did.
            ToastDelivery::Off => self.pending_job_notices.retain(|notice| notice.actionable),
            ToastDelivery::Terminal | ToastDelivery::System => self.pending_job_notices.clear(),
            ToastDelivery::Flockr => {}
        }
        if matches!(
            self.state.toast_config.delivery,
            ToastDelivery::Off | ToastDelivery::Flockr
        ) && self.state.toast.is_none()
        {
            if let Some(notice) = self.pending_job_notices.pop_front() {
                let previous = self.state.toast.clone();
                let actionable = notice.actionable;
                self.state.toast = Some(ToastNotification {
                    kind: if notice.failed {
                        ToastKind::NeedsAttention
                    } else {
                        ToastKind::Finished
                    },
                    title: notice.title,
                    context: notice.body,
                    position: None,
                    target: None,
                });
                self.sync_toast_deadline(previous);
                if actionable {
                    self.toast_deadline = Some(Instant::now() + ACTIONABLE_TOAST_DURATION);
                }
                changed = true;
            }
        }
        changed
    }

    /// Results to forward to attached clients, which present them according
    /// to `[ui.toast] delivery`.
    pub(crate) fn take_job_client_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.job_client_notices)
    }

    pub(super) fn handle_job_list(&mut self, id: String, params: JobListParams) -> String {
        self.refresh_jobs(false);
        let revision = self.jobs.revision();
        let unchanged = params.since_revision == Some(revision);
        responses::encode_success(
            id,
            ResponseResult::JobList {
                revision,
                unchanged,
                jobs: if unchanged {
                    Vec::new()
                } else {
                    self.jobs.infos(now_unix_ms())
                },
                env: self.jobs.env_status(),
            },
        )
    }

    pub(super) fn handle_job_get(&mut self, id: String, params: JobTarget) -> String {
        self.refresh_jobs(false);
        let (Some(job), Some(history)) = (
            self.jobs.info(&params.job_id, now_unix_ms()),
            self.jobs.history(&params.job_id),
        ) else {
            return job_not_found(id, &params.job_id);
        };
        responses::encode_success(id, ResponseResult::JobDetail { job, history })
    }

    pub(super) fn handle_job_run(&mut self, id: String, params: JobTarget) -> String {
        self.refresh_jobs(false);
        match self.start_job_run(&params.job_id, JobRunTrigger::Manual, now_unix_ms()) {
            Ok(run_id) => responses::encode_success(
                id,
                ResponseResult::JobRunRequested {
                    job_id: params.job_id,
                    started: true,
                    run_id: Some(run_id),
                    reason: None,
                },
            ),
            Err(BeginError::AlreadyRunning) => responses::encode_success(
                id,
                ResponseResult::JobRunRequested {
                    job_id: params.job_id,
                    started: false,
                    run_id: None,
                    reason: Some("skipped: still running".into()),
                },
            ),
            Err(error) => begin_error_response(id, &params.job_id, error),
        }
    }

    pub(super) fn handle_job_set_enabled(
        &mut self,
        id: String,
        params: JobSetEnabledParams,
    ) -> String {
        self.refresh_jobs(false);
        match self
            .jobs
            .set_enabled(&params.job_id, params.enabled, now_unix_ms())
        {
            Ok(job) => {
                self.next_jobs_tick = Some(Instant::now());
                responses::encode_success(id, ResponseResult::JobUpdated { job })
            }
            Err(error) => begin_error_response(id, &params.job_id, error),
        }
    }

    /// `job.set_schedule`: system jobs write `[jobs.system.<name>]` in
    /// config.toml; other jobs get their `job.toml` schedule line replaced, or,
    /// when the file cannot be edited safely, an override in their state.
    pub(super) fn handle_job_set_schedule(
        &mut self,
        id: String,
        params: JobSetScheduleParams,
    ) -> String {
        use crate::jobs::definition::JobSource;
        use crate::jobs::schedule_edit::{edit_job_toml, parse_schedule_input};

        self.refresh_jobs(false);
        let input = match parse_schedule_input(&params.schedule) {
            Ok(input) => input,
            Err(message) => return responses::encode_error(id, "invalid_schedule", message),
        };
        let Some(def) = self.jobs.job_def(&params.job_id).cloned() else {
            return match self.jobs.info(&params.job_id, now_unix_ms()) {
                Some(_) => responses::encode_error(
                    id,
                    "job_invalid",
                    format!(
                        "job {} has problems; fix its job file before changing its schedule",
                        params.job_id
                    ),
                ),
                None => job_not_found(id, &params.job_id),
            };
        };
        let mut note = None;
        let stored_in = match &def.source {
            JobSource::System => {
                let Some(name) = crate::jobs::system::system_job_name(&def.id) else {
                    return responses::encode_error(
                        id,
                        "job_invalid",
                        format!("{} is not a known system job", def.id),
                    );
                };
                if let Err(message) =
                    crate::config::write_system_job_schedule(name, input.key(), input.value())
                {
                    return responses::encode_error(id, "schedule_write_failed", message);
                }
                // Read the config back so the job picks the new schedule up now.
                match crate::config::load_live_config() {
                    Ok(loaded) => {
                        self.jobs_config = loaded.config.jobs.clone();
                        self.upstream_config = loaded.config.upstream.clone();
                    }
                    Err(diagnostics) => {
                        return responses::encode_error(
                            id,
                            "schedule_write_failed",
                            format!(
                                "the schedule was written but config.toml no longer loads: {}",
                                diagnostics.join("; ")
                            ),
                        );
                    }
                }
                let diagnostics = self.apply_system_jobs();
                for diagnostic in diagnostics {
                    tracing::warn!(%diagnostic, "system job configuration");
                }
                JobScheduleStore::Config
            }
            JobSource::User => {
                let path = def.dir.join("job.toml");
                let edited = std::fs::read_to_string(&path)
                    .map_err(|err| format!("cannot read {}: {err}", path.display()))
                    .and_then(|content| edit_job_toml(&content, &input));
                match edited {
                    Ok(content) => {
                        if let Err(err) = std::fs::write(&path, content) {
                            return responses::encode_error(
                                id,
                                "schedule_write_failed",
                                format!("could not write {}: {err}", path.display()),
                            );
                        }
                        let _ = self.jobs.set_schedule_override(&def.id, None);
                        JobScheduleStore::JobFile
                    }
                    Err(why) => {
                        if let Err(error) = self
                            .jobs
                            .set_schedule_override(&def.id, Some(input.describe()))
                        {
                            return begin_error_response(id, &params.job_id, error);
                        }
                        note = Some(format!(
                            "job.toml was not changed ({why}); the schedule is stored as an override in the job's state. Remove it by editing the file and running `flockr job schedule` again."
                        ));
                        JobScheduleStore::State
                    }
                }
            }
            JobSource::Plugin { .. } => {
                if let Err(error) = self
                    .jobs
                    .set_schedule_override(&def.id, Some(input.describe()))
                {
                    return begin_error_response(id, &params.job_id, error);
                }
                note = Some(
                    "plugin job schedules come from the plugin manifest; the new schedule is stored as an override in the job's state"
                        .into(),
                );
                JobScheduleStore::State
            }
        };
        self.refresh_jobs(true);
        self.jobs.restart_schedule(&params.job_id, now_unix_ms());
        self.next_jobs_tick = Some(Instant::now());
        match self.jobs.info(&params.job_id, now_unix_ms()) {
            Some(job) => responses::encode_success(
                id,
                ResponseResult::JobScheduleUpdated {
                    job,
                    stored_in,
                    note,
                },
            ),
            None => job_not_found(id, &params.job_id),
        }
    }

    pub(super) fn handle_job_logs(&mut self, id: String, params: JobLogsParams) -> String {
        self.refresh_jobs(false);
        if !self.jobs.has_job(&params.job_id) {
            return job_not_found(id, &params.job_id);
        }
        let Some(record) = self.jobs.run_record(&params.job_id, params.run_id) else {
            return responses::encode_error(
                id,
                "no_job_run",
                match params.run_id {
                    Some(run_id) => {
                        format!("job {} has no run {run_id} in its history", params.job_id)
                    }
                    None => format!("job {} has not run yet", params.job_id),
                },
            );
        };
        let lines = params
            .lines
            .map_or(DEFAULT_LOG_LINES, |lines| lines as usize);
        responses::encode_success(
            id,
            ResponseResult::JobLogs {
                job_id: params.job_id,
                run: record.info.clone(),
                stdout: last_lines(&record.stdout_tail, lines).to_owned(),
                stderr: last_lines(&record.stderr_tail, lines).to_owned(),
                stdout_truncated: record.stdout_truncated,
                stderr_truncated: record.stderr_truncated,
            },
        )
    }

    /// Start recapturing the login-shell environment in the background and
    /// answer at once; progress shows in `job.list`'s `env` status.
    pub(super) fn handle_job_env_refresh(&mut self, id: String) -> String {
        let cache = self.jobs.cache();
        cache.mark_refreshing();
        let worker_cache = std::sync::Arc::clone(&cache);
        if let Err(err) = crate::thread_spawn::spawn_named("flockr-job-env-refresh", move || {
            worker_cache.refresh();
        }) {
            tracing::warn!(err = %err, "failed to spawn job env refresh thread");
            return responses::encode_error(
                id,
                "job_env_refresh_failed",
                format!("could not start the refresh: {err}"),
            );
        }
        self.bump_jobs_revision_for_env();
        responses::encode_success(
            id,
            ResponseResult::JobEnvRefreshing {
                env: self.jobs.env_status(),
            },
        )
    }

    fn bump_jobs_revision_for_env(&mut self) {
        // The environment status rides along with `job.list`; `since_revision`
        // must not hide a refresh that just started.
        self.jobs.touch_revision();
    }
}

fn job_not_found(id: String, job_id: &str) -> String {
    responses::encode_error(id, "job_not_found", format!("no job named {job_id}"))
}

fn begin_error_response(id: String, job_id: &str, error: BeginError) -> String {
    match error {
        BeginError::NotFound => job_not_found(id, job_id),
        BeginError::Invalid(_) => {
            responses::encode_error(id, "job_invalid", format!("job {job_id}: {error}"))
        }
        BeginError::AlreadyRunning => {
            responses::encode_error(id, "job_running", format!("job {job_id} is still running"))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::schema::{
        ErrorResponse, JobRunStatus, JobState, Method, Request, SuccessResponse,
    };
    use crate::jobs::env::LoginEnvCache;
    use crate::jobs::{JobsManager, JobsPaths};

    struct Fixture {
        root: std::path::PathBuf,
        app: App,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "flockr-app-jobs-{name}-{}-{nanos}",
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
            app.state.toast_config.delivery = crate::config::ToastDelivery::Flockr;
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

        fn err(&mut self, method: Method) -> (String, String) {
            let response = self.call(method);
            let error = serde_json::from_str::<ErrorResponse>(&response)
                .unwrap_or_else(|err| panic!("not an error: {response}: {err}"))
                .error;
            (error.code, error.message)
        }

        /// Pump app events until the job's worker reports.
        fn wait_for_run(&mut self) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                self.app.drain_all_internal_events();
                if self
                    .app
                    .jobs
                    .info("echo", 0)
                    .is_none_or(|info| info.state != JobState::Running)
                    && !self.app.jobs.is_running("echo")
                {
                    return;
                }
                assert!(std::time::Instant::now() < deadline, "job did not finish");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const ECHO_JOB: &str = "title = \"Echo job\"\ncommand = [\"sh\", \"-c\", \"echo hello; echo oops >&2\"]\nevery = \"1h\"\nnotify = \"always\"\nenv = \"server\"\n";

    fn target(id: &str) -> JobTarget {
        JobTarget { job_id: id.into() }
    }

    #[test]
    fn list_get_and_unknown_jobs() {
        let mut fx = Fixture::new("list");
        fx.write_job("echo", ECHO_JOB);
        fx.write_job("broken", "command = []\n");
        let ResponseResult::JobList {
            revision,
            unchanged,
            jobs,
            ..
        } = fx.ok(Method::JobList(JobListParams::default()))
        else {
            panic!("expected a job list");
        };
        assert!(!unchanged);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[1].job_id, "echo");
        assert_eq!(jobs[0].state, JobState::Invalid);

        // The same revision answers without the list.
        let ResponseResult::JobList {
            unchanged, jobs, ..
        } = fx.ok(Method::JobList(JobListParams {
            since_revision: Some(revision),
        }))
        else {
            panic!("expected a job list");
        };
        assert!(unchanged && jobs.is_empty());

        let ResponseResult::JobDetail { job, history } = fx.ok(Method::JobGet(target("echo")))
        else {
            panic!("expected detail");
        };
        assert_eq!(job.title, "Echo job");
        assert!(history.is_empty());

        assert_eq!(fx.err(Method::JobGet(target("nope"))).0, "job_not_found");
        assert_eq!(fx.err(Method::JobRun(target("nope"))).0, "job_not_found");
        let (code, message) = fx.err(Method::JobRun(target("broken")));
        assert_eq!(code, "job_invalid");
        assert!(message.contains("command: required"), "{message}");
        assert_eq!(
            fx.err(Method::JobLogs(JobLogsParams {
                job_id: "echo".into(),
                run_id: None,
                lines: None
            }))
            .0,
            "no_job_run"
        );
    }

    #[test]
    fn running_a_job_records_output_emits_the_event_and_toasts() {
        let mut fx = Fixture::new("run");
        fx.write_job("echo", ECHO_JOB);
        let ResponseResult::JobRunRequested {
            started, run_id, ..
        } = fx.ok(Method::JobRun(target("echo")))
        else {
            panic!("expected run request");
        };
        assert!(started);
        assert_eq!(run_id, Some(1));
        // While it runs, a second request is refused without a new run.
        if fx.app.jobs.is_running("echo") {
            let ResponseResult::JobRunRequested {
                started, reason, ..
            } = fx.ok(Method::JobRun(target("echo")))
            else {
                panic!("expected run request");
            };
            if !started {
                assert_eq!(reason.as_deref(), Some("skipped: still running"));
            }
        }
        fx.wait_for_run();

        let ResponseResult::JobLogs {
            run,
            stdout,
            stderr,
            ..
        } = fx.ok(Method::JobLogs(JobLogsParams {
            job_id: "echo".into(),
            run_id: None,
            lines: None,
        }))
        else {
            panic!("expected logs");
        };
        assert_eq!(run.status, JobRunStatus::Success);
        assert_eq!(stdout, "hello\n");
        assert_eq!(stderr, "oops\n");
        assert_eq!(run.env_note.as_deref(), Some("server environment"));

        let ResponseResult::JobDetail { job, history } = fx.ok(Method::JobGet(target("echo")))
        else {
            panic!("expected detail");
        };
        assert_eq!(job.last_run.unwrap().run_id, run.run_id);
        assert_eq!(history.len(), 1);

        // job.run_finished is on the event stream.
        let events = fx.app.event_hub.events_after(0);
        let finished: Vec<_> = events
            .iter()
            .filter(|(_, event)| event.event == EventKind::JobRunFinished)
            .collect();
        assert_eq!(finished.len(), 1);
        let EventData::JobRunFinished { job_id, title, run } = &finished[0].1.data else {
            panic!("wrong payload");
        };
        assert_eq!((job_id.as_str(), title.as_str()), ("echo", "Echo job"));
        assert_eq!(run.status, JobRunStatus::Success);

        // notify = "always" with in-frame delivery shows a toast.
        let toast = fx.app.state.toast.clone().expect("toast shown");
        assert_eq!(toast.title, "Echo job");
        assert!(toast.context.starts_with("ok in "), "{}", toast.context);
        assert!(toast.context.ends_with("hello"), "{}", toast.context);
        assert_eq!(toast.kind, ToastKind::Finished);
    }

    fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(files_under(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    #[test]
    fn login_shell_values_reach_the_job_but_never_disk_or_api_output() {
        let secret = "s3cr3t-token-9f2c41";
        // The job file carries only a prefix, so a leak of the whole value is detectable.
        let prefix = "s3cr3t-token-";
        let mut fx = Fixture::new("secret");
        fx.app.jobs = JobsManager::new(
            JobsPaths {
                config_dir: fx.root.join("config"),
                state_root: fx.root.join("state"),
            },
            Arc::new(LoginEnvCache::new(Box::new(move || {
                Ok(vec![
                    ("PATH".into(), "/usr/bin:/bin".into()),
                    ("GITHUB_TOKEN".into(), secret.into()),
                ])
            }))),
            true,
        );
        // The default env mode is the login shell. The job only reports
        // whether it saw the value, never the value itself.
        fx.write_job(
            "echo",
            &format!(
                "command = [\"sh\", \"-c\", \"case \\\"$GITHUB_TOKEN\\\" in {prefix}*) echo token-visible;; esac\"]\nevery = \"1h\"\n"
            ),
        );
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();

        let ResponseResult::JobLogs { stdout, run, .. } = fx.ok(Method::JobLogs(JobLogsParams {
            job_id: "echo".into(),
            run_id: None,
            lines: None,
        })) else {
            panic!("expected logs");
        };
        assert_eq!(
            stdout, "token-visible\n",
            "the job saw the login-shell value"
        );
        assert_eq!(run.env_note.as_deref(), Some("login-shell (2 variables)"));

        let mut surfaces = vec![
            fx.call(Method::JobList(JobListParams::default())),
            fx.call(Method::JobGet(target("echo"))),
            fx.call(Method::JobLogs(JobLogsParams {
                job_id: "echo".into(),
                run_id: None,
                lines: None,
            })),
        ];
        let events = fx.app.event_hub.events_after(0);
        surfaces.push(
            serde_json::to_string(&events.iter().map(|(_, e)| e).collect::<Vec<_>>()).unwrap(),
        );
        let persisted = files_under(&fx.root.join("state"));
        assert!(
            persisted.iter().any(|path| path.ends_with("state.json")),
            "state was persisted"
        );
        for path in persisted {
            surfaces.push(std::fs::read_to_string(&path).unwrap_or_default());
        }
        for surface in surfaces {
            assert!(!surface.contains(secret), "secret leaked into: {surface}");
        }
    }

    #[test]
    fn logs_can_be_trimmed_to_the_last_lines() {
        let mut fx = Fixture::new("logs");
        fx.write_job(
            "echo",
            "command = [\"sh\", \"-c\", \"printf 'a\\\\nb\\\\nc\\\\n'\"]\nevery = \"1h\"\nenv = \"server\"\nnotify = \"never\"\n",
        );
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();
        let ResponseResult::JobLogs { stdout, .. } = fx.ok(Method::JobLogs(JobLogsParams {
            job_id: "echo".into(),
            run_id: None,
            lines: Some(2),
        })) else {
            panic!("expected logs");
        };
        assert_eq!(stdout, "b\nc\n");
        assert!(fx.app.state.toast.is_none(), "notify = never");
    }

    #[test]
    fn failures_toast_with_the_attention_style_and_delivery_off_is_silent() {
        let mut fx = Fixture::new("fail");
        fx.write_job(
            "echo",
            "command = [\"sh\", \"-c\", \"echo broke >&2; exit 4\"]\nevery = \"1h\"\nenv = \"server\"\n",
        );
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();
        let toast = fx.app.state.toast.clone().expect("failure toast");
        assert_eq!(toast.kind, ToastKind::NeedsAttention);
        assert!(
            toast.context.starts_with("failed (exit 4)"),
            "{}",
            toast.context
        );

        fx.app.state.toast = None;
        fx.app.state.toast_config.delivery = crate::config::ToastDelivery::Off;
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();
        assert!(fx.app.state.toast.is_none());
        assert!(fx.app.pending_job_notices.is_empty());
    }

    #[test]
    fn terminal_delivery_queues_a_client_notification() {
        let mut fx = Fixture::new("terminal");
        fx.write_job("echo", ECHO_JOB);
        fx.app.state.toast_config.delivery = crate::config::ToastDelivery::Terminal;
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();
        assert!(fx.app.state.toast.is_none());
        let notices = fx.app.take_job_client_notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "Echo job");
        assert!(fx.app.take_job_client_notices().is_empty());
    }

    #[test]
    fn a_busy_toast_slot_holds_job_notices_until_it_clears() {
        let mut fx = Fixture::new("busy");
        fx.write_job("echo", ECHO_JOB);
        fx.app.state.toast = Some(ToastNotification {
            kind: ToastKind::Finished,
            title: "other".into(),
            context: String::new(),
            position: None,
            target: None,
        });
        fx.ok(Method::JobRun(target("echo")));
        fx.wait_for_run();
        assert_eq!(fx.app.state.toast.as_ref().unwrap().title, "other");
        assert_eq!(fx.app.pending_job_notices.len(), 1);
        fx.app.state.toast = None;
        assert!(fx.app.deliver_job_notices());
        assert_eq!(fx.app.state.toast.as_ref().unwrap().title, "Echo job");
    }

    #[test]
    fn enable_disable_round_trip_and_disabled_jobs_are_not_scheduled() {
        let mut fx = Fixture::new("enable");
        fx.write_job("echo", ECHO_JOB);
        let ResponseResult::JobUpdated { job } =
            fx.ok(Method::JobSetEnabled(JobSetEnabledParams {
                job_id: "echo".into(),
                enabled: false,
            }))
        else {
            panic!("expected update");
        };
        assert_eq!(job.state, JobState::Disabled);
        fx.app.run_jobs_tick(Instant::now());
        assert!(!fx.app.jobs.is_running("echo"));
        let ResponseResult::JobUpdated { job } =
            fx.ok(Method::JobSetEnabled(JobSetEnabledParams {
                job_id: "echo".into(),
                enabled: true,
            }))
        else {
            panic!("expected update");
        };
        assert_eq!(job.state, JobState::Idle);
        assert_eq!(
            fx.err(Method::JobSetEnabled(JobSetEnabledParams {
                job_id: "nope".into(),
                enabled: true
            }))
            .0,
            "job_not_found"
        );
    }

    #[test]
    fn the_tick_starts_a_due_job_once_and_arms_the_timer() {
        let mut fx = Fixture::new("tick");
        fx.write_job("echo", ECHO_JOB);
        // Make the job look written long ago so its first interval has passed.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
        let file = std::fs::File::options()
            .write(true)
            .open(fx.root.join("config/echo/job.toml"))
            .unwrap();
        file.set_modified(long_ago).unwrap();
        drop(file);

        let started = Instant::now();
        assert!(fx.app.run_jobs_tick(started));
        assert!(
            fx.app.jobs.is_running("echo"),
            "missed run starts at the first tick"
        );
        let next = fx.app.next_jobs_tick.expect("timer armed");
        assert!(
            next > started
                && next <= started + MAX_TICK_INTERVAL + std::time::Duration::from_secs(1)
        );
        fx.wait_for_run();
        let history = fx.app.jobs.history("echo").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].trigger, JobRunTrigger::CatchUp);
        // A second pass does not run it again.
        fx.app.run_jobs_tick(Instant::now());
        assert!(!fx.app.jobs.is_running("echo"));
        assert_eq!(fx.app.jobs.history("echo").unwrap().len(), 1);
    }

    #[test]
    fn reload_config_rereads_job_files() {
        let mut fx = Fixture::new("reload");
        fx.write_job("echo", ECHO_JOB);
        fx.ok(Method::JobList(JobListParams::default()));
        // Same length edit within one mtime tick: only a forced reload sees it.
        fx.write_job("echo", &ECHO_JOB.replace("Echo job", "Echo JOB"));
        fx.app.reload_jobs_now();
        assert_eq!(fx.app.jobs.info("echo", 0).unwrap().title, "Echo JOB");
        assert!(fx.app.next_jobs_tick.is_some());
    }

    #[test]
    fn env_refresh_answers_at_once_and_completes_in_the_background() {
        let mut fx = Fixture::new("env");
        let ResponseResult::JobEnvRefreshing { env } = fx.ok(Method::JobEnvRefresh(
            crate::api::schema::EmptyParams::default(),
        )) else {
            panic!("expected env status");
        };
        assert!(env.refreshing || env.source != "none");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let ResponseResult::JobList { env, .. } =
                fx.ok(Method::JobList(JobListParams::default()))
            else {
                panic!("expected list");
            };
            if !env.refreshing && env.source != "none" {
                assert_eq!(env.source, "login-shell");
                assert_eq!(env.variable_count, 1);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "refresh did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn the_method_is_advertised_by_name_and_encoded_as_a_request() {
        let request = Request {
            id: "1".into(),
            method: Method::JobRun(target("a")),
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "job.run");
        assert_eq!(json["params"]["job_id"], "a");
        assert!(crate::api::request_changes_ui(&request));
        assert!(
            crate::server::client_commands::supports_client_shell_method(&Method::JobList(
                JobListParams::default()
            ))
        );
        assert!(
            !crate::server::client_commands::supports_client_shell_method(&Method::JobEnvRefresh(
                crate::api::schema::EmptyParams::default()
            ))
        );
    }

    fn set_schedule(
        fx: &mut Fixture,
        id: &str,
        text: &str,
    ) -> Result<ResponseResult, (String, String)> {
        let response = fx.call(Method::JobSetSchedule(JobSetScheduleParams {
            job_id: id.into(),
            schedule: text.into(),
        }));
        match serde_json::from_str::<SuccessResponse>(&response) {
            Ok(success) => Ok(success.result),
            Err(_) => {
                let error = serde_json::from_str::<ErrorResponse>(&response)
                    .unwrap_or_else(|err| panic!("neither success nor error: {response}: {err}"))
                    .error;
                Err((error.code, error.message))
            }
        }
    }

    #[test]
    fn schedule_of_a_file_job_is_rewritten_in_its_job_toml() {
        let mut fx = Fixture::new("sched-file");
        fx.write_job("echo", ECHO_JOB);
        let Ok(ResponseResult::JobScheduleUpdated {
            job,
            stored_in,
            note,
        }) = set_schedule(&mut fx, "echo", "every 2h")
        else {
            panic!("expected an updated schedule");
        };
        assert_eq!(stored_in, JobScheduleStore::JobFile);
        assert!(note.is_none());
        assert_eq!(job.schedule, "every 2h");
        assert!(!job.schedule_overridden);
        let text = std::fs::read_to_string(fx.root.join("config/echo/job.toml")).unwrap();
        assert!(text.contains("every = \"2h\""), "{text}");
        assert_eq!(
            text,
            ECHO_JOB.replace("every = \"1h\"", "every = \"2h\""),
            "only the schedule line changed"
        );
        // Cron works too and swaps the key.
        let Ok(ResponseResult::JobScheduleUpdated { job, .. }) =
            set_schedule(&mut fx, "echo", "0 9 * * 1-5")
        else {
            panic!("expected an updated schedule");
        };
        assert_eq!(job.schedule, "0 9 * * 1-5");
        let text = std::fs::read_to_string(fx.root.join("config/echo/job.toml")).unwrap();
        assert!(
            text.contains("schedule = \"0 9 * * 1-5\"") && !text.contains("every"),
            "{text}"
        );
        // The scheduler restarted from now: the next run is in the future.
        assert!(job.next_run_unix_ms.unwrap() > crate::jobs::now_unix_ms());
    }

    #[test]
    fn schedule_falls_back_to_a_state_override_when_the_file_cannot_be_edited() {
        let mut fx = Fixture::new("sched-state");
        // The schedule is a multi-line string, which the line editor leaves alone.
        let original = "command = [\"true\"]\nevery = \"\"\"1h\"\"\"\nenv = \"server\"\n";
        fx.write_job("odd", original);
        let Ok(ResponseResult::JobScheduleUpdated {
            job,
            stored_in,
            note,
        }) = set_schedule(&mut fx, "odd", "every 30m")
        else {
            panic!("expected an updated schedule");
        };
        assert_eq!(stored_in, JobScheduleStore::State);
        assert!(note.unwrap().contains("override"));
        assert!(job.schedule_overridden);
        assert_eq!(job.schedule, "every 30m");
        assert_eq!(
            std::fs::read_to_string(fx.root.join("config/odd/job.toml")).unwrap(),
            original,
            "the job file is untouched"
        );
        // The override is in the job's state file and survives a reload.
        let state = std::fs::read_to_string(fx.root.join("state/odd/state.json")).unwrap();
        assert!(state.contains("every 30m"), "{state}");
        fx.app.reload_jobs_now();
        assert_eq!(fx.app.jobs.info("odd", 0).unwrap().schedule, "every 30m");
    }

    #[test]
    fn bad_schedules_and_unknown_jobs_are_refused() {
        let mut fx = Fixture::new("sched-bad");
        fx.write_job("echo", ECHO_JOB);
        fx.write_job("broken", "command = []\n");
        assert_eq!(
            set_schedule(&mut fx, "echo", "every 5s").unwrap_err().0,
            "invalid_schedule"
        );
        assert_eq!(
            set_schedule(&mut fx, "echo", "whenever").unwrap_err().0,
            "invalid_schedule"
        );
        assert_eq!(
            set_schedule(&mut fx, "nope", "every 1h").unwrap_err().0,
            "job_not_found"
        );
        assert_eq!(
            set_schedule(&mut fx, "broken", "every 1h").unwrap_err().0,
            "job_invalid"
        );
        let text = std::fs::read_to_string(fx.root.join("config/echo/job.toml")).unwrap();
        assert_eq!(text, ECHO_JOB, "refused edits change nothing");
    }

    #[test]
    fn system_job_schedule_is_written_to_the_config_and_applied() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let mut fx = Fixture::new("sched-system");
        let config_path = fx.root.join("config.toml");
        std::fs::write(&config_path, "[ui]\nsidebar_width = 31\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);

        fx.app.apply_system_jobs();
        fx.app.reload_jobs_now();
        let info = fx
            .app
            .jobs
            .info("system.upstream-herdr", 0)
            .expect("system job");
        assert_eq!(info.source, "system");
        assert_eq!(info.schedule, "every 1d");

        let result = set_schedule(&mut fx, "system.upstream-herdr", "every 2h");
        let written = std::fs::read_to_string(&config_path).unwrap();
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let Ok(ResponseResult::JobScheduleUpdated { job, stored_in, .. }) = result else {
            panic!("expected an updated schedule: {result:?}");
        };
        assert_eq!(stored_in, JobScheduleStore::Config);
        assert_eq!(job.schedule, "every 2h", "applied without a restart");
        assert!(
            written.contains("[jobs.system.upstream-herdr]") && written.contains("every = \"2h\""),
            "{written}"
        );
        assert!(
            written.contains("sidebar_width = 31"),
            "the rest of the config stays"
        );
    }

    #[test]
    fn system_jobs_list_with_source_system_and_can_run_by_hand() {
        let mut fx = Fixture::new("system-list");
        fx.write_job("mine", ECHO_JOB);
        fx.app.apply_system_jobs();
        let ResponseResult::JobList { jobs, .. } = fx.ok(Method::JobList(JobListParams::default()))
        else {
            panic!("expected a job list");
        };
        let sources: Vec<_> = jobs
            .iter()
            .map(|job| (job.job_id.as_str(), job.source.as_str()))
            .collect();
        assert_eq!(
            sources,
            [("mine", "user"), ("system.upstream-herdr", "system")]
        );
    }
}
