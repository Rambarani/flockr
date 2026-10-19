//! Upstream herdr watcher and the "copy updates" workflow.
//!
//! flockr is a fork of herdr (see `NOTICE`). This module keeps track of which
//! upstream release flockr is synced to, checks the upstream GitHub releases in
//! the background, and prepares an isolated worktree where an agent adopts a
//! new upstream tag's changes without touching the checked-out `flockr` branch.
//!
//! - `release`: release records, version selection and release-note bucketing.
//! - `state`: persisted watcher state (last check, notified/skipped tags).
//! - `check`: the failure-soft, interval-gated release check.
//! - `sync`: git preflight, worktree creation, prompt file and agent launch.
//! - `patch`: replays upstream's `<base>..<tag>` changes onto the worktree as a
//!   per-file 3-way merge, because flockr shares no git history with herdr.
//! - `apply`: user-triggered merge of a finished sync branch into `flockr`.
//! - `review`: the interactive popup behind `flockr upstream review`.

pub(crate) mod apply;
pub(crate) mod check;
pub(crate) mod patch;
pub(crate) mod release;
pub(crate) mod review;
pub(crate) mod state;
pub(crate) mod sync;

use crate::update::Version;

/// Upstream tag flockr was synced to when this binary was built. The file is
/// committed (`.flockr/upstream-base`) and updated by every upstream sync.
const COMPILED_BASE_TAG: &str = include_str!("../../.flockr/upstream-base");

/// Local testing hook (like `FLOCKR_FAKE_UPDATE_VERSION`): pretend flockr was
/// built at this upstream tag, so a check against real GitHub data finds
/// "new" releases.
const FAKE_BASE_ENV: &str = "FLOCKR_FAKE_UPSTREAM_BASE";

/// Name of the upstream sync branch for a tag.
pub(crate) fn sync_branch_name(tag: &str) -> String {
    format!("upstream-sync/{tag}")
}

/// Upstream version the running binary was built from.
pub(crate) fn compiled_base_tag() -> &'static str {
    COMPILED_BASE_TAG.trim()
}

pub(crate) fn compiled_base() -> Version {
    if let Some(fake) = std::env::var(FAKE_BASE_ENV)
        .ok()
        .and_then(|tag| release::parse_tag_version(&tag))
    {
        tracing::info!(env = FAKE_BASE_ENV, base = %fake, "using fake upstream base for local testing");
        return fake;
    }
    release::parse_tag_version(compiled_base_tag()).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_base_file_holds_a_release_tag() {
        let tag = compiled_base_tag();
        assert!(tag.starts_with('v'), "{tag}");
        assert!(release::parse_tag_version(tag).is_some(), "{tag}");
        assert!(compiled_base() >= Version::parse("0.9.5").unwrap());
    }

    #[test]
    fn sync_branch_is_namespaced_by_tag() {
        assert_eq!(sync_branch_name("v0.9.7"), "upstream-sync/v0.9.7");
    }
}
