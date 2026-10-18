//! Feed overlay: the full list of a job feed, with a picker when there are
//! several feeds. Enter opens the selected item's link, the item's action
//! keys run its actions, `/` filters, `R` refreshes the feed, Tab or the
//! arrow keys switch feeds, and Esc closes.
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
use crate::api::schema::{FeedItemInfo, FeedItemState, FeedSummary};
use crate::jobs::display;

const MIN_PANEL_WIDTH: u16 = 30;
const MIN_PANEL_HEIGHT: u16 = 9;
/// Rows above the list: tabs or title, info, filter.
const HEADER_ROWS: u16 = 3;
/// Rows below the list: selected item detail, status, hint.
const FOOTER_ROWS: u16 = 3;
const MAX_BADGE_WIDTH: usize = 14;
const AGE_WIDTH: usize = 8;

#[derive(Debug)]
pub(super) struct ClientFeedOverlay {
    /// The feed shown; the first feed until one is chosen.
    pub(super) job_id: Option<String>,
    /// The selected item, by id so it survives refreshes.
    pub(super) selected: Option<String>,
    pub(super) scroll: usize,
    pub(super) filter: String,
    pub(super) filtering: bool,
    pub(super) status: Option<String>,
}

#[derive(Default)]
pub(super) struct FeedOverlayHits {
    pub(super) popup: Rect,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
}

pub(super) struct FeedRender {
    pub(super) area: Rect,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
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
        self.overlay = Some(ClientShellOverlay::Feed(ClientFeedOverlay {
            job_id,
            selected: None,
            scroll: 0,
            filter: String::new(),
            filtering: false,
            status: None,
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

    pub(super) fn visible_feed_items(&self) -> Vec<&FeedItemInfo> {
        let (Some(overlay), Some(feed)) = (self.feed_overlay(), self.current_feed()) else {
            return Vec::new();
        };
        self.feeds
            .items_of(&feed.job_id)
            .iter()
            .filter(|item| matches_filter(item, &overlay.filter))
            .collect()
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
        if let Some(overlay) = self.feed_overlay_mut() {
            overlay.selected = Some(id);
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
        if let Some(overlay) = self.feed_overlay_mut() {
            overlay.job_id = Some(job_id);
            overlay.selected = None;
            overlay.scroll = 0;
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
                let has_filter = self
                    .feed_overlay()
                    .is_some_and(|overlay| !overlay.filter.is_empty());
                if has_filter {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filter.clear();
                    }
                    self.reset_feed_list_position();
                } else {
                    self.overlay = None;
                }
            }
            KeyCode::Enter if plain => self.open_selected_feed_item(outcome),
            KeyCode::Tab if plain => self.switch_feed(1, outcome),
            KeyCode::BackTab => self.switch_feed(-1, outcome),
            KeyCode::Tab if modifiers == KeyModifiers::SHIFT => self.switch_feed(-1, outcome),
            KeyCode::Right if plain => self.switch_feed(1, outcome),
            KeyCode::Left if plain => self.switch_feed(-1, outcome),
            _ => match typed {
                Some('q') => self.overlay = None,
                Some('/') => {
                    if let Some(overlay) = self.feed_overlay_mut() {
                        overlay.filtering = true;
                    }
                }
                Some('R') => self.refresh_current_feed(outcome),
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
                        if let Some(overlay) = self.feed_overlay_mut() {
                            overlay.selected = Some(id);
                        }
                        // A second click on the selected row opens it.
                        if already {
                            self.open_selected_feed_item(outcome);
                        }
                    }
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollUp => {
                self.move_feed_selection(-3);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                self.move_feed_selection(3);
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
        rows: Vec::new(),
        viewport_rows: 1,
        offset: 0,
    };
    if inner.height < HEADER_ROWS + FOOTER_ROWS + 1 || inner.width < 20 {
        return Some(empty);
    }
    let text_width = inner.width.saturating_sub(2);
    let x = inner.x + 1;
    let hint_y = inner.bottom() - 1;
    let status_y = inner.bottom() - 2;
    let detail_y = inner.bottom() - 3;
    let fit = |text: &str, width: u16| crate::ui::truncate_end(text, usize::from(width));
    let feeds = &state.feeds.feeds;
    let current = state.current_feed();
    let mut tabs = Vec::new();

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
        if feed.refreshing {
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

    // Line 3: the filter.
    if overlay.filtering || !overlay.filter.is_empty() {
        let cursor = if overlay.filtering { "▏" } else { "" };
        put_text(
            buffer,
            x,
            inner.y + 2,
            text_width,
            &fit(&format!("/ {}{cursor}", overlay.filter), text_width),
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        );
    }

    let list_top = inner.y + HEADER_ROWS;
    let list_height = detail_y.saturating_sub(list_top);
    let viewport_rows = usize::from(list_height).max(1);
    let items = state.visible_feed_items();
    let selected_index = state.selected_feed_index(&items);
    let max_scroll = items.len().saturating_sub(viewport_rows);
    let offset = overlay.scroll.min(max_scroll);
    let badge_width = items
        .iter()
        .filter_map(|item| item.badge.as_deref())
        .map(|badge| usize::from(display_width(badge)))
        .max()
        .unwrap_or(0)
        .min(MAX_BADGE_WIDTH);
    let show_age =
        items.iter().any(|item| item.updated_at.is_some()) && usize::from(text_width) >= 50;
    let mut rows = Vec::new();
    for (row, (index, item)) in items
        .iter()
        .enumerate()
        .skip(offset)
        .take(viewport_rows)
        .enumerate()
    {
        let y = list_top + row as u16;
        let rect = Rect::new(x, y, text_width, 1);
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
        if let Some(subtitle) = &item.subtitle {
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
        let message = if state.feeds.unsupported {
            "This server does not support feeds yet. Update and restart it."
        } else if feeds.is_empty() {
            "No feeds. Add a feed table to a job's job.toml (see the Jobs docs)."
        } else if !overlay.filter.is_empty() {
            "No items match the filter."
        } else if current.is_some_and(|feed| feed.error.is_some() && !feed.loaded) {
            "The feed could not be read yet; see the line above."
        } else if current.is_some_and(|feed| feed.count > 0) {
            "loading…"
        } else {
            "Nothing here."
        };
        put_text(
            buffer,
            x,
            list_top,
            text_width,
            &fit(message, text_width),
            dim_style(palette),
        );
    }
    if items.len() > viewport_rows {
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

    // The selected item's actions.
    if let Some(item) = items.get(selected_index) {
        let mut cursor_x = x;
        let mut put = |text: &str, style: Style| {
            let room = inner.right().saturating_sub(1).saturating_sub(cursor_x);
            put_text(buffer, cursor_x, detail_y, room, text, style);
            cursor_x = cursor_x.saturating_add(display_width(text).min(room));
        };
        if item.actions.is_empty() {
            let text = item
                .url
                .clone()
                .or_else(|| item.subtitle.clone())
                .unwrap_or_default();
            put(&fit(&text, text_width), dim_style(palette));
        } else {
            for action in &item.actions {
                put(&action.key, key_style(palette));
                put(&format!(" {}   ", action.title), dim_style(palette));
            }
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
    } else {
        &[
            "↑/↓ j/k move   enter open   / filter   R refresh   tab feeds   esc close",
            "enter open   / filter   R refresh   esc close",
            "enter open   R refresh   esc close",
            "esc close",
        ]
    };
    let hint = hint
        .iter()
        .find(|hint| hint.chars().count() <= usize::from(text_width))
        .copied()
        .unwrap_or("");
    put_text(buffer, x, hint_y, text_width, hint, dim_style(palette));
    Some(FeedRender {
        area,
        tabs,
        rows,
        viewport_rows,
        offset,
    })
}
