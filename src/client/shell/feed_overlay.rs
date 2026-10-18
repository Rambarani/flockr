//! Feed overlay: a master-detail view of a job feed, with a picker when there
//! are several feeds. The list is on the left (stacked on top when the panel
//! is narrow) and the detail pane follows the selection: title, state, link,
//! the item's fields and body. Enter opens the selected item's link, the
//! item's action keys run its actions, `/` filters, `R` refreshes the feed,
//! `S` runs the feed's job now, Ctrl-D and Ctrl-U (or the wheel over the
//! pane) scroll the detail, Space zooms the detail, Tab or the arrow keys
//! switch feeds, and Esc closes.
//!
//! A feed that declares groups gets a category tab bar under its info line:
//! `All` and one tab per group, with counts. `[` and `]` step through the
//! tabs, `0` selects All and `1`-`9` the first nine groups, and a click
//! selects one. The filter, the list and the detail pane all follow the
//! selected tab. The choice is remembered per feed in the client preferences
//! under a `feedgroup:` key.
//!
//! The data is the client's cached copy of the server's `feed.*` answers (see
//! `feed_state`); while the overlay is open it is polled every two seconds
//! with the revisions the client already holds.

use crossterm::event::MouseEvent;
use ratatui::style::{Modifier, Style};

use super::feed_state::FeedOrigin;
use super::popup_frame::{
    base_style, dim_style, draw_panel, heading_style, key_style, selected_style,
};
use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::{FeedGroupInfo, FeedItemInfo, FeedItemState, FeedSummary};
use crate::jobs::display;

const MIN_PANEL_WIDTH: u16 = 30;
const MIN_PANEL_HEIGHT: u16 = 9;
/// Rows above the list: tabs or title, info, filter.
const HEADER_ROWS: u16 = 3;
/// Rows below the body: status, hint.
const FOOTER_ROWS: u16 = 2;
const MAX_BADGE_WIDTH: usize = 14;
const AGE_WIDTH: usize = 8;
/// Key prefix in the shared collapsed-group sets for the remembered category.
pub(super) const GROUP_PREF_PREFIX: &str = "feedgroup:";
/// Most digit keys that select a category (`1`-`9`).
const MAX_DIGIT_GROUPS: usize = 9;

#[derive(Debug)]
pub(super) struct ClientFeedOverlay {
    /// The feed shown; the first feed until one is chosen.
    pub(super) job_id: Option<String>,
    /// The selected item, by id so it survives refreshes.
    pub(super) selected: Option<String>,
    pub(super) scroll: usize,
    pub(super) filter: String,
    pub(super) filtering: bool,
    /// The selected category (a group id); `None` is the All tab.
    pub(super) group: Option<String>,
    pub(super) status: Option<String>,
    /// First visible line of the detail pane; it follows the selection, so a
    /// new selection starts at the top.
    pub(super) detail_scroll: usize,
    /// The detail pane takes the whole body (toggled with space).
    pub(super) zoom: bool,
}

#[derive(Default)]
pub(super) struct FeedOverlayHits {
    pub(super) popup: Rect,
    pub(super) tabs: Vec<(Rect, String)>,
    /// The category tabs; `None` is the All tab.
    pub(super) groups: Vec<(Rect, Option<String>)>,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
    /// The detail pane (empty when it is not drawn) and how far it can scroll.
    pub(super) detail: Rect,
    pub(super) detail_max_scroll: usize,
    pub(super) detail_viewport: usize,
}

pub(super) struct FeedRender {
    pub(super) area: Rect,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) groups: Vec<(Rect, Option<String>)>,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
    pub(super) detail: Rect,
    pub(super) detail_max_scroll: usize,
    pub(super) detail_viewport: usize,
}

fn matches_filter(item: &FeedItemInfo, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let needle = filter.to_lowercase();
    [
        Some(item.title.as_str()),
        item.subtitle.as_deref(),
        item.badge.as_deref(),
        Some(item.id.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&needle))
}

fn age_text(item: &FeedItemInfo, now_ms: u64) -> Option<String> {
    let secs = item
        .updated_at
        .as_deref()
        .and_then(crate::feed::format::parse_rfc3339_secs)?;
    let at_ms = u64::try_from(secs).ok()?.saturating_mul(1000);
    Some(if at_ms > now_ms {
        "now".to_owned()
    } else {
        display::ago(now_ms, at_ms)
    })
}

fn dot(state: FeedItemState, palette: &Palette) -> (&'static str, ratatui::style::Color) {
    match state {
        FeedItemState::Unread => ("●", palette.accent),
        FeedItemState::Read => ("○", palette.overlay0),
        FeedItemState::Done => ("✓", palette.green),
        FeedItemState::Warn => ("▲", palette.peach),
        FeedItemState::Error => ("✖", palette.red),
    }
}

impl ClientShellState {
    pub(super) fn open_feed_overlay(&mut self, job_id: Option<String>) {
        let group = job_id
            .as_deref()
            .or_else(|| self.feeds.feeds.first().map(|feed| feed.job_id.as_str()))
            .and_then(|job_id| self.stored_feed_group(job_id));
        self.overlay = Some(ClientShellOverlay::Feed(ClientFeedOverlay {
            job_id,
            selected: None,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            group,
            status: None,
            detail_scroll: 0,
            zoom: false,
        }));
        // Fetch right away, then at the overlay's own pace.
        self.feeds.poll_soon(std::time::Instant::now());
    }

    fn feed_overlay(&self) -> Option<&ClientFeedOverlay> {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Feed(overlay)) => Some(overlay),
            _ => None,
        }
    }

    fn feed_overlay_mut(&mut self) -> Option<&mut ClientFeedOverlay> {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Feed(overlay)) => Some(overlay),
            _ => None,
        }
    }

    /// The feed the overlay shows: the chosen one if it still exists, else
    /// the first.
    fn current_feed(&self) -> Option<&FeedSummary> {
        let overlay = self.feed_overlay()?;
        overlay
            .job_id
            .as_deref()
            .and_then(|job_id| self.feeds.summary(job_id))
            .or_else(|| self.feeds.feeds.first())
    }

    /// The selected category if the current feed still has it; `None` is All.
    fn selected_group(&self) -> Option<&str> {
        let group = self.feed_overlay()?.group.as_deref()?;
        self.current_feed()?
            .groups
            .iter()
            .any(|candidate| candidate.id == group)
            .then_some(group)
    }

    pub(super) fn visible_feed_items(&self) -> Vec<&FeedItemInfo> {
        let (Some(overlay), Some(feed)) = (self.feed_overlay(), self.current_feed()) else {
            return Vec::new();
        };
        let group = self.selected_group();
        self.feeds
            .items_of(&feed.job_id)
            .iter()
            .filter(|item| group.is_none_or(|group| item.group.as_deref() == Some(group)))
            .filter(|item| matches_filter(item, &overlay.filter))
            .collect()
    }

    /// The category remembered for a feed on the active endpoint.
    pub(super) fn stored_feed_group(&self, job_id: &str) -> Option<String> {
        let prefix = format!("{GROUP_PREF_PREFIX}{job_id}:");
        self.collapsed_groups_for_endpoint(&self.active_endpoint_id)?
            .iter()
            .find_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
    }

    /// Remember (or, with `None`, forget) a feed's category and persist it.
    pub(super) fn store_feed_group(
        &mut self,
        job_id: &str,
        group: Option<&str>,
        outcome: &mut ClientShellInput,
    ) {
        let prefix = format!("{GROUP_PREF_PREFIX}{job_id}:");
        let endpoint_id = self.active_endpoint_id.clone();
        let keys = if endpoint_id.is_local() {
            &mut self.collapsed_groups
        } else {
            self.remote_collapsed_groups.entry(endpoint_id).or_default()
        };
        keys.retain(|key| !key.starts_with(&prefix));
        if let Some(group) = group {
            keys.insert(format!("{prefix}{group}"));
        }
        self.persist_chrome_preferences(outcome);
    }

    /// Show a category (`None` is All): the list starts at its first item and
    /// the filter stays, so it applies within the category.
    pub(super) fn select_feed_group(
        &mut self,
        group: Option<String>,
        outcome: &mut ClientShellInput,
    ) {
        let Some(job_id) = self.current_feed().map(|feed| feed.job_id.clone()) else {
            return;
        };
        if let Some(overlay) = self.feed_overlay_mut() {
            overlay.group = group.clone();
            overlay.selected = None;
            overlay.scroll = 0;
            overlay.detail_scroll = 0;
            overlay.status = None;
        }
        self.store_feed_group(&job_id, group.as_deref(), outcome);
    }

    /// `[` and `]`: the next or previous tab, wrapping around All.
    fn step_feed_group(&mut self, delta: isize, outcome: &mut ClientShellInput) {
        let Some(feed) = self.current_feed() else {
            return;
        };
        let ids: Vec<String> = feed.groups.iter().map(|group| group.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let current = self
            .selected_group()
            .and_then(|group| ids.iter().position(|id| id == group))
            .map_or(0, |index| index + 1);
        let next = (current as isize + delta).rem_euclid(ids.len() as isize + 1) as usize;
        self.select_feed_group(next.checked_sub(1).map(|index| ids[index].clone()), outcome);
    }

    /// `0` selects All and `1`-`9` the first nine categories. False when the
    /// digit names no tab.
    fn pick_feed_group(&mut self, digit: usize, outcome: &mut ClientShellInput) -> bool {
        let Some(feed) = self.current_feed() else {
            return false;
        };
        if digit == 0 {
            self.select_feed_group(None, outcome);
            return true;
        }
        let Some(group) = feed.groups.get(digit - 1).map(|group| group.id.clone()) else {
            return false;
        };
        self.select_feed_group(Some(group), outcome);
        true
    }

    fn current_feed_has_groups(&self) -> bool {
        self.current_feed()
            .is_some_and(|feed| !feed.groups.is_empty())
    }

    fn selected_feed_index(&self, items: &[&FeedItemInfo]) -> usize {
        self.feed_overlay()
            .and_then(|overlay| overlay.selected.as_ref())
            .and_then(|id| items.iter().position(|item| item.id == *id))
            .unwrap_or(0)
    }

    pub(super) fn selected_feed_item(&self) -> Option<FeedItemInfo> {
        let items = self.visible_feed_items();
        items
            .get(self.selected_feed_index(&items))
            .map(|item| (*item).clone())
    }

    /// Select an item by id; a different item shows from the top of its detail.
    fn set_feed_selection(&mut self, id: String) {
        if let Some(overlay) = self.feed_overlay_mut() {
            if overlay.selected.as_ref() != Some(&id) {
                overlay.detail_scroll = 0;
            }
            overlay.selected = Some(id);
        }
    }

    /// Scroll the detail pane by `delta` lines, within what the last frame drew.
    fn scroll_feed_detail(&mut self, delta: isize) {
        let max = self.hits.feed_overlay.detail_max_scroll;
        if let Some(overlay) = self.feed_overlay_mut() {
            let current = overlay.detail_scroll.min(max) as isize;
            overlay.detail_scroll = (current + delta).clamp(0, max as isize) as usize;
        }
    }

    fn move_feed_selection(&mut self, delta: isize) {
        let viewport = self.hits.feed_overlay.viewport_rows.max(1);
        let offset = self.hits.feed_overlay.offset;
        let items = self.visible_feed_items();
        if items.is_empty() {
            return;
        }
        let current = self.selected_feed_index(&items);
        let next = (current as isize + delta).clamp(0, items.len() as isize - 1) as usize;
        let id = items[next].id.clone();
        let scroll = if next < offset {
            next
        } else if next >= offset + viewport {
            next + 1 - viewport
        } else {
            offset
        };
        self.set_feed_selection(id);
        if let Some(overlay) = self.feed_overlay_mut() {
            overlay.scroll = scroll;
        }
    }

    fn switch_feed(&mut self, delta: isize, outcome: &mut ClientShellInput) {
        let count = self.feeds.feeds.len();
        if count < 2 {
            return;
        }
        let current = self
            .current_feed()
            .and_then(|feed| {
                self.feeds
                    .feeds
                    .iter()
                    .position(|candidate| candidate.job_id == feed.job_id)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(count as isize) as usize;
        let job_id = self.feeds.feeds[next].job_id.clone();
        self.select_feed(job_id, outcome);
    }

    fn select_feed(&mut self, job_id: String, outcome: &mut ClientShellInput) {
        let group = self.stored_feed_group(&job_id);
        if let Some(overlay) = self.feed_overlay_mut() {
            overlay.group = group;
            overlay.job_id = Some(job_id);
            overlay.selected = None;
            overlay.scroll = 0;
            overlay.detail_scroll = 0;
            overlay.filter.clear();
            overlay.filtering = false;
            overlay.status = None;
        }
        self.request_missing_feed_items(outcome);
    }

    fn open_selected_feed_item(&mut self, outcome: &mut ClientShellInput) {
        let Some(item) = self.selected_feed_item() else {
            return;
        };
        match item.url {
            Some(url) => outcome.actions.push(ClientShellAction::OpenSafeWebUrl(url)),
            None => {
                if let Some(overlay) = self.feed_overlay_mut() {
                    overlay.status = Some(format!("{} has no link", item.id));
                }
            }
        }
    }

    fn run_selected_feed_action(&mut self, key: &str, outcome: &mut ClientShellInput) -> bool {
        let (Some(item), Some(job_id)) = (
            self.selected_feed_item(),
            self.current_feed().map(|feed| feed.job_id.clone()),
        ) else {
            return false;
        };
        if !item.actions.iter().any(|action| action.key == key) {
            return false;
        }
        self.run_feed_action(
            job_id,
            item.id,
            key.to_owned(),
            FeedOrigin::Overlay,
            outcome,
        );
        true
    }

    /// `S`: run the feed's job now; the server refreshes the feed when the
    /// run succeeds.
    fn sync_current_feed(&mut self, outcome: &mut ClientShellInput) {
        if let Some(job_id) = self.current_feed().map(|feed| feed.job_id.clone()) {
            self.sync_feed_now(job_id, FeedOrigin::Overlay, outcome);
        }
    }

    fn refresh_current_feed(&mut self, outcome: &mut ClientShellInput) {
        if let Some(job_id) = self.current_feed().map(|feed| feed.job_id.clone()) {
            self.refresh_feed(job_id, FeedOrigin::Overlay, outcome);
        }
    }

    fn reset_feed_list_position(&mut self) {
        let first = self
            .visible_feed_items()
            .first()
            .map(|item| item.id.clone());
        if let Some(overlay) = self.feed_overlay_mut() {
            if overlay.selected != first {
                overlay.detail_scroll = 0;
            }
            overlay.selected = first;
            overlay.scroll = 0;
        }
    }

    /// Handle a key while the overlay is open. Returns false when it is not.
    pub(super) fn route_feed_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::KeyModifiers;

        let Some(overlay) = self.feed_overlay() else {
            return false;
        };
        let filtering = overlay.filtering;
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let plain = modifiers.is_empty();
        let ctrl = modifiers == KeyModifiers::CONTROL;
        let page = self.hits.feed_overlay.viewport_rows.max(1) as isize;
        let half_detail = (self.hits.feed_overlay.detail_viewport / 2).max(1) as isize;
        outcome.repaint = true;
        let typed = {
            let shifted_without_evidence = key.modifiers.contains(KeyModifiers::SHIFT)
                && key.generated_text.is_none()
                && key.shifted_codepoint.is_none();
            (!shifted_without_evidence)
                .then(|| crate::input::keybind_help_text_char(key))
                .flatten()
        };
        // Movement works the same while filtering.
        let moved = match code {
            KeyCode::Up if plain => Some(-1),
            KeyCode::Down if plain => Some(1),
            KeyCode::Char('p') if ctrl => Some(-1),
            KeyCode::Char('n') if ctrl => Some(1),
            KeyCode::PageUp if plain => Some(-page),
            KeyCode::PageDown if plain => Some(page),
            KeyCode::Home if plain => Some(isize::MIN / 2),
            KeyCode::End if plain => Some(isize::MAX / 2),
            _ => None,
        };
        if let Some(delta) = moved {
            self.move_feed_selection(delta);
            return true;
        }
        // The detail pane scrolls on its own, also while filtering.
        if ctrl {
            match code {
                KeyCode::Char('d') => {
                    self.scroll_feed_detail(half_detail);
                    return true;
                }
                KeyCode::Char('u') => {
                    self.scroll_feed_detail(-half_detail);
                    return true;
                }
                _ => {}
            }
        }
        if filtering {
            match code {
                KeyCode::Esc => {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filter.clear();
                        overlay.filtering = false;
                    }
                    self.reset_feed_list_position();
                }
                KeyCode::Enter if plain => {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filtering = false;
                    }
                }
                KeyCode::Backspace if plain => {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filter.pop();
                    }
                    self.reset_feed_list_position();
                }
                _ => match typed {
                    Some(ch) if !ctrl => {
                        if let Some(overlay) = self.feed_overlay_mut() {
                            overlay.filter.push(ch);
                        }
                        self.reset_feed_list_position();
                    }
                    _ => outcome.repaint = false,
                },
            }
            return true;
        }
        match code {
            KeyCode::Esc => {
                let zoomed = self.feed_overlay().is_some_and(|overlay| overlay.zoom);
                let has_filter = self
                    .feed_overlay()
                    .is_some_and(|overlay| !overlay.filter.is_empty());
                if zoomed {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.zoom = false;
                    }
                } else if has_filter {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filter.clear();
                    }
                    self.reset_feed_list_position();
                } else {
                    self.overlay = None;
                }
            }
            KeyCode::Enter if plain => self.open_selected_feed_item(outcome),
            KeyCode::Char(' ') if plain || modifiers == KeyModifiers::SHIFT => {
                if let Some(overlay) = self.feed_overlay_mut() {
                    overlay.zoom = !overlay.zoom;
                }
            }
            KeyCode::Tab if plain => self.switch_feed(1, outcome),
            KeyCode::BackTab => self.switch_feed(-1, outcome),
            KeyCode::Tab if modifiers == KeyModifiers::SHIFT => self.switch_feed(-1, outcome),
            KeyCode::Right if plain => self.switch_feed(1, outcome),
            KeyCode::Left if plain => self.switch_feed(-1, outcome),
            _ => match typed {
                Some('q') => self.overlay = None,
                Some('[') if self.current_feed_has_groups() => self.step_feed_group(-1, outcome),
                Some(']') if self.current_feed_has_groups() => self.step_feed_group(1, outcome),
                Some(digit @ '0'..='9') if self.current_feed_has_groups() && !ctrl => {
                    let index = digit as usize - '0' as usize;
                    if index > MAX_DIGIT_GROUPS || !self.pick_feed_group(index, outcome) {
                        outcome.repaint = false;
                    }
                }
                Some('/') => {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filtering = true;
                    }
                }
                Some('R') => self.refresh_current_feed(outcome),
                Some('S') => self.sync_current_feed(outcome),
                Some('j') => self.move_feed_selection(1),
                Some('k') => self.move_feed_selection(-1),
                Some('g') => self.move_feed_selection(isize::MIN / 2),
                Some('G') => self.move_feed_selection(isize::MAX / 2),
                Some(ch) if !ctrl => {
                    if !self.run_selected_feed_action(&ch.to_string(), outcome) {
                        outcome.repaint = false;
                    }
                }
                _ => outcome.repaint = false,
            },
        }
        true
    }

    /// Handle a mouse event while the overlay is open. Returns false when it
    /// is not, so the event continues to the regular routing.
    pub(super) fn route_feed_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        if self.feed_overlay().is_none() {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !contains(self.hits.feed_overlay.popup, point) {
                    self.overlay = None;
                } else if let Some(job_id) = self
                    .hits
                    .feed_overlay
                    .tabs
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, job_id)| job_id.clone())
                {
                    self.select_feed(job_id, outcome);
                } else if let Some(group) = self
                    .hits
                    .feed_overlay
                    .groups
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, group)| group.clone())
                {
                    self.select_feed_group(group, outcome);
                } else if let Some(index) = self
                    .hits
                    .feed_overlay
                    .rows
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, index)| *index)
                {
                    let items = self.visible_feed_items();
                    let clicked = items.get(index).map(|item| item.id.clone());
                    let already = clicked.is_some()
                        && clicked
                            == items
                                .get(self.selected_feed_index(&items))
                                .map(|item| item.id.clone());
                    if let Some(id) = clicked {
                        self.set_feed_selection(id);
                        // A second click on the selected row opens it.
                        if already {
                            self.open_selected_feed_item(outcome);
                        }
                    }
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollUp => {
                if contains(self.hits.feed_overlay.detail, point) {
                    self.scroll_feed_detail(-3);
                } else {
                    self.move_feed_selection(-3);
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                if contains(self.hits.feed_overlay.detail, point) {
                    self.scroll_feed_detail(3);
                } else {
                    self.move_feed_selection(3);
                }
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }

    pub(super) fn render_feed_overlay(&self, buffer: &mut Buffer) -> Option<FeedRender> {
        let overlay = self.feed_overlay()?;
        render_feed(
            buffer,
            self,
            overlay,
            crate::jobs::now_unix_ms(),
            &self.config,
        )
    }
}

/// Narrowest panel (inside the border) that puts the list and the detail
/// side by side; narrower panels stack them.
const WIDE_MIN_WIDTH: u16 = 100;
/// Shortest body that can stack a list over a detail pane.
const STACK_MIN_HEIGHT: u16 = 10;
/// Share of the width the list gets when side by side.
const LIST_PERCENT: u16 = 45;
/// The list is this share of the height when stacked.
const STACK_LIST_PERCENT: u16 = 40;
/// Narrowest list that still has room for the age column.
const AGE_MIN_LIST_WIDTH: u16 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divider {
    /// A vertical rule at this column.
    Vertical(u16),
    /// A horizontal rule at this row.
    Horizontal(u16),
}

/// Where the list and the detail pane go inside the body area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BodyLayout {
    list: Option<Rect>,
    detail: Option<Rect>,
    divider: Option<Divider>,
}

/// Side by side when the panel is wide, stacked when it is narrow, the
/// detail alone when zoomed, and the list alone when there is no room for
/// both.
fn split_body(body: Rect, zoom: bool, panel_width: u16) -> BodyLayout {
    if zoom {
        return BodyLayout {
            list: None,
            detail: Some(body),
            divider: None,
        };
    }
    if panel_width >= WIDE_MIN_WIDTH {
        let list_width = body.width * LIST_PERCENT / 100;
        let detail_x = body.x + list_width + 2;
        return BodyLayout {
            list: Some(Rect::new(body.x, body.y, list_width, body.height)),
            detail: Some(Rect::new(
                detail_x,
                body.y,
                body.right().saturating_sub(detail_x),
                body.height,
            )),
            divider: Some(Divider::Vertical(body.x + list_width)),
        };
    }
    if body.height >= STACK_MIN_HEIGHT {
        let list_height = (body.height * STACK_LIST_PERCENT / 100).max(3);
        let detail_y = body.y + list_height + 1;
        return BodyLayout {
            list: Some(Rect::new(body.x, body.y, body.width, list_height)),
            detail: Some(Rect::new(
                body.x,
                detail_y,
                body.width,
                body.bottom().saturating_sub(detail_y),
            )),
            divider: Some(Divider::Horizontal(body.y + list_height)),
        };
    }
    BodyLayout {
        list: Some(body),
        detail: None,
        divider: None,
    }
}

/// One category tab: its target (`None` is All), label parts and counts.
struct CategoryTab<'a> {
    target: Option<&'a str>,
    name: &'a str,
    count: u32,
    unread: u32,
}

/// How much of each tab label is shown, most to least.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelDetail {
    /// `New House PRs 9 ●3`
    CountsAndUnread,
    /// `New House PRs 9`
    Counts,
    /// `New House… 9`: names cut to a few characters.
    ShortNames,
}

const SHORT_NAME_CHARS: usize = 8;

fn category_tabs<'a>(feed: &'a FeedSummary) -> Vec<CategoryTab<'a>> {
    let mut tabs = vec![CategoryTab {
        target: None,
        name: "All",
        count: feed.count,
        unread: feed.unread,
    }];
    tabs.extend(feed.groups.iter().map(|group: &FeedGroupInfo| CategoryTab {
        target: Some(group.id.as_str()),
        name: group.short_title.as_deref().unwrap_or(&group.title),
        count: group.count,
        unread: group.unread,
    }));
    tabs
}

fn category_label(tab: &CategoryTab<'_>, detail: LabelDetail) -> String {
    let name = match detail {
        LabelDetail::ShortNames => crate::ui::truncate_end(tab.name, SHORT_NAME_CHARS),
        _ => tab.name.to_owned(),
    };
    match detail {
        LabelDetail::CountsAndUnread if tab.unread > 0 => {
            format!(" {name} {} ●{} ", tab.count, tab.unread)
        }
        _ => format!(" {name} {} ", tab.count),
    }
}

/// Where each category tab goes on one row of `width` columns, as
/// `(tab index, offset from the row's left edge, label)`. The fullest label
/// detail that fits every tab wins; when even the shortest does not fit, the
/// row scrolls so the selected tab is visible, with `‹` and `›` marking the
/// hidden sides (the text is clipped, never wrapped).
fn layout_category_tabs(
    tabs: &[CategoryTab<'_>],
    selected: usize,
    width: u16,
) -> Vec<(usize, u16, String)> {
    let width = usize::from(width);
    let widths = |labels: &[String]| -> usize {
        labels
            .iter()
            .map(|label| usize::from(display_width(label)))
            .sum::<usize>()
            + labels.len().saturating_sub(1)
    };
    for detail in [
        LabelDetail::CountsAndUnread,
        LabelDetail::Counts,
        LabelDetail::ShortNames,
    ] {
        let labels: Vec<String> = tabs.iter().map(|tab| category_label(tab, detail)).collect();
        if widths(&labels) <= width || detail == LabelDetail::ShortNames {
            if widths(&labels) <= width {
                let mut offset = 0u16;
                return labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| {
                        let at = offset;
                        offset += display_width(&label) + 1;
                        (index, at, label)
                    })
                    .collect();
            }
            // Scroll: drop tabs from the left until the selected one fits
            // beside the marker.
            let selected = selected.min(labels.len().saturating_sub(1));
            let mut start = 0;
            while start < selected && widths(&labels[start..=selected]) + 2 > width {
                start += 1;
            }
            let mut out = Vec::new();
            let mut offset = if start > 0 { 2u16 } else { 0 };
            for (index, label) in labels.iter().enumerate().skip(start) {
                let room = width.saturating_sub(usize::from(offset));
                // Keep a column for the right marker while more tabs follow.
                let room = if index + 1 < labels.len() {
                    room.saturating_sub(1)
                } else {
                    room
                };
                if room < 4 {
                    break;
                }
                let label = crate::ui::truncate_end(label, room);
                let shown = display_width(&label);
                out.push((index, offset, label));
                offset += shown + 1;
                if usize::from(offset) >= width {
                    break;
                }
            }
            return out;
        }
    }
    Vec::new()
}

fn render_feed(
    buffer: &mut Buffer,
    state: &ClientShellState,
    overlay: &ClientFeedOverlay,
    now_ms: u64,
    config: &ClientShellConfig,
) -> Option<FeedRender> {
    let palette = &config.palette;
    let area = buffer.area;
    if area.width < MIN_PANEL_WIDTH || area.height < MIN_PANEL_HEIGHT {
        return None;
    }
    let inner = draw_panel(buffer, area, "feed", palette)?;
    let empty = FeedRender {
        area,
        tabs: Vec::new(),
        groups: Vec::new(),
        rows: Vec::new(),
        viewport_rows: 1,
        offset: 0,
        detail: Rect::default(),
        detail_max_scroll: 0,
        detail_viewport: 1,
    };
    if inner.height < HEADER_ROWS + FOOTER_ROWS + 1 || inner.width < 20 {
        return Some(empty);
    }
    // A feed with groups gets one more header row for the category tabs, when
    // the panel is tall enough to keep a few list rows.
    let category_bar = state
        .current_feed()
        .is_some_and(|feed| !feed.groups.is_empty())
        && inner.height >= HEADER_ROWS + 1 + FOOTER_ROWS + 3;
    let header_rows = HEADER_ROWS + u16::from(category_bar);
    let text_width = inner.width.saturating_sub(2);
    let x = inner.x + 1;
    let hint_y = inner.bottom() - 1;
    let status_y = inner.bottom() - 2;
    let fit = |text: &str, width: u16| crate::ui::truncate_end(text, usize::from(width));
    let feeds = &state.feeds.feeds;
    let current = state.current_feed();
    let mut tabs = Vec::new();
    let mut group_tabs = Vec::new();

    // Line 1: the feed picker, or the feed's title when there is only one.
    if feeds.len() > 1 {
        let mut tab_x = x;
        for feed in feeds {
            let count = if feed.unread > 0 {
                feed.unread.to_string()
            } else {
                String::new()
            };
            let label = if count.is_empty() {
                format!(" {} ", feed.title)
            } else {
                format!(" {} {count} ", feed.title)
            };
            let room = inner.right().saturating_sub(1).saturating_sub(tab_x);
            if room < 4 {
                break;
            }
            let label = fit(&label, room);
            let width = display_width(&label);
            let chosen = current.is_some_and(|current| current.job_id == feed.job_id);
            put_text(
                buffer,
                tab_x,
                inner.y,
                width,
                &label,
                if chosen {
                    selected_style(palette)
                } else {
                    dim_style(palette)
                },
            );
            tabs.push((Rect::new(tab_x, inner.y, width, 1), feed.job_id.clone()));
            tab_x += width + 1;
        }
    } else if let Some(feed) = current {
        put_text(
            buffer,
            x,
            inner.y,
            text_width,
            &fit(&feed.title, text_width),
            heading_style(palette),
        );
    }

    // Line 2: counts and freshness.
    if let Some(feed) = current {
        let mut parts = vec![
            format!("{} items", feed.count),
            format!("{} unread", feed.unread),
        ];
        let mut style = dim_style(palette);
        if state.feeds.is_syncing(&feed.job_id) {
            parts.push("syncing…".to_owned());
        } else if feed.refreshing {
            parts.push("refreshing…".to_owned());
        } else if let Some(error) = &feed.error {
            let kind = if feed.stale { "stale" } else { "failed" };
            parts.push(format!("{kind}: {error}"));
            style = Style::default().fg(palette.peach).bg(palette.panel_bg);
        } else if let Some(at) = feed.fetched_unix_ms {
            parts.push(format!("updated {}", display::ago(now_ms, at)));
        }
        put_text(
            buffer,
            x,
            inner.y + 1,
            text_width,
            &fit(&parts.join(" · "), text_width),
            style,
        );
    }

    // Line 3, with groups: the category tabs.
    if let (true, Some(feed)) = (category_bar, current) {
        let categories = category_tabs(feed);
        let selected = state
            .selected_group()
            .and_then(|group| categories.iter().position(|tab| tab.target == Some(group)))
            .unwrap_or(0);
        for (index, offset, label) in layout_category_tabs(&categories, selected, text_width) {
            let width = display_width(&label);
            let rect = Rect::new(x + offset, inner.y + 2, width, 1);
            put_text(
                buffer,
                rect.x,
                rect.y,
                width,
                &label,
                if index == selected {
                    selected_style(palette)
                } else {
                    dim_style(palette)
                },
            );
            group_tabs.push((rect, categories[index].target.map(str::to_owned)));
        }
        let first_shown = group_tabs.first().map(|(rect, _)| rect.x);
        if first_shown.is_some_and(|first| first > x) {
            put_text(buffer, x, inner.y + 2, 1, "‹", dim_style(palette));
        }
        let last_shown = group_tabs.last().and_then(|(_, target)| {
            categories
                .iter()
                .position(|tab| tab.target == target.as_deref())
        });
        if last_shown.is_some_and(|last| last + 1 < categories.len()) {
            put_text(
                buffer,
                x + text_width.saturating_sub(1),
                inner.y + 2,
                1,
                "›",
                dim_style(palette),
            );
        }
    }

    // The filter row, last of the header.
    if overlay.filtering || !overlay.filter.is_empty() {
        let cursor = if overlay.filtering { "▏" } else { "" };
        put_text(
            buffer,
            x,
            inner.y + header_rows - 1,
            text_width,
            &fit(&format!("/ {}{cursor}", overlay.filter), text_width),
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }

    let list_top = inner.y + header_rows;
    let body = Rect::new(x, list_top, text_width, status_y.saturating_sub(list_top));
    let layout = split_body(body, overlay.zoom, inner.width);
    let items = state.visible_feed_items();
    let selected_index = state.selected_feed_index(&items);
    let empty_message = if state.feeds.unsupported {
        "This server does not support feeds yet. Update and restart it."
    } else if feeds.is_empty() {
        "No feeds. Add a feed table to a job's job.toml (see the Jobs docs)."
    } else if !overlay.filter.is_empty() {
        "No items match the filter."
    } else if state.selected_group().is_some() && current.is_some_and(|feed| feed.loaded) {
        "Nothing in this category."
    } else if current.is_some_and(|feed| feed.error.is_some() && !feed.loaded) {
        "The feed could not be read yet; see the line above."
    } else if current.is_some_and(|feed| feed.count > 0) {
        "loading…"
    } else {
        "Nothing here."
    };

    // The dividing rule.
    match layout.divider {
        Some(Divider::Vertical(rule_x)) => {
            for y in body.y..body.bottom() {
                put_text(buffer, rule_x, y, 1, "│", dim_style(palette));
            }
        }
        Some(Divider::Horizontal(rule_y)) => {
            put_text(
                buffer,
                body.x,
                rule_y,
                body.width,
                &"─".repeat(usize::from(body.width)),
                dim_style(palette),
            );
        }
        None => {}
    }

    // The list. Beside a detail pane it is compact: dot, badge, title, age.
    let mut rows = Vec::new();
    let mut viewport_rows = state.hits.feed_overlay.viewport_rows.max(1);
    let mut offset = overlay.scroll;
    if let Some(list) = layout.list {
        viewport_rows = usize::from(list.height).max(1);
        let max_scroll = items.len().saturating_sub(viewport_rows);
        offset = overlay.scroll.min(max_scroll);
        let compact = layout.detail.is_some();
        let badge_width = items
            .iter()
            .filter_map(|item| item.badge.as_deref())
            .map(|badge| usize::from(display_width(badge)))
            .max()
            .unwrap_or(0)
            .min(MAX_BADGE_WIDTH);
        let show_age = items.iter().any(|item| item.updated_at.is_some())
            && list.width >= if compact { AGE_MIN_LIST_WIDTH } else { 50 };
        for (row, (index, item)) in items
            .iter()
            .enumerate()
            .skip(offset)
            .take(viewport_rows)
            .enumerate()
        {
            let y = list.y + row as u16;
            let rect = Rect::new(list.x, y, list.width, 1);
            let is_selected = index == selected_index;
            let plain = if is_selected {
                selected_style(palette)
            } else {
                base_style(palette)
            };
            let dim = if is_selected {
                selected_style(palette)
            } else {
                dim_style(palette)
            };
            buffer.set_style(rect, plain);
            let (marker, color) = dot(item.state, palette);
            let mut cursor_x = rect.x;
            let right_limit = rect.right();
            let age = show_age.then(|| age_text(item, now_ms)).flatten();
            let age_room = if show_age { AGE_WIDTH + 1 } else { 0 };
            let mut put = |text: &str, style: Style, cursor_x: &mut u16| {
                let room = right_limit
                    .saturating_sub(*cursor_x)
                    .saturating_sub(age_room as u16);
                put_text(buffer, *cursor_x, y, room, text, style);
                *cursor_x = cursor_x.saturating_add(display_width(text).min(room));
            };
            put(
                marker,
                if is_selected {
                    selected_style(palette)
                } else {
                    Style::default().fg(color).bg(palette.panel_bg)
                },
                &mut cursor_x,
            );
            put(" ", plain, &mut cursor_x);
            if badge_width > 0 {
                let badge = item.badge.as_deref().unwrap_or("");
                let badge = crate::ui::truncate_end(badge, badge_width);
                let pad = badge_width.saturating_sub(usize::from(display_width(&badge)));
                put(&format!("{badge}{}", " ".repeat(pad)), dim, &mut cursor_x);
                put(" ", plain, &mut cursor_x);
            }
            let title_style = if item.state == FeedItemState::Unread && !is_selected {
                base_style(palette).add_modifier(Modifier::BOLD)
            } else {
                plain
            };
            let room = usize::from(
                right_limit
                    .saturating_sub(cursor_x)
                    .saturating_sub(age_room as u16),
            );
            let title = crate::ui::truncate_end(&item.title, room);
            let title_width = usize::from(display_width(&title));
            put(&title, title_style, &mut cursor_x);
            if let (false, Some(subtitle)) = (compact, &item.subtitle) {
                let left = room.saturating_sub(title_width);
                if left > 6 {
                    put(
                        &format!("  {}", crate::ui::truncate_end(subtitle, left - 2)),
                        dim,
                        &mut cursor_x,
                    );
                }
            }
            if let Some(age) = age {
                let age = crate::ui::truncate_end(&age, AGE_WIDTH);
                let age_x = rect.right().saturating_sub(display_width(&age));
                put_text(buffer, age_x, y, display_width(&age), &age, dim);
            }
            rows.push((rect, index));
        }
        if items.is_empty() {
            put_text(
                buffer,
                list.x,
                list.y,
                list.width,
                &fit(empty_message, list.width),
                dim_style(palette),
            );
        }
    }
    if layout.list.is_some() && items.len() > viewport_rows {
        let position = format!(
            "{}-{} / {}",
            offset + 1,
            (offset + viewport_rows).min(items.len()),
            items.len()
        );
        let width = display_width(&position);
        put_text(
            buffer,
            inner.right().saturating_sub(width + 1),
            hint_y,
            width,
            &position,
            dim_style(palette),
        );
    }

    // The detail pane: the selected item scrolls above a pinned row of key hints.
    let mut detail_rect = Rect::default();
    let mut detail_max_scroll = 0;
    let mut detail_viewport = 1;
    if let Some(detail) = layout
        .detail
        .filter(|rect| rect.width > 0 && rect.height > 0)
    {
        detail_rect = detail;
        let pinned_y = detail.bottom() - 1;
        let content = Rect::new(
            detail.x,
            detail.y,
            detail.width,
            detail.height.saturating_sub(1),
        );
        detail_viewport = usize::from(content.height).max(1);
        if let Some(item) = items.get(selected_index) {
            let lines =
                super::feed_detail::build_detail(item, usize::from(content.width), now_ms, palette);
            detail_max_scroll = lines.len().saturating_sub(detail_viewport);
            let scroll = overlay.detail_scroll.min(detail_max_scroll);
            super::feed_detail::draw_lines(buffer, content, &lines, scroll);
            let mut cursor_x = detail.x;
            let mut put = |text: &str, style: Style| {
                let room = detail.right().saturating_sub(cursor_x);
                put_text(buffer, cursor_x, pinned_y, room, text, style);
                cursor_x = cursor_x.saturating_add(display_width(text).min(room));
            };
            if item.url.is_some() {
                put("enter", key_style(palette));
                put(" open   ", dim_style(palette));
            }
            for action in &item.actions {
                put(&action.key, key_style(palette));
                put(&format!(" {}   ", action.title), dim_style(palette));
            }
            if detail_max_scroll > 0 {
                let more = format!("{}/{}", scroll + 1, detail_max_scroll + 1);
                let width = display_width(&more);
                let more_x = detail.right().saturating_sub(width);
                if more_x > cursor_x {
                    put_text(buffer, more_x, pinned_y, width, &more, dim_style(palette));
                }
            }
        } else if layout.list.is_none() {
            put_text(
                buffer,
                detail.x,
                detail.y,
                detail.width,
                &fit(empty_message, detail.width),
                dim_style(palette),
            );
        }
    }

    if let Some(status) = &overlay.status {
        put_text(
            buffer,
            x,
            status_y,
            text_width,
            &fit(status, text_width),
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }
    let hint: &[&str] = if overlay.filtering {
        &[
            "type to filter   enter keep   esc clear",
            "enter keep   esc clear",
        ]
    } else if overlay.zoom {
        &[
            "↑/↓ j/k item   ctrl-d/u scroll   enter open   S sync   R refresh   space list   esc back",
            "j/k item   ctrl-d/u scroll   space list   esc back",
            "space list   esc back",
        ]
    } else if category_bar {
        &[
            "j/k move   [/] or 0-9 category   enter open   / filter   S sync   R refresh   tab feeds   esc close",
            "j/k move   [/] 0-9 category   enter open   / filter   R refresh   esc close",
            "[/] category   enter open   / filter   R refresh   esc close",
            "[/] category   enter open   esc close",
            "enter open   esc close",
            "esc close",
        ]
    } else {
        &[
            "j/k move   ctrl-d/u scroll   enter open   / filter   S sync   R refresh   tab feeds   space zoom   esc close",
            "j/k move   ctrl-d/u scroll   enter open   / filter   S sync   R refresh   esc close",
            "↑/↓ j/k move   enter open   / filter   R refresh   tab feeds   esc close",
            "enter open   / filter   R refresh   esc close",
            "enter open   R refresh   esc close",
            "esc close",
        ]
    };
    // Leave room for the "a-b / n" position at the right when it is drawn.
    let hint_room = usize::from(text_width).saturating_sub(
        if layout.list.is_some() && items.len() > viewport_rows {
            16
        } else {
            0
        },
    );
    let hint = hint
        .iter()
        .find(|hint| hint.chars().count() <= hint_room)
        .copied()
        .unwrap_or("");
    put_text(buffer, x, hint_y, text_width, hint, dim_style(palette));
    Some(FeedRender {
        area,
        tabs,
        groups: group_tabs,
        rows,
        viewport_rows,
        offset,
        detail: detail_rect,
        detail_max_scroll,
        detail_viewport,
    })
}
