//! Interval-gated, failure-soft check of the upstream herdr releases.
//!
//! Network access goes through `curl` like the other background checks
//! (`crate::update`), and is injected as a closure so tests never touch it.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::release::{notification_title, parse_releases_json, pending_releases, UpstreamRelease};
use super::state::UpstreamState;
use crate::update::Version;

#[derive(Debug, Clone)]
pub(crate) struct CheckParams {
    pub include_prereleases: bool,
    pub interval: Duration,
    /// Ignore the interval and hit the network (manual `flockr upstream check`).
    pub force: bool,
    /// Record the newest pending tag as notified. Background checks set this so
    /// each release notifies once; the manual CLI check does not.
    pub mark_notified: bool,
    pub now_unix: u64,
    pub compiled_base: Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamNotification {
    pub tag: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckOutcome {
    pub base: Version,
    pub pending: Vec<UpstreamRelease>,
    /// The network was queried and answered.
    pub fetched: bool,
    pub error: Option<String>,
    pub notification: Option<UpstreamNotification>,
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Runs one check against the state file at `state_path`.
///
/// A failed fetch keeps the cached pending list, records the error and leaves
/// `last_check_unix` alone so the next poll retries.
pub(crate) fn run_check(
    state_path: &Path,
    params: &CheckParams,
    fetch: &dyn Fn() -> Result<Vec<UpstreamRelease>, String>,
) -> CheckOutcome {
    let mut state = UpstreamState::load_from(state_path);
    let base = state.effective_base(&params.compiled_base);

    let due = params.force
        || state
            .last_check_unix
            .is_none_or(|last| params.now_unix.saturating_sub(last) >= params.interval.as_secs());

    let mut fetched = false;
    let mut error = None;
    let pending = if due {
        match fetch() {
            Ok(releases) => {
                fetched = true;
                state.last_check_unix = Some(params.now_unix);
                state.last_error = None;
                let pending = pending_releases(
                    &releases,
                    &base,
                    params.include_prereleases,
                    &state.skipped_tags,
                );
                state.pending = pending.clone();
                pending
            }
            Err(err) => {
                state.last_error = Some(err.clone());
                error = Some(err);
                cached_pending(&state, &base, params)
            }
        }
    } else {
        cached_pending(&state, &base, params)
    };

    let notification = pending.last().and_then(|latest| {
        if state.last_notified_tag.as_deref() == Some(latest.tag.as_str()) {
            return None;
        }
        Some(UpstreamNotification {
            tag: latest.tag.clone(),
            title: notification_title(&pending)?,
        })
    });
    if params.mark_notified {
        if let Some(notification) = &notification {
            state.last_notified_tag = Some(notification.tag.clone());
        }
    }

    if let Err(err) = state.save_to(state_path) {
        tracing::warn!(err = %err, "failed to save upstream watcher state");
    }

    CheckOutcome {
        base,
        pending,
        fetched,
        error,
        notification: if params.mark_notified {
            notification
        } else {
            None
        },
    }
}

fn cached_pending(
    state: &UpstreamState,
    base: &Version,
    params: &CheckParams,
) -> Vec<UpstreamRelease> {
    pending_releases(
        &state.pending,
        base,
        params.include_prereleases,
        &state.skipped_tags,
    )
}

/// Fetches the upstream releases listing from GitHub with `curl`.
pub(crate) fn fetch_releases_curl() -> Result<Vec<UpstreamRelease>, String> {
    let output = crate::noninteractive_process::curl_command()
        .args([
            "-sfL",
            "--retry",
            "2",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: flockr-upstream-watch",
            crate::product::UPSTREAM_RELEASES_API_URL,
        ])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !output.status.success() {
        return Err("failed to fetch upstream releases".into());
    }
    parse_releases_json(&output.stdout)
}

pub(crate) fn check_params(
    config: &crate::config::UpstreamConfig,
    force: bool,
    mark_notified: bool,
) -> CheckParams {
    CheckParams {
        include_prereleases: config.include_prereleases,
        interval: config.check_interval(),
        force,
        mark_notified,
        now_unix: unix_now(),
        compiled_base: super::compiled_base(),
    }
}

/// Background entry point: runs a due check and, when a release is new to the
/// user, tells the app so it can show a toast. Safe to call often; it only
/// touches the network once per `check_interval_hours`.
pub(crate) fn background_check(
    config: &crate::config::UpstreamConfig,
    events: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    if !config.enabled {
        return;
    }
    let outcome = run_check(
        &super::state::state_path(),
        &check_params(config, false, true),
        &fetch_releases_curl,
    );
    if let Some(error) = &outcome.error {
        tracing::warn!(err = %error, "upstream release check failed");
    }
    if let Some(notification) = outcome.notification {
        tracing::info!(tag = %notification.tag, "new upstream herdr release");
        let review_hint = if config.review_key.trim().is_empty() {
            "run `flockr upstream review`".to_string()
        } else {
            format!(
                "press {} to review and copy updates",
                config.review_key.trim()
            )
        };
        let _ = events.blocking_send(crate::events::AppEvent::UpstreamUpdatesAvailable {
            title: notification.title,
            context: review_hint,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::PathBuf;

    use super::*;

    fn release(tag: &str, body: &str) -> UpstreamRelease {
        UpstreamRelease {
            tag: tag.to_string(),
            name: tag.to_string(),
            body: body.to_string(),
            prerelease: false,
            published_at: None,
            url: String::new(),
        }
    }

    fn temp_state_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "flockr-upstream-check-{name}-{}-{}",
                std::process::id(),
                unix_now()
            ))
            .join("upstream.json")
    }

    fn params(now_unix: u64) -> CheckParams {
        CheckParams {
            include_prereleases: false,
            interval: Duration::from_secs(24 * 3600),
            force: false,
            mark_notified: true,
            now_unix,
            compiled_base: Version::parse("0.9.5").unwrap(),
        }
    }

    #[test]
    fn first_check_fetches_notifies_once_and_caches() {
        let path = temp_state_path("first");
        let calls = Cell::new(0);
        let fetch = || {
            calls.set(calls.get() + 1);
            Ok(vec![
                release("v0.9.5", "### Fixed\n- old"),
                release("v0.9.6", "### Added\n- a\n### Fixed\n- b\n- c"),
            ])
        };

        let outcome = run_check(&path, &params(1_000), &fetch);
        assert!(outcome.fetched);
        assert_eq!(calls.get(), 1);
        assert_eq!(outcome.pending.len(), 1);
        assert_eq!(
            outcome.notification.as_ref().unwrap().title,
            "herdr v0.9.6 available upstream — 1 feature, 2 fixes"
        );

        // Within the interval: no network, cached pending, no second toast.
        let again = run_check(&path, &params(1_000 + 3600), &fetch);
        assert!(!again.fetched);
        assert_eq!(calls.get(), 1);
        assert_eq!(again.pending.len(), 1);
        assert!(again.notification.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn check_refetches_after_interval_and_notifies_for_newer_tag_only() {
        let path = temp_state_path("interval");
        let first = || Ok(vec![release("v0.9.6", "### Fixed\n- a")]);
        run_check(&path, &params(0), &first);

        let calls = Cell::new(0);
        let second = || {
            calls.set(calls.get() + 1);
            Ok(vec![
                release("v0.9.6", "### Fixed\n- a"),
                release("v0.9.7", "### Added\n- b"),
            ])
        };
        let outcome = run_check(&path, &params(24 * 3600), &second);
        assert_eq!(calls.get(), 1);
        assert_eq!(outcome.pending.len(), 2);
        assert_eq!(outcome.notification.unwrap().tag, "v0.9.7");

        let third = || Ok(vec![release("v0.9.6", ""), release("v0.9.7", "")]);
        let outcome = run_check(&path, &params(2 * 24 * 3600), &third);
        assert!(outcome.notification.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn failed_fetch_is_soft_keeps_cache_and_retries_next_poll() {
        let path = temp_state_path("fail");
        let ok = || Ok(vec![release("v0.9.6", "### Fixed\n- a")]);
        run_check(&path, &params(0), &ok);

        let failing = || Err("failed to fetch upstream releases".to_string());
        let outcome = run_check(&path, &params(24 * 3600), &failing);
        assert!(!outcome.fetched);
        assert_eq!(
            outcome.error.as_deref(),
            Some("failed to fetch upstream releases")
        );
        assert_eq!(outcome.pending.len(), 1, "cached pending survives");
        assert!(outcome.notification.is_none());

        let state = UpstreamState::load_from(&path);
        assert_eq!(
            state.last_check_unix,
            Some(0),
            "failure must not reset interval"
        );
        assert!(state.last_error.is_some());

        // The next poll retries immediately and clears the error.
        let calls = Cell::new(0);
        let retry = || {
            calls.set(calls.get() + 1);
            Ok(vec![release("v0.9.6", "")])
        };
        run_check(&path, &params(24 * 3600 + 1800), &retry);
        assert_eq!(calls.get(), 1);
        assert!(UpstreamState::load_from(&path).last_error.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn skipped_tags_are_never_pending_or_notified() {
        let path = temp_state_path("skip");
        let mut state = UpstreamState::default();
        state.skip("v0.9.6");
        state.save_to(&path).unwrap();

        let fetch = || Ok(vec![release("v0.9.6", "### Added\n- a")]);
        let outcome = run_check(&path, &params(0), &fetch);
        assert!(outcome.pending.is_empty());
        assert!(outcome.notification.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn manual_force_check_ignores_interval_and_does_not_mark_notified() {
        let path = temp_state_path("force");
        let fetch = || Ok(vec![release("v0.9.6", "### Added\n- a")]);
        run_check(&path, &params(0), &fetch);

        let calls = Cell::new(0);
        let counting = || {
            calls.set(calls.get() + 1);
            Ok(vec![release("v0.9.6", "### Added\n- a")])
        };
        let mut manual = params(60);
        manual.force = true;
        manual.mark_notified = false;
        let outcome = run_check(&path, &manual, &counting);
        assert_eq!(calls.get(), 1);
        assert!(outcome.fetched);
        assert!(outcome.notification.is_none());
        assert_eq!(
            UpstreamState::load_from(&path).last_notified_tag.as_deref(),
            Some("v0.9.6"),
            "the earlier background check's marker is untouched"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn synced_base_hides_already_applied_releases() {
        let path = temp_state_path("synced");
        let mut state = UpstreamState::default();
        state.record_synced("v0.9.6");
        state.save_to(&path).unwrap();

        let fetch = || {
            Ok(vec![
                release("v0.9.6", "### Added\n- a"),
                release("v0.9.7", "### Added\n- b"),
            ])
        };
        let outcome = run_check(&path, &params(0), &fetch);
        assert_eq!(outcome.base, Version::parse("0.9.6").unwrap());
        let tags: Vec<_> = outcome.pending.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v0.9.7"]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn up_to_date_checks_notify_nothing() {
        let path = temp_state_path("uptodate");
        let fetch = || Ok(vec![release("v0.9.5", "### Fixed\n- a")]);
        let outcome = run_check(&path, &params(0), &fetch);
        assert!(outcome.fetched);
        assert!(outcome.pending.is_empty());
        assert!(outcome.notification.is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
