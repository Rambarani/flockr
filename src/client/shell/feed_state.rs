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

use super::*;
use crate::api::schema::{
    FeedGetParams, FeedItemInfo, FeedListParams, FeedRunActionParams, FeedSummary, FeedTarget,
    Method, ResponseResult,
};

/// How often the sidebar section asks the server for changes.
pub(super) const SIDEBAR_POLL: Duration = Duration::from_secs(5);
const OVERLAY_POLL: Duration = Duration::from_secs(2);
/// Cadence just after an action or refresh, until the result is in.
const FAST_POLL: Duration = Duration::from_secs(1);
const FAST_WINDOW: Duration = Duration::from_secs(30);
/// The palette's feed entries refresh when the cached list is older.
const CACHE_MAX_AGE: Duration = Duration::from_secs(10);

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
}

impl FeedClientState {
    pub(super) fn summary(&self, job_id: &str) -> Option<&FeedSummary> {
        self.feeds.iter().find(|feed| feed.job_id == job_id)
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
                if feed.count > 0
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
            if let Some(job_id) = overlay.job_id.as_ref() {
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
    pub(super) fn run_feed_action(
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
    fn show_feed_message(&mut self, origin: FeedOrigin, code: &str, title: &str, text: String) {
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
