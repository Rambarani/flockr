//! Persisted upstream watcher state, stored in flockr's state directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::release::{parse_tag_version, UpstreamRelease};
use crate::update::Version;

const UPSTREAM_STATE_PATH: &str = "upstream.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpstreamState {
    /// Unix seconds of the last successful release check.
    #[serde(default)]
    pub last_check_unix: Option<u64>,
    /// Newest pending tag the user was already notified about.
    #[serde(default)]
    pub last_notified_tag: Option<String>,
    /// Tags the user chose to skip.
    #[serde(default)]
    pub skipped_tags: Vec<String>,
    /// Upstream tag recorded by `flockr upstream apply`. Wins over the compiled
    /// base until a rebuilt binary catches up.
    #[serde(default)]
    pub synced_base: Option<String>,
    /// Pending releases from the last successful check, so the review popup
    /// and `status` work offline.
    #[serde(default)]
    pub pending: Vec<UpstreamRelease>,
    /// Last check failure, cleared by the next success.
    #[serde(default)]
    pub last_error: Option<String>,
}

pub(crate) fn state_path() -> PathBuf {
    crate::config::state_dir().join(UPSTREAM_STATE_PATH)
}

impl UpstreamState {
    pub(crate) fn load() -> Self {
        Self::load_from(&state_path())
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        self.save_to(&state_path())
    }

    /// A missing or unreadable state file is an empty state: the watcher must
    /// never fail because of its own cache.
    pub(crate) fn load_from(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
        fs::write(&tmp_path, json)?;
        if let Err(err) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }

    /// Upstream version flockr is synced to: the newer of the compiled base and
    /// the base recorded by the last `apply`.
    pub(crate) fn effective_base(&self, compiled: &Version) -> Version {
        self.synced_base
            .as_deref()
            .and_then(parse_tag_version)
            .filter(|synced| synced > compiled)
            .unwrap_or_else(|| compiled.clone())
    }

    pub(crate) fn is_skipped(&self, tag: &str) -> bool {
        self.skipped_tags.iter().any(|skipped| skipped == tag)
    }

    /// Marks `tag` skipped and drops it from the cached pending list.
    pub(crate) fn skip(&mut self, tag: &str) {
        if !self.is_skipped(tag) {
            self.skipped_tags.push(tag.to_string());
        }
        self.pending.retain(|release| release.tag != tag);
    }

    /// Records a finished sync. Releases at or below the new base are no longer
    /// pending.
    pub(crate) fn record_synced(&mut self, tag: &str) {
        self.synced_base = Some(tag.to_string());
        if let Some(base) = parse_tag_version(tag) {
            self.pending.retain(|release| {
                parse_tag_version(&release.tag).is_some_and(|version| version > base)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> UpstreamRelease {
        UpstreamRelease {
            tag: tag.to_string(),
            name: String::new(),
            body: String::new(),
            prerelease: false,
            published_at: None,
            url: String::new(),
        }
    }

    fn temp_state_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "flockr-upstream-state-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join("upstream.json")
    }

    #[test]
    fn state_round_trips_through_disk() {
        let path = temp_state_path("roundtrip");
        let mut state = UpstreamState {
            last_check_unix: Some(1_700_000_000),
            last_notified_tag: Some("v0.9.7".into()),
            synced_base: Some("v0.9.6".into()),
            pending: vec![release("v0.9.7")],
            last_error: Some("boom".into()),
            ..Default::default()
        };
        state.skip("v0.9.8");
        state.save_to(&path).unwrap();

        let loaded = UpstreamState::load_from(&path);
        assert_eq!(loaded, state);
        assert!(loaded.is_skipped("v0.9.8"));
        assert!(!loaded.is_skipped("v0.9.7"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_or_corrupt_state_loads_as_default() {
        let path = temp_state_path("corrupt");
        assert_eq!(UpstreamState::load_from(&path), UpstreamState::default());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();
        assert_eq!(UpstreamState::load_from(&path), UpstreamState::default());
        // Unknown and missing fields are tolerated for forward compatibility.
        fs::write(&path, r#"{"skipped_tags":["v1.0.0"],"future":1}"#).unwrap();
        assert!(UpstreamState::load_from(&path).is_skipped("v1.0.0"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skip_is_idempotent_and_drops_cached_release() {
        let mut state = UpstreamState {
            pending: vec![release("v0.9.6"), release("v0.9.7")],
            ..Default::default()
        };
        state.skip("v0.9.6");
        state.skip("v0.9.6");
        assert_eq!(state.skipped_tags, ["v0.9.6"]);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].tag, "v0.9.7");
    }

    #[test]
    fn effective_base_prefers_newer_recorded_sync() {
        let compiled = Version::parse("0.9.5").unwrap();
        let mut state = UpstreamState::default();
        assert_eq!(state.effective_base(&compiled), compiled);
        state.synced_base = Some("v0.9.7".into());
        assert_eq!(
            state.effective_base(&compiled),
            Version::parse("0.9.7").unwrap()
        );
        // A rebuilt binary with a newer compiled base beats a stale record.
        let rebuilt = Version::parse("0.9.8").unwrap();
        assert_eq!(state.effective_base(&rebuilt), rebuilt);
        state.synced_base = Some("garbage".into());
        assert_eq!(state.effective_base(&compiled), compiled);
    }

    #[test]
    fn record_synced_prunes_pending_at_or_below_new_base() {
        let mut state = UpstreamState {
            pending: vec![release("v0.9.6"), release("v0.9.7"), release("v0.9.8")],
            ..Default::default()
        };
        state.record_synced("v0.9.7");
        assert_eq!(state.synced_base.as_deref(), Some("v0.9.7"));
        let tags: Vec<_> = state.pending.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.9.8"]);
    }
}
