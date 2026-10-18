//! Per-job persisted state: the enabled override, the last 20 runs and the
//! tail of the last output. Stored as `<state_dir>/jobs/<id>/state.json`,
//! written atomically. Environment values are never part of it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::schema::{JobRunInfo, JobRunStatus, JobRunTrigger};

pub(crate) const HISTORY_LIMIT: usize = 20;
/// Bytes of each output stream kept per run.
pub(crate) const OUTPUT_TAIL_BYTES: usize = 16 * 1024;
const STATE_VERSION: u32 = 1;

/// A finished (or skipped) run with its output tails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunRecord {
    #[serde(flatten)]
    pub(crate) info: JobRunInfo,
    #[serde(default)]
    pub(crate) stdout_tail: String,
    #[serde(default)]
    pub(crate) stderr_tail: String,
    #[serde(default)]
    pub(crate) stdout_truncated: bool,
    #[serde(default)]
    pub(crate) stderr_truncated: bool,
    /// The JSON of a stdout line `{"flockr_handoff": {...}}`, kept verbatim;
    /// the handoff evaluation (`super::handoff`) reads it after the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) handoff: Option<serde_json::Value>,
}

/// A stdout line that starts with this is a dynamic handoff.
pub(crate) const HANDOFF_MARKER: &str = "{\"flockr_handoff\":";

/// Find the last stdout line that starts with `{"flockr_handoff":` and parses.
pub(crate) fn find_handoff_line(stdout: &str) -> Option<serde_json::Value> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| line.starts_with(HANDOFF_MARKER))
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .and_then(|value| value.get("flockr_handoff").cloned())
}

impl RunRecord {
    pub(crate) fn skipped(run_id: u64, now_ms: u64) -> Self {
        Self {
            info: JobRunInfo {
                run_id,
                trigger: JobRunTrigger::Schedule,
                status: JobRunStatus::Skipped,
                started_unix_ms: now_ms,
                finished_unix_ms: now_ms,
                duration_ms: 0,
                exit_code: None,
                summary: "skipped: still running".into(),
                error: None,
                skipped_count: 1,
                env_note: None,
            },
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            handoff: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JobStateFile {
    #[serde(default)]
    version: u32,
    /// Set by `flockr job enable/disable`; wins over the job file's `enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) enabled_override: Option<bool>,
    /// Set by `flockr job schedule` when the schedule could not be written to
    /// the job's own file (a plugin job, or a `job.toml` that cannot be edited
    /// safely). The text is what the user typed: `every 2h` or a cron
    /// expression. Wins over the job file's schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) schedule_override: Option<String>,
    /// Fingerprint of the previous run's output and feed, for
    /// `[handoff] when = "on_change"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) handoff_fingerprint: Option<String>,
    /// The last run id handed out.
    #[serde(default)]
    pub(crate) last_run_id: u64,
    /// Oldest first, at most `HISTORY_LIMIT`.
    #[serde(default)]
    pub(crate) history: Vec<RunRecord>,
}

impl Default for JobStateFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            enabled_override: None,
            schedule_override: None,
            handoff_fingerprint: None,
            last_run_id: 0,
            history: Vec::new(),
        }
    }
}

impl JobStateFile {
    pub(crate) fn next_run_id(&mut self) -> u64 {
        self.last_run_id += 1;
        self.last_run_id
    }

    /// The most recent run that actually executed (not a skip).
    pub(crate) fn last_executed(&self) -> Option<&RunRecord> {
        self.history
            .iter()
            .rev()
            .find(|record| record.info.status != JobRunStatus::Skipped)
    }

    /// Anchor for scheduling: when the last executed run started.
    pub(crate) fn last_started_unix_ms(&self) -> Option<u64> {
        self.last_executed()
            .map(|record| record.info.started_unix_ms)
    }

    /// Append a record, folding consecutive skips into one entry and keeping
    /// only the newest `HISTORY_LIMIT`.
    pub(crate) fn push(&mut self, record: RunRecord) {
        if record.info.status == JobRunStatus::Skipped {
            if let Some(last) = self.history.last_mut() {
                if last.info.status == JobRunStatus::Skipped {
                    last.info.skipped_count += record.info.skipped_count;
                    last.info.finished_unix_ms = record.info.finished_unix_ms;
                    return;
                }
            }
        }
        self.history.push(record);
        let extra = self.history.len().saturating_sub(HISTORY_LIMIT);
        if extra > 0 {
            self.history.drain(0..extra);
        }
    }
}

pub(crate) fn state_dir_for(jobs_state_root: &Path, job_id: &str) -> PathBuf {
    // Plugin job ids contain ':' which is not portable in file names.
    jobs_state_root.join(job_id.replace(':', "_"))
}

pub(crate) fn state_file_path(jobs_state_root: &Path, job_id: &str) -> PathBuf {
    state_dir_for(jobs_state_root, job_id).join("state.json")
}

/// Load a job's state. A missing file is a fresh state; an unreadable or
/// corrupt one is logged and replaced rather than blocking the job.
pub(crate) fn load(path: &Path) -> JobStateFile {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return JobStateFile::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), err = %err, "failed to read job state");
            return JobStateFile::default();
        }
    };
    match serde_json::from_str::<JobStateFile>(&content) {
        Ok(mut state) => {
            let extra = state.history.len().saturating_sub(HISTORY_LIMIT);
            if extra > 0 {
                state.history.drain(0..extra);
            }
            state.last_run_id = state.last_run_id.max(
                state
                    .history
                    .iter()
                    .map(|record| record.info.run_id)
                    .max()
                    .unwrap_or(0),
            );
            state
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), err = %err, "job state is corrupt; starting fresh");
            JobStateFile::default()
        }
    }
}

/// Write `state` atomically: a temporary file in the same directory, synced,
/// then renamed over the destination.
pub(crate) fn save(path: &Path, state: &JobStateFile) -> std::io::Result<()> {
    write_json_atomic(path, state)
}

/// Serialize `value` as pretty JSON and replace `path` with it atomically.
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    use std::io::Write;

    let Some(dir) = path.parent() else {
        return Err(std::io::Error::other("state path has no parent directory"));
    };
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let file_name = path.file_name().map_or_else(
        || "state".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    // A stale temporary from a crashed process with the same pid would make
    // the exclusive create below fail forever.
    let _ = std::fs::remove_file(&temporary);
    let result = (|| {
        let mut file = crate::platform::create_private_state_file(&temporary)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        crate::platform::replace_file(&temporary, path)?;
        crate::platform::sync_parent_directory(dir)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Keeps the last `cap` bytes written to it.
#[derive(Debug, Default)]
pub(crate) struct TailBuffer {
    bytes: Vec<u8>,
    cap: usize,
    pub(crate) truncated: bool,
}

impl TailBuffer {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            cap,
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        if self.cap == 0 {
            self.truncated |= !data.is_empty();
            return;
        }
        if data.len() >= self.cap {
            self.truncated |= !self.bytes.is_empty() || data.len() > self.cap;
            self.bytes.clear();
            self.bytes.extend_from_slice(&data[data.len() - self.cap..]);
            return;
        }
        self.bytes.extend_from_slice(data);
        if self.bytes.len() > self.cap * 2 {
            // Trim lazily so a chatty job does not memmove on every read.
            let excess = self.bytes.len() - self.cap;
            self.bytes.drain(0..excess);
            self.truncated = true;
        }
    }

    /// The kept tail as text, cut to whole lines when it was truncated.
    pub(crate) fn text(&self) -> (String, bool) {
        let mut start = self.bytes.len().saturating_sub(self.cap);
        let truncated = self.truncated || start > 0;
        let slice = &self.bytes[start..];
        if truncated {
            // Drop a partial first line (and any split multi-byte character).
            if let Some(newline) = slice.iter().position(|byte| *byte == b'\n') {
                start += newline + 1;
            }
        }
        (
            String::from_utf8_lossy(&self.bytes[start..]).into_owned(),
            truncated,
        )
    }
}

/// The last `lines` lines of `text`.
pub(crate) fn last_lines(text: &str, lines: usize) -> &str {
    if lines == 0 {
        return "";
    }
    let trimmed = text.trim_end_matches('\n');
    let mut seen = 0;
    for (index, byte) in trimmed.bytes().enumerate().rev() {
        if byte == b'\n' {
            seen += 1;
            if seen == lines {
                return &text[index + 1..];
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "flockr-jobs-state-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(run_id: u64, status: JobRunStatus, started: u64) -> RunRecord {
        RunRecord {
            info: JobRunInfo {
                run_id,
                trigger: JobRunTrigger::Schedule,
                status,
                started_unix_ms: started,
                finished_unix_ms: started + 10,
                duration_ms: 10,
                exit_code: Some(0),
                summary: "exit 0".into(),
                error: None,
                skipped_count: 0,
                env_note: None,
            },
            stdout_tail: format!("out {run_id}"),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            handoff: None,
        }
    }

    #[test]
    fn history_keeps_the_newest_twenty() {
        let mut state = JobStateFile::default();
        for run in 1..=25 {
            let id = state.next_run_id();
            state.push(record(id, JobRunStatus::Success, run * 1000));
        }
        assert_eq!(state.history.len(), HISTORY_LIMIT);
        assert_eq!(state.history.first().unwrap().info.run_id, 6);
        assert_eq!(state.history.last().unwrap().info.run_id, 25);
        assert_eq!(state.last_started_unix_ms(), Some(25_000));
    }

    #[test]
    fn consecutive_skips_fold_into_one_entry() {
        let mut state = JobStateFile::default();
        state.push(record(1, JobRunStatus::Success, 1000));
        for tick in 0..5 {
            state.push(RunRecord::skipped(100 + tick, 2000 + tick * 100));
        }
        assert_eq!(state.history.len(), 2);
        let skipped = state.history.last().unwrap();
        assert_eq!(skipped.info.status, JobRunStatus::Skipped);
        assert_eq!(skipped.info.skipped_count, 5);
        assert_eq!(skipped.info.summary, "skipped: still running");
        // A skip never moves the scheduling anchor.
        assert_eq!(state.last_started_unix_ms(), Some(1000));
        state.push(record(2, JobRunStatus::Failed, 9000));
        state.push(RunRecord::skipped(200, 9500));
        assert_eq!(state.history.len(), 4);
    }

    #[test]
    fn state_round_trips_atomically() {
        let dir = temp_dir("roundtrip");
        let path = state_file_path(&dir, "acme:job");
        assert!(path.to_string_lossy().contains("acme_job"));
        assert_eq!(load(&path), JobStateFile::default());

        let mut state = JobStateFile {
            enabled_override: Some(false),
            ..JobStateFile::default()
        };
        let id = state.next_run_id();
        state.push(record(id, JobRunStatus::Success, 1234));
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["state.json".to_string()]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_state_is_replaced_not_fatal() {
        let dir = temp_dir("corrupt");
        let path = dir.join("state.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), JobStateFile::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loading_repairs_run_ids_and_trims_oversized_history() {
        let dir = temp_dir("repair");
        let path = dir.join("state.json");
        let mut state = JobStateFile::default();
        for run in 1..=30u64 {
            state.history.push(record(run, JobRunStatus::Success, run));
        }
        state.last_run_id = 0;
        save(&path, &state).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.history.len(), HISTORY_LIMIT);
        assert_eq!(loaded.last_run_id, 30);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tail_buffer_keeps_the_last_bytes_on_whole_lines() {
        let mut tail = TailBuffer::new(16);
        tail.push(b"first line\n");
        tail.push(b"second line\n");
        tail.push(b"third\n");
        let (text, truncated) = tail.text();
        assert!(truncated);
        assert!(text.len() <= 16);
        assert!(text.ends_with("third\n"));
        assert!(
            !text.starts_with("irst"),
            "partial first line dropped: {text:?}"
        );

        let mut small = TailBuffer::new(64);
        small.push(b"hello\n");
        assert_eq!(small.text(), ("hello\n".to_string(), false));
    }

    #[test]
    fn tail_buffer_handles_one_huge_write() {
        let mut tail = TailBuffer::new(8);
        tail.push(&[b'x'; 1_000]);
        let (text, truncated) = tail.text();
        assert!(truncated);
        assert_eq!(text.len(), 8);
    }

    #[test]
    fn handoff_line_is_recognised_and_kept() {
        let out = "noise\n{\"flockr_handoff\": {\"ask\": \"go?\", \"context\": {\"tag\": \"v1\"}}}\ntrailer\n";
        let found = find_handoff_line(out).unwrap();
        assert_eq!(found["context"]["tag"], "v1");
        assert!(find_handoff_line("{\"flockr_handoff\": nope}\n").is_none());
        assert!(find_handoff_line("plain\n").is_none());
    }

    #[test]
    fn last_lines_returns_the_tail() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(last_lines("a\nb\nc", 2), "b\nc");
        assert_eq!(last_lines("a\nb\nc\n", 10), "a\nb\nc\n");
        assert_eq!(last_lines("a\nb\nc\n", 0), "");
    }
}
