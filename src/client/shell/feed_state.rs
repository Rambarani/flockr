//! Client-side memory of the active server's job feeds, shared by the sidebar
//! section, the Feed overlay and the command palette.
//!
//! The data comes from the server's `feed.*` methods on the client command
//! lane, never from the snapshot. A poll asks `feed.list` with the revision
//! the client already holds, so an idle server answers with a few bytes; items
//! are fetched with `feed.get` only for feeds that are on screen and whose
//! revision changed. Polling runs only while something shows feed data: the
//! expanded sidebar (every few seconds), the overlay (faster) or the palette.
//! Servers without the methods turn the feature off without an error.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::jobs_overlay::JobsRequest;
use super::*;
use crate::api::schema::{
    FeedGetParams, FeedItemInfo, FeedListParams, FeedRunActionParams, FeedSummary, FeedTarget,
    JobListParams, JobRunStatus, JobState, JobTarget, Method, ResponseResult,
};
use crate::jobs::display;

/// How often the sidebar section asks the server for changes.
pub(super) const SIDEBAR_POLL: Duration = Duration::from_secs(5);
const OVERLAY_POLL: Duration = Duration::from_secs(2);
/// Cadence just after an action or refresh, until the result is in.
const FAST_POLL: Duration = Duration::from_secs(1);
const FAST_WINDOW: Duration = Duration::from_secs(30);
/// The palette's feed entries refresh when the cached list is older.
const CACHE_MAX_AGE: Duration = Duration::from_secs(10);
/// How often a sync asks the server whether its run has finished.
const SYNC_CHECK_GAP: Duration = Duration::from_secs(1);
/// A finished run's feed refresh gets this long before the sync is called done.
const SYNC_REFRESH_GRACE: Duration = Duration::from_secs(30);
/// A sync that never reports back is forgotten after this long.
const SYNC_GIVE_UP: Duration = Duration::from_secs(10 * 60);

/// Where a refresh or action was asked for, so its answer shows in the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FeedOrigin {
    Overlay,
    Elsewhere,
}

/// What a pending `feed.*` request was for.
#[derive(Debug, Clone)]
pub(super) enum FeedRequest {
    List,
    Get {
        job_id: String,
    },
    Refresh {
        job_id: String,
        origin: FeedOrigin,
    },
    Action {
        job_id: String,
        item_id: String,
        key: String,
        origin: FeedOrigin,
    },
    /// `feed.work`: start an agent tab on selected items.
    Work {
        job_id: String,
    },
}

/// A "sync now" this client asked for: the feed's job runs, then the server
/// refreshes the feed.
#[derive(Debug, Clone)]
pub(super) struct FeedSync {
    job_id: String,
    /// The feed's `fetched_unix_ms` when the sync began; a newer one means
    /// the refresh after the run landed.
    baseline_fetched: Option<u64>,
    /// The server accepted `job.run`.
    accepted: bool,
    run_id: Option<u64>,
    started_at: Instant,
    last_check: Option<Instant>,
    /// When the job's run was seen finished successfully.
    run_finished_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub(super) struct FeedItems {
    pub(super) revision: u64,
    pub(super) items: Vec<FeedItemInfo>,
}

#[derive(Debug, Default)]
pub(super) struct FeedClientState {
    /// The endpoint whose feeds these are; a switch clears everything.
    pub(super) endpoint: Option<ClientEndpointId>,
    pub(super) feeds: Vec<FeedSummary>,
    pub(super) list_revision: Option<u64>,
    pub(super) items: HashMap<String, FeedItems>,
    items_in_flight: HashSet<String>,
    /// The server rejected or does not know the `feed.*` methods.
    pub(super) unsupported: bool,
    /// The expanded sidebar was drawn in the last frame.
    pub(super) sidebar_visible: bool,
    last_poll: Option<Instant>,
    pub(super) fast_until: Option<Instant>,
    /// An action this client asked for and has not seen the outcome of:
    /// the job and the `finished_unix_ms` it must beat.
    awaiting_action: Option<(String, u64)>,
    /// A sync now that has not finished.
    sync: Option<FeedSync>,
}

impl FeedClientState {
    pub(super) fn summary(&self, job_id: &str) -> Option<&FeedSummary> {
        self.feeds.iter().find(|feed| feed.job_id == job_id)
    }

    /// A sync now for this job is running.
    pub(super) fn is_syncing(&self, job_id: &str) -> bool {
        self.sync.as_ref().is_some_and(|sync| sync.job_id == job_id)
    }

    pub(super) fn items_of(&self, job_id: &str) -> &[FeedItemInfo] {
        self.items
            .get(job_id)
            .map_or(&[][..], |items| items.items.as_slice())
    }

    fn reset_for(&mut self, endpoint: ClientEndpointId) {
        *self = Self {
            endpoint: Some(endpoint),
            sidebar_visible: self.sidebar_visible,
            ..Self::default()
        };
    }

    /// Ask again at the next tick, and at a faster pace for a while.
    pub(super) fn poll_soon(&mut self, now: Instant) {
        self.last_poll = None;
        self.fast_until = Some(now + FAST_WINDOW);
    }
}

fn update_from_summary(feeds: &mut [FeedSummary], fresh: &FeedSummary) {
    if let Some(slot) = feeds.iter_mut().find(|feed| feed.job_id == fresh.job_id) {
        *slot = fresh.clone();
    }
}

impl ClientShellState {
    fn feed_list_in_flight(&self) -> bool {
        self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::Feed {
                    request: FeedRequest::List
                }
            )
        })
    }

    fn feed_overlay_open(&self) -> bool {
        matches!(self.overlay, Some(ClientShellOverlay::Feed(_)))
    }

    /// Ask the server for something; a server without the method switches
    /// the feature off quietly instead of raising a notice.
    fn push_feed_request(
        &mut self,
        method: Method,
        request: FeedRequest,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.supports_endpoint_method(&method) {
            self.feeds.unsupported = true;
            outcome.repaint = true;
            return false;
        }
        if !self.endpoint_is_online(&self.active_endpoint_id) {
            return false;
        }
        self.push_endpoint_method_with_kind(method, PendingEndpointKind::Feed { request }, outcome)
    }

    pub(super) fn request_feed_list(&mut self, outcome: &mut ClientShellInput) {
        let revision = self.feeds.list_revision;
        self.push_feed_request(
            Method::FeedList(FeedListParams {
                since_revision: revision,
            }),
            FeedRequest::List,
            outcome,
        );
    }

    /// Jobs whose items the screen needs now: expanded sidebar sections and
    /// the overlay's current feed.
    fn wanted_feed_items(&self) -> Vec<String> {
        let mut wanted = Vec::new();
        if self.feeds.sidebar_visible && self.config.feeds.enabled {
            for feed in super::feed_sidebar::sidebar_feeds(&self.feeds, &self.config.feeds) {
                // A feed with groups shows group rows, which need no items.
                if feed.count > 0
                    && feed.groups.is_empty()
                    && !self.group_is_collapsed(
                        &self.active_endpoint_id,
                        &super::feed_sidebar::feed_key(&feed.job_id),
                    )
                {
                    wanted.push(feed.job_id.clone());
                }
            }
        }
        if let Some(ClientShellOverlay::Feed(overlay)) = &self.overlay {
            // The chosen feed, or the first one until one is chosen.
            let shown = overlay
                .job_id
                .as_ref()
                .or_else(|| self.feeds.feeds.first().map(|feed| &feed.job_id));
            if let Some(job_id) = shown {
                if self
                    .feeds
                    .summary(job_id)
                    .is_some_and(|feed| feed.count > 0)
                    && !wanted.contains(job_id)
                {
                    wanted.push(job_id.clone());
                }
            }
        }
        wanted
    }

    /// Fetch items for wanted feeds whose cached revision is behind.
    pub(super) fn request_missing_feed_items(&mut self, outcome: &mut ClientShellInput) {
        for job_id in self.wanted_feed_items() {
            let Some(summary) = self.feeds.summary(&job_id) else {
                continue;
            };
            let held = self.feeds.items.get(&job_id).map(|items| items.revision);
            if held == Some(summary.revision) || self.feeds.items_in_flight.contains(&job_id) {
                continue;
            }
            self.feeds.items_in_flight.insert(job_id.clone());
            let sent = self.push_feed_request(
                Method::FeedGet(FeedGetParams {
                    job_id: job_id.clone(),
                    since_revision: held,
                    limit: None,
                }),
                FeedRequest::Get {
                    job_id: job_id.clone(),
                },
                outcome,
            );
            if !sent {
                self.feeds.items_in_flight.remove(&job_id);
            }
        }
    }

    /// Poll while something shows feed data. Returns whether to repaint.
    pub(crate) fn tick_feeds(&mut self, now: Instant, outcome: &mut ClientShellInput) -> bool {
        if self.feeds.endpoint.as_ref() != Some(&self.active_endpoint_id) {
            let endpoint = self.active_endpoint_id.clone();
            self.feeds.reset_for(endpoint);
        }
        self.tick_feed_sync(now, outcome);
        let overlay_open = self.feed_overlay_open();
        let palette_open = matches!(self.overlay, Some(ClientShellOverlay::Palette(_)));
        let sidebar = self.feeds.sidebar_visible && self.config.feeds.enabled;
        if !(sidebar || overlay_open || palette_open) || self.feeds.unsupported {
            return false;
        }
        if !self.endpoint_is_online(&self.active_endpoint_id) || self.feed_list_in_flight() {
            return false;
        }
        let fast = self.feeds.fast_until.is_some_and(|until| now < until);
        let gap = if fast {
            FAST_POLL
        } else if overlay_open {
            OVERLAY_POLL
        } else if sidebar {
            SIDEBAR_POLL
        } else {
            CACHE_MAX_AGE
        };
        let due = self
            .feeds
            .last_poll
            .is_none_or(|at| now.saturating_duration_since(at) >= gap);
        if !due {
            return false;
        }
        self.feeds.last_poll = Some(now);
        let was_unsupported = self.feeds.unsupported;
        self.request_feed_list(outcome);
        self.feeds.unsupported != was_unsupported
    }

    /// Ask whether the sync's run has finished, about once a second.
    fn tick_feed_sync(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        let Some(sync) = self.feeds.sync.clone() else {
            return;
        };
        if now.saturating_duration_since(sync.started_at) > SYNC_GIVE_UP {
            self.feeds.sync = None;
            return;
        }
        let in_flight = self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::Jobs {
                    request: JobsRequest::FeedSyncCheck { .. }
                }
            )
        });
        let due = sync
            .last_check
            .is_none_or(|at| now.saturating_duration_since(at) >= SYNC_CHECK_GAP);
        if !sync.accepted || in_flight || !due || !self.endpoint_is_online(&self.active_endpoint_id)
        {
            return;
        }
        if let Some(sync) = self.feeds.sync.as_mut() {
            sync.last_check = Some(now);
        }
        self.push_jobs_request(
            Method::JobList(JobListParams {
                since_revision: None,
            }),
            JobsRequest::FeedSyncCheck {
                job_id: sync.job_id,
            },
            outcome,
        );
    }

    /// Run the feed's job now. The server refreshes the feed itself after a
    /// successful run; the client watches `job.list` until the run is done
    /// and the new items are in.
    pub(super) fn sync_feed_now(
        &mut self,
        job_id: String,
        origin: FeedOrigin,
        outcome: &mut ClientShellInput,
    ) {
        if self.feeds.is_syncing(&job_id) {
            self.show_feed_message(
                origin,
                "feed.sync",
                "Feed already syncing",
                format!("{job_id}: already syncing"),
            );
            return;
        }
        let baseline_fetched = self
            .feeds
            .summary(&job_id)
            .and_then(|feed| feed.fetched_unix_ms);
        self.feeds.sync = Some(FeedSync {
            job_id: job_id.clone(),
            baseline_fetched,
            accepted: false,
            run_id: None,
            started_at: Instant::now(),
            last_check: None,
            run_finished_at: None,
        });
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(format!("syncing {job_id}…"));
        }
        let sent = self.push_jobs_request(
            Method::JobRun(JobTarget {
                job_id: job_id.clone(),
            }),
            JobsRequest::FeedSync { job_id, origin },
            outcome,
        );
        if !sent {
            self.feeds.sync = None;
            if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
                overlay.status = Some("this server cannot run jobs".to_owned());
            }
        }
        outcome.repaint = true;
    }

    /// The answer to `job.run` for a sync.
    pub(super) fn complete_feed_sync_start(
        &mut self,
        job_id: String,
        origin: FeedOrigin,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        match result {
            Ok(ResponseResult::JobRunRequested {
                started: true,
                run_id,
                ..
            }) => {
                if let Some(sync) = self
                    .feeds
                    .sync
                    .as_mut()
                    .filter(|sync| sync.job_id == job_id)
                {
                    sync.accepted = true;
                    sync.run_id = run_id;
                }
                self.feeds.poll_soon(Instant::now());
            }
            Ok(ResponseResult::JobRunRequested { reason, .. }) => {
                self.feeds.sync = None;
                let text = format!(
                    "{job_id}: {}",
                    reason.unwrap_or_else(|| "not started".into())
                );
                self.show_feed_message(origin, "feed.sync", "Feed not synced", text);
            }
            Ok(_) => self.feeds.sync = None,
            Err(error) => {
                self.feeds.sync = None;
                self.show_feed_message(origin, "feed.sync", "Feed not synced", error.message);
            }
        }
        true
    }

    /// The answer to a sync's `job.list` check: is the run done, and did the
    /// feed refresh after it?
    pub(super) fn complete_feed_sync_check(
        &mut self,
        job_id: &str,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(sync) = self.feeds.sync.clone().filter(|sync| sync.job_id == job_id) else {
            return false;
        };
        let Ok(ResponseResult::JobList { jobs, .. }) = result else {
            return false;
        };
        let Some(job) = jobs.iter().find(|job| job.job_id == job_id) else {
            self.finish_feed_sync(&sync, false, format!("{job_id}: the job is gone"));
            return true;
        };
        let run = job.last_run.as_ref().filter(|run| match sync.run_id {
            Some(id) => run.run_id >= id && job.state != JobState::Running,
            None => {
                job.state != JobState::Running && sync.started_at.elapsed() > Duration::from_secs(2)
            }
        });
        let Some(run) = run else {
            return false;
        };
        if run.status != JobRunStatus::Success {
            let text = format!(
                "sync failed: {}: {}",
                display::status_word(run.status),
                run.summary
            );
            self.finish_feed_sync(&sync, false, text);
            return true;
        }
        let summary = self.feeds.summary(job_id).cloned();
        let refreshed = summary.as_ref().is_some_and(|feed| {
            !feed.refreshing
                && feed.fetched_unix_ms.is_some()
                && feed.fetched_unix_ms > sync.baseline_fetched
        });
        if !job.has_feed {
            self.finish_feed_sync(&sync, true, format!("ran {job_id}: ok"));
            return true;
        }
        if refreshed {
            let count = summary.map_or(0, |feed| feed.count);
            self.finish_feed_sync(&sync, true, format!("synced {job_id}: {count} items"));
            return true;
        }
        let finished_at = sync.run_finished_at.unwrap_or_else(Instant::now);
        if let Some(sync) = self.feeds.sync.as_mut() {
            sync.run_finished_at = Some(finished_at);
        }
        if finished_at.elapsed() > SYNC_REFRESH_GRACE {
            self.finish_feed_sync(
                &sync,
                false,
                format!("{job_id}: ran, but the feed did not refresh"),
            );
            return true;
        }
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(format!("syncing {job_id}: refreshing the feed…"));
        }
        true
    }

    /// End a sync and say how it went: in the overlay's status line, or as
    /// a notice for a failure when no overlay is open.
    fn finish_feed_sync(&mut self, sync: &FeedSync, ok: bool, text: String) {
        self.feeds.sync = None;
        self.feeds.poll_soon(Instant::now());
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(text);
        } else if !ok {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                format!("feed.sync:{}", sync.job_id),
                "Feed sync failed",
                text,
            );
        }
    }

    /// Refresh a feed on the server.
    pub(super) fn refresh_feed(
        &mut self,
        job_id: String,
        origin: FeedOrigin,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(format!("refreshing {job_id}…"));
        }
        self.push_feed_request(
            Method::FeedRefresh(FeedTarget {
                job_id: job_id.clone(),
            }),
            FeedRequest::Refresh { job_id, origin },
            outcome,
        );
    }

    /// Run an item's action on the server, then the server refreshes the feed.
    /// An action with a `confirm` question asks y/n first.
    pub(super) fn run_feed_action(
        &mut self,
        job_id: String,
        item_id: String,
        key: String,
        origin: FeedOrigin,
        outcome: &mut ClientShellInput,
    ) {
        let question = self
            .feed_item(&job_id, &item_id)
            .and_then(|item| item.actions.into_iter().find(|action| action.key == key))
            .and_then(|action| action.confirm);
        if let Some(question) = question {
            let return_to = match self.overlay.take() {
                Some(ClientShellOverlay::Feed(overlay)) => Some(overlay),
                other => {
                    self.overlay = other;
                    None
                }
            };
            self.overlay = Some(ClientShellOverlay::ConfirmFeedAction(Box::new(
                ClientConfirmFeedActionOverlay {
                    job_id,
                    item_id,
                    key,
                    origin,
                    question,
                    return_to,
                },
            )));
            outcome.repaint = true;
            return;
        }
        self.send_feed_action(job_id, item_id, key, origin, outcome);
    }

    /// The answer to a confirm question: run the action on y, drop it on n.
    pub(super) fn answer_feed_action_confirmation(
        &mut self,
        accepted: bool,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::ConfirmFeedAction(confirm)) = self.overlay.take() else {
            return;
        };
        let confirm = *confirm;
        self.overlay = confirm.return_to.map(ClientShellOverlay::Feed);
        if accepted {
            self.send_feed_action(
                confirm.job_id,
                confirm.item_id,
                confirm.key,
                confirm.origin,
                outcome,
            );
        }
        outcome.repaint = true;
    }

    fn send_feed_action(
        &mut self,
        job_id: String,
        item_id: String,
        key: String,
        origin: FeedOrigin,
        outcome: &mut ClientShellInput,
    ) {
        self.push_feed_request(
            Method::FeedRunAction(FeedRunActionParams {
                job_id: job_id.clone(),
                item_id: item_id.clone(),
                key: key.clone(),
            }),
            FeedRequest::Action {
                job_id,
                item_id,
                key,
                origin,
            },
            outcome,
        );
    }

    /// Ask the server to start an agent tab on feed items (`w`). The caller
    /// has checked that the server advertises `feed.work`.
    pub(super) fn request_feed_work(
        &mut self,
        method: Method,
        job_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.push_feed_request(method, FeedRequest::Work { job_id }, outcome);
    }

    /// The palette's "feed: <title>" and "refresh feed <job>" entries.
    pub(super) fn feed_palette_entries(&self) -> Vec<(String, String)> {
        if self.feeds.endpoint.as_ref() != Some(&self.active_endpoint_id) {
            return Vec::new();
        }
        self.feeds
            .feeds
            .iter()
            .map(|feed| (feed.job_id.clone(), feed.title.clone()))
            .collect()
    }

    /// Handle the response to a `feed.*` request. These are quiet: failures
    /// show in the overlay (or one notice for an action) instead of generic
    /// endpoint notices.
    pub(super) fn complete_feed_request(
        &mut self,
        request: FeedRequest,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let mut outcome = ClientShellInput::default();
        let repaint = match (request, result) {
            (
                FeedRequest::List,
                Ok(ResponseResult::FeedList {
                    revision,
                    unchanged,
                    feeds,
                }),
            ) => {
                self.feeds.unsupported = false;
                self.feeds.list_revision = Some(revision);
                let mut repaint = false;
                if !unchanged {
                    self.feeds.feeds = feeds;
                    let known: HashSet<&str> = self
                        .feeds
                        .feeds
                        .iter()
                        .map(|feed| feed.job_id.as_str())
                        .collect();
                    self.feeds
                        .items
                        .retain(|job_id, _| known.contains(job_id.as_str()));
                    repaint = true;
                }
                self.announce_feed_action_outcome(&mut repaint);
                self.request_missing_feed_items(&mut outcome);
                repaint
            }
            (
                FeedRequest::Get { job_id },
                Ok(ResponseResult::FeedDetail {
                    feed,
                    unchanged,
                    items,
                }),
            ) => {
                self.feeds.items_in_flight.remove(&job_id);
                update_from_summary(&mut self.feeds.feeds, &feed);
                if !unchanged {
                    self.feeds.items.insert(
                        job_id,
                        FeedItems {
                            revision: feed.revision,
                            items,
                        },
                    );
                }
                true
            }
            (
                FeedRequest::Refresh { job_id, origin },
                Ok(ResponseResult::FeedRefreshRequested {
                    started, reason, ..
                }),
            ) => {
                self.feeds.poll_soon(Instant::now());
                if started {
                    if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
                        overlay.status = Some(format!("refreshing {job_id}…"));
                    }
                } else {
                    let text = format!(
                        "{job_id}: {}",
                        reason.unwrap_or_else(|| "not refreshed".into())
                    );
                    self.show_feed_message(origin, "feed.refresh", "Feed not refreshed", text);
                }
                true
            }
            (
                FeedRequest::Action {
                    job_id,
                    item_id,
                    key,
                    origin,
                },
                Ok(ResponseResult::FeedActionRequested {
                    started, reason, ..
                }),
            ) => {
                self.feeds.poll_soon(Instant::now());
                if started {
                    let before = self
                        .feeds
                        .summary(&job_id)
                        .and_then(|feed| feed.last_action.as_ref())
                        .map_or(0, |action| action.finished_unix_ms);
                    self.feeds.awaiting_action = Some((job_id.clone(), before));
                    if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
                        overlay.status = Some(format!("running {key} on {item_id}…"));
                    }
                } else {
                    let text = format!("{job_id}: {}", reason.unwrap_or_else(|| "not run".into()));
                    self.show_feed_message(origin, "feed.run_action", "Feed action not run", text);
                }
                true
            }
            (
                FeedRequest::Work { job_id },
                Ok(ResponseResult::FeedWorkStarted {
                    count,
                    agent,
                    tab_title,
                    tab_id,
                    warnings,
                    skipped,
                    draft,
                    ..
                }),
            ) => {
                let label = if tab_title.is_empty() {
                    tab_id
                } else {
                    tab_title
                };
                let plural = if count == 1 { "" } else { "s" };
                let mut text = if draft {
                    format!(
                        "opened {agent} with {count} item{plural} as a draft — add your note and press Enter"
                    )
                } else {
                    format!("started {agent} with {count} item{plural} in tab {label}")
                };
                if !skipped.is_empty() {
                    text.push_str(&format!(" ({} no longer in the feed)", skipped.len()));
                }
                if let Some(warning) = warnings.first() {
                    text.push_str(&format!("; {warning}"));
                }
                // The new tab is focused on the server; get out of its way,
                // as a decision's Start does, and say what happened.
                if matches!(self.overlay, Some(ClientShellOverlay::Feed(_))) {
                    self.overlay = None;
                }
                self.push_endpoint_notice(
                    ClientEndpointNoticeKind::Rejected,
                    format!("feed.work:{job_id}"),
                    "Feed work started",
                    text,
                );
                true
            }
            (FeedRequest::List, Err(error)) => {
                // A server that does not answer is retried at the next poll;
                // one that rejects the call is treated as lacking the feature.
                if matches!(
                    error.code.as_deref(),
                    Some("unsupported_method" | "not_implemented")
                ) {
                    self.feeds.unsupported = true;
                }
                false
            }
            (FeedRequest::Get { job_id }, Err(_)) => {
                self.feeds.items_in_flight.remove(&job_id);
                false
            }
            (FeedRequest::Work { .. }, Err(error)) => {
                // Stay in the overlay with the selection, and say why.
                let text = if matches!(
                    error.code.as_deref(),
                    Some("unsupported_method" | "not_implemented")
                ) {
                    "this server does not support starting work from a feed yet; update and restart it"
                        .to_owned()
                } else {
                    error.message
                };
                self.show_feed_message(FeedOrigin::Overlay, "feed.work", "Feed work failed", text);
                true
            }
            (
                FeedRequest::Refresh { origin, .. } | FeedRequest::Action { origin, .. },
                Err(error),
            ) => {
                self.show_feed_message(
                    origin,
                    "feed.request",
                    "Feed request failed",
                    error.message,
                );
                true
            }
            (_, Ok(_)) => false,
        };
        (repaint, outcome.actions)
    }

    /// Show a message where the user asked: in the overlay's status line, or
    /// as one notice when no overlay is open.
    pub(super) fn show_feed_message(
        &mut self,
        origin: FeedOrigin,
        code: &str,
        title: &str,
        text: String,
    ) {
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(text);
        } else if origin == FeedOrigin::Elsewhere || self.overlay.is_none() {
            self.push_endpoint_notice(ClientEndpointNoticeKind::Rejected, code, title, text);
        }
    }

    /// Report the outcome of an action this client started, once.
    fn announce_feed_action_outcome(&mut self, repaint: &mut bool) {
        let Some((job_id, before)) = self.feeds.awaiting_action.clone() else {
            return;
        };
        let Some(action) = self
            .feeds
            .summary(&job_id)
            .and_then(|feed| feed.last_action.clone())
            .filter(|action| action.finished_unix_ms > before)
        else {
            return;
        };
        self.feeds.awaiting_action = None;
        *repaint = true;
        if let Some(ClientShellOverlay::Feed(overlay)) = self.overlay.as_mut() {
            overlay.status = Some(if action.ok {
                format!("{} on {}: ok", action.key, action.item_id)
            } else {
                format!(
                    "{} on {} failed: {}",
                    action.key, action.item_id, action.summary
                )
            });
        } else if !action.ok {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                format!("feed.action:{job_id}"),
                "Feed action failed",
                format!("{}: {}", action.item_id, action.summary),
            );
        }
    }
}
