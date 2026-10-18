//! Pending decisions: a handoff offer waits here until the user starts,
//! snoozes or dismisses it. The list survives restarts
//! (`<state_dir>/decisions.json`, written atomically), is capped, and
//! de-duplicates by job and content hash so a job that keeps printing the
//! same thing asks once.
//!
//! Pure with respect to the clock: every method that cares takes `now_ms`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::schema::{DecisionInfo, DecisionStatus};

/// Decisions kept, pending and resolved together. When the list is full the
/// oldest resolved one goes first, then the oldest unanswered one.
pub(crate) const MAX_DECISIONS: usize = 50;
/// How long Snooze hides a decision by default.
pub(crate) const DEFAULT_SNOOZE_SECS: u64 = 60 * 60;
/// The longest snooze a caller may ask for.
pub(crate) const MAX_SNOOZE_SECS: u64 = 30 * 24 * 60 * 60;
const STORE_VERSION: u32 = 1;
const STORE_FILE: &str = "decisions.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DecisionRecord {
    pub(crate) id: u64,
    pub(crate) job_id: String,
    pub(crate) job_title: String,
    pub(crate) run_id: u64,
    pub(crate) created_unix_ms: u64,
    pub(crate) ask: String,
    pub(crate) tab_title: String,
    pub(crate) cwd: String,
    pub(crate) agent_command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context: Option<serde_json::Value>,
    pub(crate) content_hash: String,
    pub(crate) status: DecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) snoozed_until_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
}

impl DecisionRecord {
    pub(crate) fn public_id(&self) -> String {
        format!("d{}", self.id)
    }

    fn unresolved(&self) -> bool {
        matches!(
            self.status,
            DecisionStatus::Pending | DecisionStatus::Snoozed
        )
    }

    pub(crate) fn info(&self) -> DecisionInfo {
        DecisionInfo {
            decision_id: self.public_id(),
            job_id: self.job_id.clone(),
            job_title: self.job_title.clone(),
            run_id: self.run_id,
            ask: self.ask.clone(),
            tab_title: self.tab_title.clone(),
            cwd: self.cwd.clone(),
            agent_command: self.agent_command.clone(),
            status: self.status,
            created_unix_ms: self.created_unix_ms,
            snoozed_until_unix_ms: self.snoozed_until_unix_ms,
            resolved_unix_ms: self.resolved_unix_ms,
            outcome: self.outcome.clone(),
            last_error: self.last_error.clone(),
            prompt_file: self.prompt_file.clone(),
            context: self.context.clone(),
            prompt: None,
        }
    }
}

/// What the handoff evaluation produced, ready to store.
#[derive(Debug, Clone)]
pub(crate) struct NewDecision {
    pub(crate) job_id: String,
    pub(crate) job_title: String,
    pub(crate) run_id: u64,
    pub(crate) ask: String,
    pub(crate) tab_title: String,
    pub(crate) cwd: String,
    pub(crate) agent_command: String,
    pub(crate) prompt_file: Option<String>,
    pub(crate) context: Option<serde_json::Value>,
    pub(crate) content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddOutcome {
    Created(u64),
    /// The same job already has a decision with this content.
    Duplicate(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecisionError {
    NotFound,
    /// Already started or dismissed.
    Resolved(DecisionStatus),
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such decision"),
            Self::Resolved(DecisionStatus::Started) => f.write_str("it was already started"),
            Self::Resolved(_) => f.write_str("it was already dismissed"),
        }
    }
}

/// `d12` or `12`.
pub(crate) fn parse_decision_id(text: &str) -> Option<u64> {
    let digits = text.trim().strip_prefix('d').unwrap_or(text.trim());
    digits.parse().ok()
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    decisions: Vec<DecisionRecord>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            next_id: 1,
            decisions: Vec::new(),
        }
    }
}

pub(crate) struct DecisionStore {
    /// `None` keeps everything in memory (tests, servers that run no jobs).
    path: Option<PathBuf>,
    file: StoreFile,
    revision: u64,
}

pub(crate) fn store_path() -> PathBuf {
    crate::config::state_dir().join(STORE_FILE)
}

impl DecisionStore {
    /// Load the list at `path`. A missing file is an empty list; a corrupt one
    /// is logged and replaced rather than blocking the server.
    pub(crate) fn open(path: PathBuf) -> Self {
        let file = match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<StoreFile>(&content) {
                Ok(mut file) => {
                    let highest = file.decisions.iter().map(|d| d.id).max().unwrap_or(0);
                    file.next_id = file.next_id.max(highest + 1).max(1);
                    file
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), err = %err, "decision list is corrupt; starting fresh");
                    StoreFile::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => StoreFile::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), err = %err, "failed to read the decision list");
                StoreFile::default()
            }
        };
        Self {
            path: Some(path),
            file,
            revision: 1,
        }
    }

    /// A store that never touches the disk.
    pub(crate) fn in_memory() -> Self {
        Self {
            path: None,
            file: StoreFile::default(),
            revision: 1,
        }
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(err) = super::state::write_json_atomic(path, &self.file) {
            tracing::warn!(path = %path.display(), err = %err, "failed to save the decision list");
        }
    }

    fn changed(&mut self) {
        self.revision += 1;
        self.persist();
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether `job_id` already has a decision with this content, resolved or
    /// not. Checked before the prompt file is written.
    pub(crate) fn find_duplicate(&self, job_id: &str, content_hash: &str) -> Option<u64> {
        self.file
            .decisions
            .iter()
            .find(|d| d.job_id == job_id && d.content_hash == content_hash)
            .map(|d| d.id)
    }

    pub(crate) fn add(&mut self, new: NewDecision, now_ms: u64) -> AddOutcome {
        if let Some(existing) = self.find_duplicate(&new.job_id, &new.content_hash) {
            return AddOutcome::Duplicate(existing);
        }
        let id = self.file.next_id;
        self.file.next_id += 1;
        self.file.decisions.push(DecisionRecord {
            id,
            job_id: new.job_id,
            job_title: new.job_title,
            run_id: new.run_id,
            created_unix_ms: now_ms,
            ask: new.ask,
            tab_title: new.tab_title,
            cwd: new.cwd,
            agent_command: new.agent_command,
            prompt_file: new.prompt_file,
            context: new.context,
            content_hash: new.content_hash,
            status: DecisionStatus::Pending,
            snoozed_until_unix_ms: None,
            resolved_unix_ms: None,
            outcome: None,
            last_error: None,
        });
        while self.file.decisions.len() > MAX_DECISIONS {
            let victim = self
                .file
                .decisions
                .iter()
                .position(|d| !d.unresolved())
                .unwrap_or(0);
            let removed = self.file.decisions.remove(victim);
            tracing::debug!(
                decision = removed.id,
                "decision list is full; dropped the oldest"
            );
        }
        self.changed();
        AddOutcome::Created(id)
    }

    pub(crate) fn get(&self, id: u64) -> Option<&DecisionRecord> {
        self.file.decisions.iter().find(|d| d.id == id)
    }

    /// Newest first; resolved ones only when asked.
    pub(crate) fn list(&self, include_resolved: bool) -> Vec<&DecisionRecord> {
        let mut list: Vec<&DecisionRecord> = self
            .file
            .decisions
            .iter()
            .filter(|d| include_resolved || d.unresolved())
            .collect();
        list.sort_by(|a, b| {
            b.created_unix_ms
                .cmp(&a.created_unix_ms)
                .then(b.id.cmp(&a.id))
        });
        list
    }

    /// Decisions waiting for an answer; snoozed ones do not count.
    pub(crate) fn pending_count(&self) -> u32 {
        self.file
            .decisions
            .iter()
            .filter(|d| d.status == DecisionStatus::Pending)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    fn unresolved_mut(&mut self, id: u64) -> Result<&mut DecisionRecord, DecisionError> {
        let record = self
            .file
            .decisions
            .iter_mut()
            .find(|d| d.id == id)
            .ok_or(DecisionError::NotFound)?;
        if record.unresolved() {
            Ok(record)
        } else {
            Err(DecisionError::Resolved(record.status))
        }
    }

    pub(crate) fn snooze(
        &mut self,
        id: u64,
        for_secs: u64,
        now_ms: u64,
    ) -> Result<DecisionRecord, DecisionError> {
        let for_secs = for_secs.clamp(1, MAX_SNOOZE_SECS);
        let record = self.unresolved_mut(id)?;
        record.status = DecisionStatus::Snoozed;
        record.snoozed_until_unix_ms = Some(now_ms.saturating_add(for_secs * 1000));
        let out = record.clone();
        self.changed();
        Ok(out)
    }

    pub(crate) fn dismiss(
        &mut self,
        id: u64,
        now_ms: u64,
    ) -> Result<DecisionRecord, DecisionError> {
        let record = self.unresolved_mut(id)?;
        record.status = DecisionStatus::Dismissed;
        record.snoozed_until_unix_ms = None;
        record.resolved_unix_ms = Some(now_ms);
        record.outcome = Some("dismissed".into());
        let out = record.clone();
        self.changed();
        Ok(out)
    }

    pub(crate) fn mark_started(
        &mut self,
        id: u64,
        outcome: String,
        now_ms: u64,
    ) -> Result<DecisionRecord, DecisionError> {
        let record = self.unresolved_mut(id)?;
        record.status = DecisionStatus::Started;
        record.snoozed_until_unix_ms = None;
        record.resolved_unix_ms = Some(now_ms);
        record.outcome = Some(outcome);
        record.last_error = None;
        let out = record.clone();
        self.changed();
        Ok(out)
    }

    /// Remember why a Start failed; the decision stays as it was.
    pub(crate) fn record_error(
        &mut self,
        id: u64,
        error: String,
    ) -> Result<DecisionRecord, DecisionError> {
        let record = self.unresolved_mut(id)?;
        record.last_error = Some(error);
        let out = record.clone();
        self.changed();
        Ok(out)
    }

    /// Snoozed decisions whose time has come become pending again. Returns
    /// them so the caller can remind the user.
    pub(crate) fn expire_snoozes(&mut self, now_ms: u64) -> Vec<DecisionRecord> {
        let mut woken = Vec::new();
        for record in &mut self.file.decisions {
            if record.status == DecisionStatus::Snoozed
                && record
                    .snoozed_until_unix_ms
                    .is_none_or(|until| until <= now_ms)
            {
                record.status = DecisionStatus::Pending;
                record.snoozed_until_unix_ms = None;
                woken.push(record.clone());
            }
        }
        if !woken.is_empty() {
            self.changed();
        }
        woken
    }

    /// When the next snooze ends, for the caller's timer.
    pub(crate) fn next_snooze_expiry_ms(&self) -> Option<u64> {
        self.file
            .decisions
            .iter()
            .filter(|d| d.status == DecisionStatus::Snoozed)
            .filter_map(|d| d.snoozed_until_unix_ms)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(job: &str, hash: &str) -> NewDecision {
        NewDecision {
            job_id: job.into(),
            job_title: job.into(),
            run_id: 1,
            ask: format!("ask {hash}"),
            tab_title: "tab".into(),
            cwd: "/tmp".into(),
            agent_command: "claude".into(),
            prompt_file: Some("/tmp/p.md".into()),
            context: Some(serde_json::json!({"tag": "v1"})),
            content_hash: hash.into(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir()
            .join(format!(
                "flockr-decisions-{name}-{}-{nanos}",
                std::process::id()
            ))
            .join("decisions.json")
    }

    #[test]
    fn decisions_are_added_pending_and_listed_newest_first() {
        let mut store = DecisionStore::in_memory();
        assert_eq!(store.add(new("a", "h1"), 100), AddOutcome::Created(1));
        assert_eq!(store.add(new("b", "h2"), 200), AddOutcome::Created(2));
        let ids: Vec<_> = store.list(false).iter().map(|d| d.public_id()).collect();
        assert_eq!(ids, ["d2", "d1"]);
        assert_eq!(store.pending_count(), 2);
        assert_eq!(store.get(1).unwrap().status, DecisionStatus::Pending);
    }

    #[test]
    fn the_same_job_and_content_does_not_create_a_second_decision() {
        let mut store = DecisionStore::in_memory();
        assert_eq!(store.add(new("a", "same"), 100), AddOutcome::Created(1));
        let revision = store.revision();
        assert_eq!(store.add(new("a", "same"), 200), AddOutcome::Duplicate(1));
        assert_eq!(store.revision(), revision, "a duplicate changes nothing");
        // Another job, or other content, is a new question.
        assert_eq!(store.add(new("b", "same"), 300), AddOutcome::Created(2));
        assert_eq!(store.add(new("a", "other"), 400), AddOutcome::Created(3));
        // A resolved decision still blocks a repeat: the user already answered.
        store.dismiss(1, 500).unwrap();
        assert_eq!(store.add(new("a", "same"), 600), AddOutcome::Duplicate(1));
    }

    #[test]
    fn decisions_survive_a_restart_and_ids_keep_counting() {
        let path = temp_path("restart");
        {
            let mut store = DecisionStore::open(path.clone());
            store.add(new("a", "h1"), 100);
            store.add(new("a", "h2"), 200);
            store.snooze(2, 3600, 250).unwrap();
            store.dismiss(1, 300).unwrap();
        }
        let mut store = DecisionStore::open(path.clone());
        assert_eq!(store.get(1).unwrap().status, DecisionStatus::Dismissed);
        let two = store.get(2).unwrap();
        assert_eq!(two.status, DecisionStatus::Snoozed);
        assert_eq!(two.snoozed_until_unix_ms, Some(250 + 3_600_000));
        assert_eq!(two.context, Some(serde_json::json!({"tag": "v1"})));
        assert_eq!(store.add(new("a", "h3"), 400), AddOutcome::Created(3));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_corrupt_file_starts_fresh() {
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        let mut store = DecisionStore::open(path.clone());
        assert!(store.list(true).is_empty());
        assert_eq!(store.add(new("a", "h"), 1), AddOutcome::Created(1));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn snooze_hides_the_decision_until_it_expires() {
        let mut store = DecisionStore::in_memory();
        store.add(new("a", "h1"), 0);
        store.add(new("a", "h2"), 0);
        let snoozed = store.snooze(1, DEFAULT_SNOOZE_SECS, 1_000).unwrap();
        assert_eq!(snoozed.status, DecisionStatus::Snoozed);
        assert_eq!(store.pending_count(), 1, "snoozed ones do not count");
        assert_eq!(store.next_snooze_expiry_ms(), Some(1_000 + 3_600_000));
        // Not yet.
        assert!(store.expire_snoozes(3_000_000).is_empty());
        assert_eq!(store.get(1).unwrap().status, DecisionStatus::Snoozed);
        // At the deadline it is pending again and reported once.
        let woken = store.expire_snoozes(1_000 + 3_600_000);
        assert_eq!(woken.len(), 1);
        assert_eq!(woken[0].id, 1);
        assert_eq!(store.get(1).unwrap().status, DecisionStatus::Pending);
        assert_eq!(store.pending_count(), 2);
        assert!(store.expire_snoozes(u64::MAX).is_empty());
        assert_eq!(store.next_snooze_expiry_ms(), None);
    }

    #[test]
    fn snooze_length_is_clamped() {
        let mut store = DecisionStore::in_memory();
        store.add(new("a", "h"), 0);
        let long = store.snooze(1, u64::MAX / 4, 0).unwrap();
        assert_eq!(long.snoozed_until_unix_ms, Some(MAX_SNOOZE_SECS * 1000));
        let short = store.snooze(1, 0, 0).unwrap();
        assert_eq!(short.snoozed_until_unix_ms, Some(1000));
    }

    #[test]
    fn resolved_decisions_cannot_be_changed_again() {
        let mut store = DecisionStore::in_memory();
        store.add(new("a", "h"), 0);
        let started = store
            .mark_started(1, "started in tab x".into(), 10)
            .unwrap();
        assert_eq!(started.status, DecisionStatus::Started);
        assert_eq!(started.outcome.as_deref(), Some("started in tab x"));
        assert_eq!(
            store.dismiss(1, 20).unwrap_err(),
            DecisionError::Resolved(DecisionStatus::Started)
        );
        assert_eq!(
            store.snooze(1, 5, 20).unwrap_err(),
            DecisionError::Resolved(DecisionStatus::Started)
        );
        assert_eq!(store.dismiss(9, 20).unwrap_err(), DecisionError::NotFound);
        assert!(store.list(false).is_empty());
        assert_eq!(store.list(true).len(), 1);
    }

    #[test]
    fn a_failed_start_keeps_the_decision_pending_with_the_reason() {
        let mut store = DecisionStore::in_memory();
        store.add(new("a", "h"), 0);
        let failed = store.record_error(1, "cwd missing".into()).unwrap();
        assert_eq!(failed.status, DecisionStatus::Pending);
        assert_eq!(failed.last_error.as_deref(), Some("cwd missing"));
        let started = store.mark_started(1, "ok".into(), 5).unwrap();
        assert!(started.last_error.is_none(), "success clears the error");
    }

    #[test]
    fn the_list_is_capped_dropping_resolved_decisions_first() {
        let mut store = DecisionStore::in_memory();
        for n in 0..MAX_DECISIONS {
            store.add(new("a", &format!("h{n}")), n as u64);
        }
        store.dismiss(5, 1_000).unwrap();
        store.add(new("a", "extra"), 2_000);
        assert_eq!(store.list(true).len(), MAX_DECISIONS);
        assert!(store.get(5).is_none(), "the resolved one made room");
        assert!(store.get(1).is_some(), "unanswered decisions stay");
        // With nothing resolved left, the oldest unanswered one goes.
        store.add(new("a", "extra2"), 3_000);
        assert_eq!(store.list(true).len(), MAX_DECISIONS);
        assert!(store.get(1).is_none());
        assert!(store.get(2).is_some());
    }

    #[test]
    fn ids_parse_with_or_without_the_prefix() {
        assert_eq!(parse_decision_id("d12"), Some(12));
        assert_eq!(parse_decision_id(" 7 "), Some(7));
        assert_eq!(parse_decision_id("x"), None);
        assert_eq!(parse_decision_id("d"), None);
    }

    #[test]
    fn info_projects_the_public_fields() {
        let mut store = DecisionStore::in_memory();
        store.add(new("a", "h"), 42);
        let info = store.get(1).unwrap().info();
        assert_eq!(info.decision_id, "d1");
        assert_eq!(info.created_unix_ms, 42);
        assert_eq!(info.status, DecisionStatus::Pending);
        assert_eq!(info.prompt_file.as_deref(), Some("/tmp/p.md"));
        assert!(info.prompt.is_none());
    }
}
