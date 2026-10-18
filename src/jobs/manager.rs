//! The job registry and scheduler. Pure with respect to threads and the
//! clock: every method takes `now_ms`, runs are started by the caller from
//! the returned `RunPlan`, and results come back through `finish_run`. That
//! keeps scheduling, catch-up, overlap handling and notification policy
//! testable without processes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use super::cron::{SystemLocal, UtcOffset};
use super::definition::{
    format_duration, parse_job_toml, parse_plugin_manifest_jobs, valid_job_id, InvalidJob, JobDef,
    JobSource, NotifyPolicy, Schedule,
};
use super::env::LoginEnvCache;
use super::runner::{JobRunResult, RunPlan};
use super::state::{self, JobStateFile, RunRecord};
use crate::api::schema::{
    JobEnvStatus, JobInfo, JobRunInfo, JobRunStatus, JobRunTrigger, JobScheduleKind, JobState,
};

#[derive(Debug, Clone)]
pub(crate) struct JobsPaths {
    /// `<config>/jobs`
    pub(crate) config_dir: PathBuf,
    /// `<state>/jobs`
    pub(crate) state_root: PathBuf,
}

impl JobsPaths {
    pub(crate) fn system() -> Self {
        Self {
            config_dir: crate::config::config_dir().join("jobs"),
            state_root: crate::config::state_dir().join("jobs"),
        }
    }
}

/// An installed, enabled plugin whose manifest may declare `[[jobs]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginJobSource {
    pub(crate) plugin_id: String,
    pub(crate) plugin_root: PathBuf,
    pub(crate) manifest_path: PathBuf,
}

enum Definition {
    Valid(Box<JobDef>),
    Invalid(InvalidJob),
}

struct Running {
    started_unix_ms: u64,
}

struct Entry {
    definition: Definition,
    state: JobStateFile,
    running: Option<Running>,
    next_due_ms: Option<u64>,
    /// Whether the due time found at load was missed while flockr was down.
    catch_up: bool,
    /// Modification time of the file that defines the job: the schedule
    /// anchor for a job that has never run.
    file_modified_ms: u64,
}

impl Entry {
    fn def(&self) -> Option<&JobDef> {
        match &self.definition {
            Definition::Valid(def) => Some(def),
            Definition::Invalid(_) => None,
        }
    }

    fn enabled(&self) -> bool {
        match &self.definition {
            Definition::Valid(def) => self.state.enabled_override.unwrap_or(def.enabled),
            Definition::Invalid(_) => false,
        }
    }
}

type Signature = Vec<(String, u64, u64)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BeginError {
    NotFound,
    Invalid(Vec<String>),
    AlreadyRunning,
}

impl std::fmt::Display for BeginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such job"),
            Self::Invalid(diagnostics) => {
                write!(f, "the job file is invalid: {}", diagnostics.join("; "))
            }
            Self::AlreadyRunning => f.write_str("still running"),
        }
    }
}

/// A toast-worthy result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) title: String,
    pub(crate) body: String,
    /// The run did not succeed; the toast uses the attention style.
    pub(crate) failed: bool,
}

/// A job that has a feed, for the feed API.
#[derive(Debug, Clone)]
pub(crate) struct FeedJobRef {
    pub(crate) job_id: String,
    pub(crate) job_title: String,
    pub(crate) feed_title: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) running: bool,
}

pub(crate) struct FinishedRun {
    pub(crate) job_id: String,
    pub(crate) title: String,
    pub(crate) info: JobRunInfo,
    pub(crate) notice: Option<Notice>,
}

pub(crate) struct JobsManager {
    active: bool,
    paths: JobsPaths,
    entries: BTreeMap<String, Entry>,
    signature: Option<Signature>,
    revision: u64,
    cache: Arc<LoginEnvCache>,
    zone: Box<dyn UtcOffset + Send + Sync>,
    /// Jobs defined in code (`system.*`), merged with file and plugin jobs on
    /// every reload.
    system_jobs: Vec<JobDef>,
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn modified_ms(path: &Path) -> Option<(u64, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(0))
        .unwrap_or(0);
    Some((modified, metadata.len()))
}

struct UserJobDir {
    name: String,
    dir: PathBuf,
    file: PathBuf,
    modified_ms: u64,
    len: u64,
}

/// The `<id>/job.toml` files under the jobs directory, sorted by id.
fn scan_user_jobs(config_dir: &Path) -> Vec<UserJobDir> {
    let Ok(read_dir) = std::fs::read_dir(config_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let file = dir.join("job.toml");
        let Some((modified, len)) = modified_ms(&file) else {
            continue;
        };
        found.push(UserJobDir {
            name,
            dir,
            file,
            modified_ms: modified,
            len,
        });
    }
    found.sort_by(|left, right| left.name.cmp(&right.name));
    found
}

impl JobsManager {
    pub(crate) fn new(paths: JobsPaths, cache: Arc<LoginEnvCache>, active: bool) -> Self {
        Self {
            active,
            paths,
            entries: BTreeMap::new(),
            signature: None,
            revision: 1,
            cache,
            zone: Box::new(SystemLocal),
            system_jobs: Vec::new(),
        }
    }

    /// Register a built-in job (id must start with `system.`). It takes effect
    /// at the next `reload`.
    #[allow(dead_code)] // first caller arrives with the system jobs phase
    pub(crate) fn register_system_job(&mut self, job: JobDef) {
        debug_assert!(job.id.starts_with("system.") && job.source == JobSource::System);
        self.system_jobs.retain(|existing| existing.id != job.id);
        self.system_jobs.push(job);
        self.signature = None;
    }

    #[cfg(test)]
    pub(crate) fn with_zone(mut self, zone: Box<dyn UtcOffset + Send + Sync>) -> Self {
        self.zone = zone;
        self
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Advance the revision without a job change, for state that rides along
    /// with the job list (the environment status).
    pub(crate) fn touch_revision(&mut self) {
        self.revision += 1;
    }

    pub(crate) fn cache(&self) -> Arc<LoginEnvCache> {
        Arc::clone(&self.cache)
    }

    pub(crate) fn env_status(&self) -> JobEnvStatus {
        self.cache.status()
    }

    pub(crate) fn state_dir_for(&self, job_id: &str) -> PathBuf {
        state::state_dir_for(&self.paths.state_root, job_id)
    }

    fn signature(&self, plugins: &[PluginJobSource]) -> Signature {
        let mut signature: Signature = scan_user_jobs(&self.paths.config_dir)
            .into_iter()
            .map(|job| (job.name, job.modified_ms, job.len))
            .collect();
        for plugin in plugins {
            let (modified, len) = modified_ms(&plugin.manifest_path).unwrap_or((0, 0));
            signature.push((format!("plugin:{}", plugin.plugin_id), modified, len));
        }
        signature
    }

    /// Re-read job files when they changed (or when `force` is set). Cheap
    /// when nothing changed: a directory listing and one `stat` per job.
    /// Returns whether the job set changed.
    pub(crate) fn reload(&mut self, now_ms: u64, plugins: &[PluginJobSource], force: bool) -> bool {
        if !self.active {
            return false;
        }
        let signature = self.signature(plugins);
        if !force && self.signature.as_ref() == Some(&signature) {
            return false;
        }
        let first_load = self.signature.is_none();
        self.signature = Some(signature);

        let mut definitions: Vec<(String, Definition, u64)> = Vec::new();
        for job in scan_user_jobs(&self.paths.config_dir) {
            let id = job.name.clone();
            let definition = if !valid_job_id(&job.name) {
                Definition::Invalid(InvalidJob {
                    id: id.clone(),
                    path: job.file.clone(),
                    diagnostics: vec![format!(
                        "directory name '{}' is not a valid job id; use letters, digits, '.', '_' and '-'",
                        job.name
                    )],
                    source: JobSource::User,
                })
            } else {
                match std::fs::read_to_string(&job.file) {
                    Err(err) => Definition::Invalid(InvalidJob {
                        id: id.clone(),
                        path: job.file.clone(),
                        diagnostics: vec![format!("cannot read job.toml: {err}")],
                        source: JobSource::User,
                    }),
                    Ok(content) => match parse_job_toml(&content, &job.name, &job.dir) {
                        Ok(def) => Definition::Valid(Box::new(def)),
                        Err(diagnostics) => Definition::Invalid(InvalidJob {
                            id: id.clone(),
                            path: job.file.clone(),
                            diagnostics,
                            source: JobSource::User,
                        }),
                    },
                }
            };
            definitions.push((id, definition, job.modified_ms));
        }
        for job in &self.system_jobs {
            definitions.push((
                job.id.clone(),
                Definition::Valid(Box::new(job.clone())),
                now_ms,
            ));
        }
        for plugin in plugins {
            let modified = modified_ms(&plugin.manifest_path).map_or(0, |(modified, _)| modified);
            let content = match std::fs::read_to_string(&plugin.manifest_path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            match parse_plugin_manifest_jobs(
                &content,
                &plugin.plugin_id,
                &plugin.plugin_root,
                &plugin.manifest_path,
            ) {
                Ok(results) => {
                    for result in results {
                        match result {
                            Ok(def) => {
                                definitions.push((
                                    def.id.clone(),
                                    Definition::Valid(Box::new(def)),
                                    modified,
                                ));
                            }
                            Err(invalid) => {
                                definitions.push((
                                    invalid.id.clone(),
                                    Definition::Invalid(invalid),
                                    modified,
                                ));
                            }
                        }
                    }
                }
                Err(err) => {
                    let id = format!("{}:jobs", plugin.plugin_id);
                    definitions.push((
                        id.clone(),
                        Definition::Invalid(InvalidJob {
                            id,
                            path: plugin.manifest_path.clone(),
                            diagnostics: vec![format!("cannot parse the manifest: {err}")],
                            source: JobSource::Plugin {
                                plugin_id: plugin.plugin_id.clone(),
                                plugin_root: plugin.plugin_root.clone(),
                            },
                        }),
                        modified,
                    ));
                }
            }
        }

        let mut previous = std::mem::take(&mut self.entries);
        let mut entries = BTreeMap::new();
        for (id, definition, file_modified_ms) in definitions {
            if entries.contains_key(&id) {
                // Two sources claim one id; the first (user jobs sort first) wins.
                tracing::warn!(job = %id, "duplicate job id ignored");
                continue;
            }
            let carried = previous.remove(&id);
            let (state, running) = match carried {
                Some(old) => (old.state, old.running),
                None => (
                    state::load(&state::state_file_path(&self.paths.state_root, &id)),
                    None,
                ),
            };
            let mut entry = Entry {
                definition,
                state,
                running,
                next_due_ms: None,
                catch_up: false,
                file_modified_ms,
            };
            self.compute_initial_due(&mut entry, now_ms, first_load);
            entries.insert(id, entry);
        }
        self.entries = entries;
        self.revision += 1;
        true
    }

    /// Work out when a freshly loaded job is next due. A job whose last
    /// scheduled time passed since its last run (or, for a job that never
    /// ran, since its file was written) is due now: it runs once, whatever
    /// number of times it was missed.
    fn compute_initial_due(&self, entry: &mut Entry, now_ms: u64, first_load: bool) {
        let Some(def) = entry.def() else {
            entry.next_due_ms = None;
            return;
        };
        if !entry.enabled() {
            entry.next_due_ms = None;
            return;
        }
        let anchor = entry
            .state
            .last_started_unix_ms()
            .unwrap_or(entry.file_modified_ms.min(now_ms));
        let due = self.next_after_ms(&def.schedule, anchor);
        entry.next_due_ms = due;
        entry.catch_up = first_load && due.is_some_and(|due| due <= now_ms);
    }

    fn next_after_ms(&self, schedule: &Schedule, after_ms: u64) -> Option<u64> {
        let after_secs = i64::try_from(after_ms / 1000).ok()?;
        let next = schedule.next_after(after_secs, self.zone.as_ref())?;
        u64::try_from(next).ok().map(|secs| secs * 1000)
    }

    /// Jobs due at `now_ms`. A due job that is still running is recorded as
    /// skipped and rescheduled; the rest are returned for the caller to start
    /// with `begin_run`.
    pub(crate) fn take_due(&mut self, now_ms: u64) -> Vec<(String, JobRunTrigger)> {
        if !self.active {
            return Vec::new();
        }
        let mut due = Vec::new();
        let mut skipped = Vec::new();
        for (id, entry) in &mut self.entries {
            let Some(due_ms) = entry.next_due_ms else {
                continue;
            };
            if due_ms > now_ms || !entry.enabled() {
                continue;
            }
            if entry.running.is_some() {
                skipped.push(id.clone());
            } else {
                let trigger = if std::mem::take(&mut entry.catch_up) {
                    JobRunTrigger::CatchUp
                } else {
                    JobRunTrigger::Schedule
                };
                due.push((id.clone(), trigger));
            }
        }
        for id in skipped {
            self.record_skip(&id, now_ms);
        }
        due
    }

    fn record_skip(&mut self, job_id: &str, now_ms: u64) {
        let next = self
            .entries
            .get(job_id)
            .and_then(Entry::def)
            .and_then(|def| self.next_after_ms(&def.schedule, now_ms));
        let Some(entry) = self.entries.get_mut(job_id) else {
            return;
        };
        let run_id = entry.state.next_run_id();
        entry.state.push(RunRecord::skipped(run_id, now_ms));
        entry.next_due_ms = next;
        self.persist(job_id);
        self.revision += 1;
    }

    /// Mark the job running and describe how to run it. A run that is
    /// already going makes a scheduled trigger a recorded skip, and a manual
    /// one an error.
    pub(crate) fn begin_run(
        &mut self,
        job_id: &str,
        trigger: JobRunTrigger,
        now_ms: u64,
    ) -> Result<RunPlan, BeginError> {
        let Some(entry) = self.entries.get(job_id) else {
            return Err(BeginError::NotFound);
        };
        let job = match &entry.definition {
            Definition::Valid(job) => (**job).clone(),
            Definition::Invalid(invalid) => {
                return Err(BeginError::Invalid(invalid.diagnostics.clone()))
            }
        };
        if entry.running.is_some() {
            if trigger != JobRunTrigger::Manual {
                self.record_skip(job_id, now_ms);
            }
            return Err(BeginError::AlreadyRunning);
        }
        let next = self.next_after_ms(&job.schedule, now_ms);
        let state_dir = self.state_dir_for(job_id);
        let Some(entry) = self.entries.get_mut(job_id) else {
            return Err(BeginError::NotFound);
        };
        let run_id = entry.state.next_run_id();
        entry.running = Some(Running {
            started_unix_ms: now_ms,
        });
        entry.catch_up = false;
        if entry.enabled() {
            entry.next_due_ms = next;
        }
        self.revision += 1;
        Ok(RunPlan {
            job,
            run_id,
            trigger,
            state_dir,
            started_unix_ms: now_ms,
        })
    }

    /// Record a finished run: persist it and decide whether to notify.
    pub(crate) fn finish_run(&mut self, result: JobRunResult) -> Option<FinishedRun> {
        let entry = self.entries.get_mut(&result.job_id)?;
        let title = entry
            .def()
            .map_or_else(|| result.job_id.clone(), |def| def.title.clone());
        let policy = entry.def().map_or(NotifyPolicy::Never, |def| def.notify);
        entry.running = None;
        let previous = entry.state.last_executed().cloned();
        let info = result.record.info.clone();
        let notice = notice_for(policy, &title, &result.record, previous.as_ref());
        entry.state.push(result.record);
        self.persist(&result.job_id);
        self.revision += 1;
        Some(FinishedRun {
            job_id: result.job_id,
            title,
            info,
            notice,
        })
    }

    fn persist(&self, job_id: &str) {
        let Some(entry) = self.entries.get(job_id) else {
            return;
        };
        let path = state::state_file_path(&self.paths.state_root, job_id);
        if let Err(err) = state::save(&path, &entry.state) {
            tracing::warn!(job = %job_id, path = %path.display(), err = %err, "failed to save job state");
        }
    }

    /// Set the enabled override. Enabling restarts the schedule from `now_ms`
    /// so a job that was off for a week does not run at once.
    pub(crate) fn set_enabled(
        &mut self,
        job_id: &str,
        enabled: bool,
        now_ms: u64,
    ) -> Result<JobInfo, BeginError> {
        let Some(entry) = self.entries.get(job_id) else {
            return Err(BeginError::NotFound);
        };
        let Some(def) = entry.def() else {
            return Err(BeginError::Invalid(match &entry.definition {
                Definition::Invalid(invalid) => invalid.diagnostics.clone(),
                Definition::Valid(_) => Vec::new(),
            }));
        };
        let file_value = def.enabled;
        let schedule = def.schedule.clone();
        let next = enabled
            .then(|| self.next_after_ms(&schedule, now_ms))
            .flatten();
        let Some(entry) = self.entries.get_mut(job_id) else {
            return Err(BeginError::NotFound);
        };
        // An override equal to the file's value adds nothing; clearing it lets
        // later edits to `enabled` in the file take effect.
        entry.state.enabled_override = (enabled != file_value).then_some(enabled);
        entry.next_due_ms = next;
        entry.catch_up = false;
        self.persist(job_id);
        self.revision += 1;
        self.info(job_id, now_ms).ok_or(BeginError::NotFound)
    }

    /// The soonest instant something needs attention, for the caller's timer.
    pub(crate) fn next_deadline_ms(&self) -> Option<u64> {
        if !self.active {
            return None;
        }
        self.entries
            .values()
            .filter(|entry| entry.enabled())
            .filter_map(|entry| entry.next_due_ms)
            .min()
    }

    fn job_info(&self, id: &str, entry: &Entry) -> JobInfo {
        let last_run = entry.state.history.last().map(|record| record.info.clone());
        match &entry.definition {
            Definition::Invalid(invalid) => JobInfo {
                job_id: id.to_owned(),
                title: id.to_owned(),
                source: source_text(&invalid.source),
                command: Vec::new(),
                schedule: String::new(),
                schedule_kind: JobScheduleKind::Interval,
                timeout_secs: 0,
                enabled: false,
                enabled_overridden: false,
                env: String::new(),
                notify: String::new(),
                state: JobState::Invalid,
                next_run_unix_ms: None,
                running_since_unix_ms: None,
                last_run,
                has_feed: false,
                diagnostics: invalid.diagnostics.clone(),
                dir: invalid
                    .path
                    .parent()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default(),
            },
            Definition::Valid(def) => {
                let enabled = entry.enabled();
                let state = if entry.running.is_some() {
                    JobState::Running
                } else if !enabled {
                    JobState::Disabled
                } else {
                    JobState::Idle
                };
                JobInfo {
                    job_id: id.to_owned(),
                    title: def.title.clone(),
                    source: source_text(&def.source),
                    command: def.command.clone(),
                    schedule: def.schedule.describe(),
                    schedule_kind: match def.schedule {
                        Schedule::Cron(_) => JobScheduleKind::Cron,
                        Schedule::Every(_) => JobScheduleKind::Interval,
                    },
                    timeout_secs: def.timeout.as_secs(),
                    enabled,
                    enabled_overridden: entry.state.enabled_override.is_some(),
                    env: def.env.as_str().to_owned(),
                    notify: def.notify.as_str().to_owned(),
                    state,
                    next_run_unix_ms: enabled.then_some(entry.next_due_ms).flatten(),
                    running_since_unix_ms: entry.running.as_ref().map(|r| r.started_unix_ms),
                    last_run,
                    has_feed: def.feed.is_some(),
                    diagnostics: Vec::new(),
                    dir: def.dir.display().to_string(),
                }
            }
        }
    }

    pub(crate) fn infos(&self, _now_ms: u64) -> Vec<JobInfo> {
        self.entries
            .iter()
            .map(|(id, entry)| self.job_info(id, entry))
            .collect()
    }

    pub(crate) fn info(&self, job_id: &str, _now_ms: u64) -> Option<JobInfo> {
        self.entries
            .get(job_id)
            .map(|entry| self.job_info(job_id, entry))
    }

    /// Oldest first, without output.
    pub(crate) fn history(&self, job_id: &str) -> Option<Vec<JobRunInfo>> {
        self.entries.get(job_id).map(|entry| {
            entry
                .state
                .history
                .iter()
                .map(|record| record.info.clone())
                .collect()
        })
    }

    /// The record for `run_id`, or the latest executed run.
    pub(crate) fn run_record(&self, job_id: &str, run_id: Option<u64>) -> Option<&RunRecord> {
        let entry = self.entries.get(job_id)?;
        match run_id {
            Some(run_id) => entry
                .state
                .history
                .iter()
                .find(|record| record.info.run_id == run_id),
            None => entry.state.last_executed(),
        }
    }

    /// The definition of a valid job.
    pub(crate) fn job_def(&self, job_id: &str) -> Option<&JobDef> {
        self.entries.get(job_id).and_then(Entry::def)
    }

    /// The valid jobs that declare a feed, in id order.
    pub(crate) fn feed_jobs(&self) -> Vec<FeedJobRef> {
        self.entries
            .iter()
            .filter_map(|(id, entry)| {
                let def = entry.def()?;
                let feed = def.feed.as_ref()?;
                Some(FeedJobRef {
                    job_id: id.clone(),
                    job_title: def.title.clone(),
                    feed_title: feed.title.clone(),
                    enabled: entry.enabled(),
                    running: entry.running.is_some(),
                })
            })
            .collect()
    }

    pub(crate) fn has_job(&self, job_id: &str) -> bool {
        self.entries.contains_key(job_id)
    }

    pub(crate) fn is_running(&self, job_id: &str) -> bool {
        self.entries
            .get(job_id)
            .is_some_and(|entry| entry.running.is_some())
    }

    /// Running state of every job, for tests and shutdown handling.
    #[cfg(test)]
    pub(crate) fn running_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.running.is_some())
            .count()
    }
}

fn source_text(source: &JobSource) -> String {
    match source {
        JobSource::User => "user".into(),
        JobSource::System => "system".into(),
        JobSource::Plugin { plugin_id, .. } => format!("plugin:{plugin_id}"),
    }
}

fn format_run_duration(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        format!("{duration_ms}ms")
    } else if duration_ms < 10_000 {
        format!("{:.1}s", duration_ms as f64 / 1000.0)
    } else {
        format_duration(std::time::Duration::from_secs(duration_ms / 1000))
    }
}

fn last_nonempty_line(text: &str) -> Option<&str> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Decide whether a finished run earns a notification and what it says.
///
/// - `never`: no.
/// - `on_failure`: when the run did not succeed.
/// - `on_change`: when the result differs from the previous run's (status,
///   or stdout text); the first run only when it failed.
/// - `always`: every run.
pub(crate) fn notice_for(
    policy: NotifyPolicy,
    title: &str,
    record: &RunRecord,
    previous: Option<&RunRecord>,
) -> Option<Notice> {
    let success = record.info.status == JobRunStatus::Success;
    let notify = match policy {
        NotifyPolicy::Never => false,
        NotifyPolicy::OnFailure => !success,
        NotifyPolicy::Always => true,
        NotifyPolicy::OnChange => match previous {
            None => !success,
            Some(previous) => {
                previous.info.status != record.info.status
                    || previous.stdout_tail.trim() != record.stdout_tail.trim()
            }
        },
    };
    if !notify {
        return None;
    }
    let took = format_run_duration(record.info.duration_ms);
    let mut body = if success {
        format!("ok in {took}")
    } else {
        match record.info.status {
            JobRunStatus::TimedOut | JobRunStatus::SpawnFailed => record.info.summary.clone(),
            _ => format!("failed ({}) in {took}", record.info.summary),
        }
    };
    let detail = if success {
        last_nonempty_line(&record.stdout_tail)
    } else {
        last_nonempty_line(&record.stderr_tail).or_else(|| last_nonempty_line(&record.stdout_tail))
    };
    if let Some(detail) = detail {
        body.push_str(" · ");
        body.push_str(&clip(detail, 80));
    }
    Some(Notice {
        title: title.to_owned(),
        body,
        failed: !success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const UTC: fn(i64) -> i32 = |_| 0;

    struct Fixture {
        root: PathBuf,
        manager: JobsManager,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "flockr-jobs-mgr-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(root.join("config")).unwrap();
            let manager = Self::manager_at(&root);
            Self { root, manager }
        }

        fn manager_at(root: &Path) -> JobsManager {
            JobsManager::new(
                JobsPaths {
                    config_dir: root.join("config"),
                    state_root: root.join("state"),
                },
                Arc::new(LoginEnvCache::new(Box::new(|| {
                    Ok(vec![("PATH".into(), "/usr/bin:/bin".into())])
                }))),
                true,
            )
            .with_zone(Box::new(UTC))
        }

        fn write_job(&self, id: &str, body: &str) {
            let dir = self.root.join("config").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let mut file = std::fs::File::create(dir.join("job.toml")).unwrap();
            file.write_all(body.as_bytes()).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn result(
        job_id: &str,
        run_id: u64,
        status: JobRunStatus,
        started: u64,
        stdout: &str,
    ) -> JobRunResult {
        JobRunResult {
            job_id: job_id.into(),
            record: RunRecord {
                info: JobRunInfo {
                    run_id,
                    trigger: JobRunTrigger::Schedule,
                    status,
                    started_unix_ms: started,
                    finished_unix_ms: started + 1500,
                    duration_ms: 1500,
                    exit_code: Some(i32::from(status != JobRunStatus::Success)),
                    summary: if status == JobRunStatus::Success {
                        "exit 0".into()
                    } else {
                        "exit 1".into()
                    },
                    error: None,
                    skipped_count: 0,
                    env_note: None,
                },
                stdout_tail: stdout.into(),
                stderr_tail: if status == JobRunStatus::Success {
                    String::new()
                } else {
                    "it broke\n".into()
                },
                stdout_truncated: false,
                stderr_truncated: false,
                handoff: None,
            },
        }
    }

    const EVERY_HOUR: &str = "command = [\"echo\"]\nevery = \"1h\"\n";
    const HOUR_MS: u64 = 3_600_000;

    #[test]
    fn loads_user_jobs_and_reports_invalid_ones_with_diagnostics() {
        let mut fx = Fixture::new("load");
        fx.write_job("good", EVERY_HOUR);
        fx.write_job("broken", "command = []\n");
        std::fs::create_dir_all(fx.root.join("config/no-file")).unwrap();
        std::fs::create_dir_all(fx.root.join("config/bad id")).unwrap();
        std::fs::write(fx.root.join("config/bad id/job.toml"), EVERY_HOUR).unwrap();
        assert!(fx.manager.reload(1_000_000, &[], false));

        let infos = fx.manager.infos(1_000_000);
        let ids: Vec<_> = infos.iter().map(|info| info.job_id.as_str()).collect();
        assert_eq!(ids, vec!["bad id", "broken", "good"]);
        let good = fx.manager.info("good", 0).unwrap();
        assert_eq!(good.state, JobState::Idle);
        assert_eq!(good.schedule, "every 1h");
        let broken = fx.manager.info("broken", 0).unwrap();
        assert_eq!(broken.state, JobState::Invalid);
        assert!(broken
            .diagnostics
            .iter()
            .any(|d| d.starts_with("command: required")));
        assert!(broken
            .diagnostics
            .iter()
            .any(|d| d.starts_with("schedule/every: set exactly one")));
        let bad_id = fx.manager.info("bad id", 0).unwrap();
        assert_eq!(bad_id.state, JobState::Invalid);
        assert!(bad_id.diagnostics[0].contains("not a valid job id"));
    }

    #[test]
    fn an_inactive_manager_does_nothing() {
        let fx = Fixture::new("inactive");
        fx.write_job("good", EVERY_HOUR);
        let mut manager = JobsManager::new(
            JobsPaths {
                config_dir: fx.root.join("config"),
                state_root: fx.root.join("state"),
            },
            Arc::new(LoginEnvCache::new(Box::new(|| Ok(Vec::new())))),
            false,
        );
        assert!(!manager.reload(0, &[], true));
        assert!(manager.infos(0).is_empty());
        assert!(manager.take_due(u64::MAX).is_empty());
        assert_eq!(manager.next_deadline_ms(), None);
    }

    #[test]
    fn reload_is_a_no_op_until_a_file_changes() {
        let mut fx = Fixture::new("hot");
        fx.write_job("a", EVERY_HOUR);
        assert!(fx.manager.reload(0, &[], false));
        let revision = fx.manager.revision();
        assert!(!fx.manager.reload(0, &[], false), "unchanged signature");
        assert_eq!(fx.manager.revision(), revision);

        // A new job appears and an existing one is edited (length changes).
        fx.write_job("b", EVERY_HOUR);
        fx.write_job(
            "a",
            "command = [\"echo\"]\nevery = \"2h\"\ntitle = \"Renamed\"\n",
        );
        assert!(fx.manager.reload(0, &[], false));
        assert_eq!(fx.manager.info("a", 0).unwrap().title, "Renamed");
        assert!(fx.manager.has_job("b"));

        // A job removed from disk disappears.
        std::fs::remove_dir_all(fx.root.join("config/b")).unwrap();
        assert!(fx.manager.reload(0, &[], false));
        assert!(!fx.manager.has_job("b"));
        // Force re-reads even with an identical signature.
        assert!(fx.manager.reload(0, &[], true));
    }

    #[test]
    fn a_new_interval_job_first_runs_one_interval_after_it_was_created() {
        let mut fx = Fixture::new("interval");
        fx.write_job("tick", EVERY_HOUR);
        let t0 = 10 * HOUR_MS;
        fx.manager.reload(t0, &[], false);
        let first_due = fx.manager.next_deadline_ms().unwrap();
        assert_eq!(first_due, t0 + HOUR_MS);
        assert!(fx.manager.take_due(first_due - 1).is_empty());
        assert_eq!(
            fx.manager.take_due(first_due),
            vec![("tick".to_string(), JobRunTrigger::Schedule)]
        );
        // After a run starts, the next one is an interval after that start.
        fx.manager
            .begin_run("tick", JobRunTrigger::Schedule, first_due + 2000)
            .unwrap();
        assert_eq!(
            fx.manager.next_deadline_ms(),
            Some(first_due + 2000 + HOUR_MS)
        );
    }

    #[test]
    fn catch_up_runs_once_for_any_number_of_missed_intervals() {
        let mut fx = Fixture::new("catchup");
        fx.write_job("tick", EVERY_HOUR);
        // Seed state: last run started 5 hours ago.
        let now = 100 * HOUR_MS;
        let state_path = state::state_file_path(&fx.root.join("state"), "tick");
        let mut seeded = JobStateFile::default();
        let id = seeded.next_run_id();
        seeded.push(result("tick", id, JobRunStatus::Success, now - 5 * HOUR_MS, "").record);
        state::save(&state_path, &seeded).unwrap();

        fx.manager.reload(now, &[], false);
        let due = fx.manager.take_due(now);
        assert_eq!(due, vec![("tick".to_string(), JobRunTrigger::CatchUp)]);
        let plan = fx
            .manager
            .begin_run("tick", JobRunTrigger::CatchUp, now)
            .unwrap();
        assert_eq!(plan.trigger, JobRunTrigger::CatchUp);
        assert_eq!(plan.run_id, 2);
        // Not due again until an hour after the catch-up run started, and no
        // second catch-up fires for the other four missed hours.
        assert_eq!(fx.manager.next_deadline_ms(), Some(now + HOUR_MS));
        assert!(fx.manager.take_due(now + 1).is_empty());
    }

    #[test]
    fn nothing_is_caught_up_when_the_last_run_was_recent_enough() {
        let mut fx = Fixture::new("nocatchup");
        fx.write_job("tick", EVERY_HOUR);
        let now = 100 * HOUR_MS;
        let state_path = state::state_file_path(&fx.root.join("state"), "tick");
        let mut seeded = JobStateFile::default();
        let id = seeded.next_run_id();
        seeded.push(result("tick", id, JobRunStatus::Success, now - 10 * 60_000, "").record);
        state::save(&state_path, &seeded).unwrap();
        fx.manager.reload(now, &[], false);
        assert!(fx.manager.take_due(now).is_empty());
        assert_eq!(
            fx.manager.next_deadline_ms(),
            Some(now - 10 * 60_000 + HOUR_MS)
        );
    }

    #[test]
    fn cron_catch_up_uses_the_last_scheduled_time_not_the_last_run() {
        let mut fx = Fixture::new("cron-catchup");
        fx.write_job("daily", "command = [\"echo\"]\nschedule = \"0 10 * * *\"\n");
        let day = 86_400_000u64;
        // Last ran at 10:00 on day 50; flockr starts at 09:00 on day 52. The
        // 10:00 runs of days 51 were missed and 52 is still ahead.
        let last = 50 * day + 10 * HOUR_MS;
        let now = 52 * day + 9 * HOUR_MS;
        let state_path = state::state_file_path(&fx.root.join("state"), "daily");
        let mut seeded = JobStateFile::default();
        let id = seeded.next_run_id();
        seeded.push(result("daily", id, JobRunStatus::Success, last, "").record);
        state::save(&state_path, &seeded).unwrap();
        fx.manager.reload(now, &[], false);
        assert_eq!(
            fx.manager.take_due(now),
            vec![("daily".to_string(), JobRunTrigger::CatchUp)]
        );
        fx.manager
            .begin_run("daily", JobRunTrigger::CatchUp, now)
            .unwrap();
        assert_eq!(fx.manager.next_deadline_ms(), Some(52 * day + 10 * HOUR_MS));
    }

    #[test]
    fn overlap_is_skipped_and_recorded_once_per_trigger() {
        let mut fx = Fixture::new("overlap");
        fx.write_job("slow", EVERY_HOUR);
        let t = 100 * HOUR_MS;
        fx.manager.reload(t, &[], false);
        let plan = fx
            .manager
            .begin_run("slow", JobRunTrigger::Manual, t)
            .unwrap();
        assert_eq!(fx.manager.running_count(), 1);
        assert_eq!(fx.manager.info("slow", t).unwrap().state, JobState::Running);

        // A manual request while running is refused without a history entry.
        assert_eq!(
            fx.manager
                .begin_run("slow", JobRunTrigger::Manual, t + 1)
                .unwrap_err(),
            BeginError::AlreadyRunning
        );
        assert_eq!(fx.manager.history("slow").unwrap().len(), 0);

        // The schedule fires while the run is still going: skip and record it.
        let due_at = t + HOUR_MS;
        assert!(fx.manager.take_due(due_at).is_empty());
        let history = fx.manager.history("slow").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, JobRunStatus::Skipped);
        assert_eq!(history[0].summary, "skipped: still running");
        // Rescheduled a full interval ahead; another skip folds into the entry.
        assert_eq!(fx.manager.next_deadline_ms(), Some(due_at + HOUR_MS));
        assert!(fx.manager.take_due(due_at + HOUR_MS).is_empty());
        let history = fx.manager.history("slow").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].skipped_count, 2);

        // When the run ends, the real result follows the folded skip entry.
        let finished = fx
            .manager
            .finish_run(result(
                "slow",
                plan.run_id,
                JobRunStatus::Success,
                t,
                "done\n",
            ))
            .unwrap();
        assert_eq!(finished.info.status, JobRunStatus::Success);
        assert_eq!(fx.manager.history("slow").unwrap().len(), 2);
        assert_eq!(fx.manager.running_count(), 0);
        let info = fx.manager.info("slow", 0).unwrap();
        assert_eq!(info.last_run.unwrap().status, JobRunStatus::Success);
    }

    #[test]
    fn scheduled_begin_while_running_is_a_recorded_skip() {
        let mut fx = Fixture::new("begin-skip");
        fx.write_job("slow", EVERY_HOUR);
        fx.manager.reload(0, &[], false);
        fx.manager
            .begin_run("slow", JobRunTrigger::Schedule, 5)
            .unwrap();
        let error = fx
            .manager
            .begin_run("slow", JobRunTrigger::Schedule, 9)
            .unwrap_err();
        assert_eq!(error, BeginError::AlreadyRunning);
        assert_eq!(
            fx.manager.history("slow").unwrap()[0].status,
            JobRunStatus::Skipped
        );
    }

    #[test]
    fn finished_runs_persist_and_reload_with_history() {
        let mut fx = Fixture::new("persist");
        fx.write_job("job", EVERY_HOUR);
        fx.manager.reload(0, &[], false);
        for n in 0..25u64 {
            let plan = fx
                .manager
                .begin_run("job", JobRunTrigger::Manual, n * 1000)
                .unwrap();
            fx.manager.finish_run(result(
                "job",
                plan.run_id,
                JobRunStatus::Success,
                n * 1000,
                &format!("run {n}\n"),
            ));
        }
        let history = fx.manager.history("job").unwrap();
        assert_eq!(history.len(), 20, "history is capped");
        assert_eq!(history.last().unwrap().run_id, 25);

        // A fresh manager (a server restart) sees the same history.
        let mut restarted = Fixture::manager_at(&fx.root);
        restarted.reload(1_000_000, &[], false);
        assert_eq!(restarted.history("job").unwrap(), history);
        let record = restarted.run_record("job", None).unwrap();
        assert_eq!(record.stdout_tail, "run 24\n");
        assert_eq!(
            restarted.run_record("job", Some(10)).unwrap().info.run_id,
            10
        );
        // Run ids continue instead of restarting.
        let plan = restarted
            .begin_run("job", JobRunTrigger::Manual, 1_000_000)
            .unwrap();
        assert_eq!(plan.run_id, 26);
    }

    #[test]
    fn enable_and_disable_override_the_file_and_persist() {
        let mut fx = Fixture::new("enable");
        fx.write_job("job", EVERY_HOUR);
        fx.manager.reload(0, &[], false);
        let t = 50 * HOUR_MS;
        let disabled = fx.manager.set_enabled("job", false, t).unwrap();
        assert!(!disabled.enabled);
        assert!(disabled.enabled_overridden);
        assert_eq!(disabled.state, JobState::Disabled);
        assert_eq!(disabled.next_run_unix_ms, None);
        assert_eq!(fx.manager.next_deadline_ms(), None);
        assert!(fx.manager.take_due(u64::MAX).is_empty());

        let mut restarted = Fixture::manager_at(&fx.root);
        restarted.reload(t, &[], false);
        assert_eq!(restarted.info("job", t).unwrap().state, JobState::Disabled);

        // Enabling restarts the schedule from now: no immediate run.
        let enabled = fx.manager.set_enabled("job", true, t).unwrap();
        assert!(enabled.enabled);
        assert!(
            !enabled.enabled_overridden,
            "equal to the file value, so cleared"
        );
        assert_eq!(enabled.next_run_unix_ms, Some(t + HOUR_MS));
        assert!(fx.manager.take_due(t).is_empty());

        assert_eq!(
            fx.manager.set_enabled("nope", true, t).unwrap_err(),
            BeginError::NotFound
        );
    }

    #[test]
    fn jobs_disabled_in_the_file_do_not_run_until_enabled() {
        let mut fx = Fixture::new("file-disabled");
        fx.write_job(
            "job",
            "command = [\"echo\"]\nevery = \"1h\"\nenabled = false\n",
        );
        fx.manager.reload(0, &[], false);
        assert_eq!(fx.manager.info("job", 0).unwrap().state, JobState::Disabled);
        assert!(fx.manager.take_due(u64::MAX).is_empty());
        // A manual run still works for a disabled job.
        assert!(fx
            .manager
            .begin_run("job", JobRunTrigger::Manual, 1)
            .is_ok());
        let info = fx.manager.set_enabled("job", true, 100).unwrap();
        assert!(info.enabled && info.enabled_overridden);
    }

    #[test]
    fn invalid_and_unknown_jobs_cannot_run() {
        let mut fx = Fixture::new("cannot-run");
        fx.write_job("broken", "command = []\n");
        fx.manager.reload(0, &[], false);
        assert!(matches!(
            fx.manager.begin_run("broken", JobRunTrigger::Manual, 0),
            Err(BeginError::Invalid(_))
        ));
        assert_eq!(
            fx.manager
                .begin_run("missing", JobRunTrigger::Manual, 0)
                .unwrap_err(),
            BeginError::NotFound
        );
    }

    #[test]
    fn plugin_manifest_jobs_load_with_namespaced_ids() {
        let mut fx = Fixture::new("plugin");
        let plugin_root = fx.root.join("plugin-root");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let manifest = plugin_root.join("flockr-plugin.toml");
        std::fs::write(
            &manifest,
            "id = \"acme.tools\"\nname = \"Acme\"\n[[jobs]]\nid = \"refresh\"\ncommand = [\"./refresh.sh\"]\nevery = \"30m\"\n",
        )
        .unwrap();
        let source = PluginJobSource {
            plugin_id: "acme.tools".into(),
            plugin_root: plugin_root.clone(),
            manifest_path: manifest.clone(),
        };
        fx.manager.reload(0, std::slice::from_ref(&source), false);
        let info = fx.manager.info("acme.tools:refresh", 0).unwrap();
        assert_eq!(info.source, "plugin:acme.tools");
        assert_eq!(info.dir, plugin_root.display().to_string());
        let plan = fx
            .manager
            .begin_run("acme.tools:refresh", JobRunTrigger::Manual, 5)
            .unwrap();
        assert_eq!(plan.job.cwd, plugin_root);
        assert!(plan
            .state_dir
            .to_string_lossy()
            .contains("acme.tools_refresh"));
        // Removing the plugin removes its jobs.
        fx.manager.reload(0, &[], false);
        assert!(!fx.manager.has_job("acme.tools:refresh"));
    }

    #[test]
    fn code_defined_system_jobs_sit_beside_file_jobs() {
        let mut fx = Fixture::new("system");
        fx.write_job("mine", EVERY_HOUR);
        let mut job = parse_job_toml(EVERY_HOUR, "x", Path::new("/tmp")).unwrap();
        job.id = "system.demo".into();
        job.source = JobSource::System;
        fx.manager.register_system_job(job);
        fx.manager.reload(0, &[], false);
        assert_eq!(fx.manager.info("system.demo", 0).unwrap().source, "system");
        assert!(fx.manager.has_job("mine"));
        assert!(fx
            .manager
            .begin_run("system.demo", JobRunTrigger::Manual, 1)
            .is_ok());
    }

    #[test]
    fn notify_never_and_always() {
        let ok = result("j", 1, JobRunStatus::Success, 0, "all good\n").record;
        let bad = result("j", 2, JobRunStatus::Failed, 0, "").record;
        assert_eq!(notice_for(NotifyPolicy::Never, "T", &bad, None), None);
        assert!(notice_for(NotifyPolicy::Always, "T", &ok, None).is_some());
        assert!(notice_for(NotifyPolicy::Always, "T", &bad, None).is_some());
    }

    #[test]
    fn notify_on_failure_only_for_unsuccessful_runs() {
        let ok = result("j", 1, JobRunStatus::Success, 0, "x\n").record;
        let bad = result("j", 2, JobRunStatus::Failed, 0, "").record;
        let timed_out = {
            let mut record = result("j", 3, JobRunStatus::TimedOut, 0, "").record;
            record.info.summary = "timed out after 10m".into();
            record
        };
        assert_eq!(notice_for(NotifyPolicy::OnFailure, "T", &ok, None), None);
        let notice = notice_for(NotifyPolicy::OnFailure, "Sync", &bad, Some(&ok)).unwrap();
        assert_eq!(notice.title, "Sync");
        assert_eq!(notice.body, "failed (exit 1) in 1.5s · it broke");
        let notice = notice_for(NotifyPolicy::OnFailure, "Sync", &timed_out, None).unwrap();
        assert!(notice.body.starts_with("timed out after 10m"), "{notice:?}");
        let spawn = {
            let mut record = result("j", 4, JobRunStatus::SpawnFailed, 0, "").record;
            record.info.summary = "could not start x: not found".into();
            record
        };
        let notice = notice_for(NotifyPolicy::OnFailure, "Sync", &spawn, None).unwrap();
        assert!(notice.body.starts_with("could not start x: not found"));
    }

    #[test]
    fn notify_on_change_compares_status_and_stdout() {
        let ok_a = result("j", 1, JobRunStatus::Success, 0, "3 items\n").record;
        let ok_a2 = result("j", 2, JobRunStatus::Success, 0, "3 items\n\n").record;
        let ok_b = result("j", 3, JobRunStatus::Success, 0, "4 items\n").record;
        let bad = result("j", 4, JobRunStatus::Failed, 0, "3 items\n").record;
        // First run: only a failure is news.
        assert_eq!(notice_for(NotifyPolicy::OnChange, "T", &ok_a, None), None);
        assert!(notice_for(NotifyPolicy::OnChange, "T", &bad, None).is_some());
        // Same status and same output (whitespace aside): quiet.
        assert_eq!(
            notice_for(NotifyPolicy::OnChange, "T", &ok_a2, Some(&ok_a)),
            None
        );
        // Different output, or a different status: notify.
        let changed = notice_for(NotifyPolicy::OnChange, "T", &ok_b, Some(&ok_a)).unwrap();
        assert_eq!(changed.body, "ok in 1.5s · 4 items");
        assert!(notice_for(NotifyPolicy::OnChange, "T", &bad, Some(&ok_a)).is_some());
        assert!(notice_for(NotifyPolicy::OnChange, "T", &ok_a, Some(&bad)).is_some());
    }

    #[test]
    fn finish_run_applies_the_job_notify_policy() {
        let mut fx = Fixture::new("finish-notify");
        fx.write_job(
            "job",
            "command = [\"echo\"]\nevery = \"1h\"\nnotify = \"always\"\ntitle = \"My Job\"\n",
        );
        fx.manager.reload(0, &[], false);
        let plan = fx
            .manager
            .begin_run("job", JobRunTrigger::Manual, 0)
            .unwrap();
        let finished = fx
            .manager
            .finish_run(result(
                "job",
                plan.run_id,
                JobRunStatus::Success,
                0,
                "hello\n",
            ))
            .unwrap();
        let notice = finished.notice.unwrap();
        assert_eq!(notice.title, "My Job");
        assert_eq!(notice.body, "ok in 1.5s · hello");
        // Results for jobs that no longer exist are dropped.
        assert!(fx
            .manager
            .finish_run(result("ghost", 1, JobRunStatus::Success, 0, ""))
            .is_none());
    }

    #[test]
    fn long_detail_lines_are_clipped() {
        let long = "x".repeat(200);
        let record = result("j", 1, JobRunStatus::Success, 0, &format!("{long}\n")).record;
        let notice = notice_for(NotifyPolicy::Always, "T", &record, None).unwrap();
        assert!(notice.body.chars().count() < 120);
        assert!(notice.body.ends_with('…'));
    }

    #[test]
    fn revision_advances_on_every_visible_change() {
        let mut fx = Fixture::new("revision");
        fx.write_job("job", EVERY_HOUR);
        fx.manager.reload(0, &[], false);
        let mut last = fx.manager.revision();
        let plan = fx
            .manager
            .begin_run("job", JobRunTrigger::Manual, 0)
            .unwrap();
        assert!(fx.manager.revision() > last);
        last = fx.manager.revision();
        fx.manager
            .finish_run(result("job", plan.run_id, JobRunStatus::Success, 0, ""));
        assert!(fx.manager.revision() > last);
        last = fx.manager.revision();
        fx.manager.set_enabled("job", false, 0).unwrap();
        assert!(fx.manager.revision() > last);
    }
}
