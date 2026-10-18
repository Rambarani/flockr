//! App-side glue for scheduled jobs: the scheduler tick, worker threads,
//! toasts, events and the `job.*` socket API. The registry, scheduling rules
//! and process execution live in `crate::jobs`.

use std::time::{Duration, Instant};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, JobListParams, JobLogsParams, JobRunStatus, JobRunTrigger,
    JobSetEnabledParams, JobTarget, ResponseResult,
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
        changed |= self.deliver_job_notices();
        let mut wait = MAX_TICK_INTERVAL;
        if let Some(due_ms) = self.jobs.next_deadline_ms() {
            wait = wait.min(Duration::from_millis(due_ms.saturating_sub(now_ms)));
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
        self.emit_event(EventEnvelope {
            event: EventKind::JobRunFinished,
            data: EventData::JobRunFinished {
                job_id: finished.job_id.clone(),
                title: finished.title.clone(),
                run: finished.info,
            },
        });
        if let Some(notice) = finished.notice {
            if self.pending_job_notices.len() >= MAX_PENDING_NOTICES {
                self.pending_job_notices.pop_front();
            }
            self.pending_job_notices.push_back(notice);
        }
        self.deliver_job_notices();
        true
    }

    /// Show queued job results according to `[ui.toast] delivery`: in-frame
    /// toasts one at a time when the slot is free, client notifications for
    /// terminal and system delivery, nothing when delivery is off.
    pub(crate) fn deliver_job_notices(&mut self) -> bool {
        use crate::config::ToastDelivery;

        let mut changed = false;
        match self.state.toast_config.delivery {
            ToastDelivery::Off => self.pending_job_notices.clear(),
            ToastDelivery::Terminal | ToastDelivery::System => {
                self.job_client_notices
                    .extend(self.pending_job_notices.drain(..));
            }
            ToastDelivery::Flockr => {
                if self.state.toast.is_none() {
                    if let Some(notice) = self.pending_job_notices.pop_front() {
                        let previous = self.state.toast.clone();
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
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Results for clients to show as terminal or system notifications.
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
