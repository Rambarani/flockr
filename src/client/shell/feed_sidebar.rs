//! Job feed sections below the agent list in the expanded sidebar.
//!
//! Each feed is a collapsible section: a header with the feed title and its
//! unread count, then up to `max_rows` item rows (state dot, badge, title) and
//! a "+N more" row that opens the Feed overlay. Clicking a header collapses
//! or expands the section (remembered in the client preferences under a
//! `feed:` key); clicking a row opens the item's link; right-clicking opens a
//! menu with the item's actions. This is presentation of data the client
//! already holds: rendering does no I/O and the agent list above is laid out
//! exactly as before, in the rows the feed section leaves it.
//!
//! A feed that declares groups shows one compact row per non-empty group
//! (`New House PRs 9 ●3`) instead of item rows; clicking one opens the Feed
//! overlay on that category. Such a section does not need its items, so the
//! client does not fetch them for the sidebar.

use std::collections::HashSet;

use ratatui::style::{Modifier, Style};

use super::feed_overlay::is_workable;
use super::feed_state::{FeedClientState, FeedOrigin};
use super::render::{display_width, put_text};
use super::*;
use crate::api::schema::{FeedGroupInfo, FeedItemInfo, FeedItemState, FeedSummary};
use crate::config::FeedsSidebarConfig;

/// Rows the agent panel keeps (its header and a few agents) before any feed
/// section may take space.
const AGENT_MIN_HEIGHT: u16 = 7;
/// The sidebar's last row belongs to the collapse toggle, so the section
/// stops one row above it.
pub(super) const BOTTOM_MARGIN: u16 = 1;
/// Key prefix in the shared collapsed-group sets.
const FEED_KEY_PREFIX: &str = "feed:";

pub(super) fn feed_key(job_id: &str) -> String {
    format!("{FEED_KEY_PREFIX}{job_id}")
}

/// The feeds the sidebar may show: allowed by `show`, and with something to
/// show (items, or an error to explain why there are none).
pub(super) fn sidebar_feeds<'a>(
    state: &'a FeedClientState,
    config: &'a FeedsSidebarConfig,
) -> impl Iterator<Item = &'a FeedSummary> {
    state
        .feeds
        .iter()
        .filter(|feed| config.show.includes(&feed.job_id) && (feed.loaded || feed.error.is_some()))
}

/// Borrowed inputs of the sidebar section for one frame.
#[derive(Clone, Copy)]
pub(super) struct FeedSidebarInput<'a> {
    pub(super) state: &'a FeedClientState,
    pub(super) config: &'a FeedsSidebarConfig,
    pub(super) collapsed: Option<&'a HashSet<String>>,
}

pub(super) fn sidebar_input<'a>(
    state: &'a FeedClientState,
    config: &'a ClientShellConfig,
    active_endpoint_id: &ClientEndpointId,
    local_collapsed: &'a HashSet<String>,
    remote_collapsed: &'a HashMap<ClientEndpointId, HashSet<String>>,
) -> Option<FeedSidebarInput<'a>> {
    (config.feeds.enabled
        && !state.unsupported
        && state.endpoint.as_ref() == Some(active_endpoint_id))
    .then(|| FeedSidebarInput {
        state,
        config: &config.feeds,
        collapsed: if active_endpoint_id.is_local() {
            Some(local_collapsed)
        } else {
            remote_collapsed.get(active_endpoint_id)
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FeedLine {
    Separator,
    Header { section: usize },
    Item { section: usize, index: usize },
    More { section: usize, hidden: usize },
}

/// What the layout needs to know about one section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SectionShape {
    pub(super) collapsed: bool,
    pub(super) items: usize,
    pub(super) row_limit: usize,
}

/// Lay out the section lines in `budget` rows (the separator included).
/// Headers come first for every section that fits; leftover rows go to item
/// rows in order, and a section that cannot show all its items ends in a
/// "+N more" row.
pub(super) fn layout_lines(shapes: &[SectionShape], budget: usize) -> Vec<FeedLine> {
    if budget < 2 || shapes.is_empty() {
        return Vec::new();
    }
    let sections = shapes.len().min(budget - 1);
    let mut extra = budget - 1 - sections;
    let mut lines = vec![FeedLine::Separator];
    for (section, shape) in shapes.iter().take(sections).enumerate() {
        lines.push(FeedLine::Header { section });
        if shape.collapsed || shape.items == 0 {
            continue;
        }
        let shown = shape.items.min(shape.row_limit);
        let beyond = shape.items - shown;
        let needed = shown + usize::from(beyond > 0);
        if needed <= extra {
            lines.extend((0..shown).map(|index| FeedLine::Item { section, index }));
            if beyond > 0 {
                lines.push(FeedLine::More {
                    section,
                    hidden: beyond,
                });
            }
            extra -= needed;
        } else if extra >= 2 {
            let visible = extra - 1;
            lines.extend((0..visible).map(|index| FeedLine::Item { section, index }));
            lines.push(FeedLine::More {
                section,
                hidden: shape.items - visible,
            });
            extra = 0;
        } else if extra == 1 {
            lines.push(FeedLine::More {
                section,
                hidden: shape.items,
            });
            extra = 0;
        }
    }
    lines
}

pub(super) struct FeedSection<'a> {
    pub(super) summary: &'a FeedSummary,
    pub(super) collapsed: bool,
    pub(super) items: &'a [FeedItemInfo],
    /// The non-empty groups of a feed with groups, which replace item rows.
    pub(super) groups: Vec<&'a FeedGroupInfo>,
}

impl FeedSection<'_> {
    fn is_grouped(&self) -> bool {
        !self.summary.groups.is_empty()
    }

    /// Rows the section can show below its header.
    fn row_count(&self) -> usize {
        if self.is_grouped() {
            self.groups.len()
        } else {
            self.summary.count as usize
        }
    }
}

pub(super) struct FeedPlan<'a> {
    pub(super) sections: Vec<FeedSection<'a>>,
    pub(super) lines: Vec<FeedLine>,
}

impl FeedPlan<'_> {
    pub(super) fn height(&self) -> u16 {
        u16::try_from(self.lines.len()).unwrap_or(u16::MAX)
    }
}

/// Decide what the section shows in the detail area of `detail_height` rows.
/// `None` when no feed shows or there is no room without squeezing the agents.
pub(super) fn plan<'a>(input: FeedSidebarInput<'a>, detail_height: u16) -> Option<FeedPlan<'a>> {
    let budget = usize::from(detail_height.saturating_sub(AGENT_MIN_HEIGHT + BOTTOM_MARGIN));
    if budget < 2 {
        return None;
    }
    let sections = sidebar_feeds(input.state, input.config)
        .map(|summary| FeedSection {
            summary,
            collapsed: input
                .collapsed
                .is_some_and(|set| set.contains(&feed_key(&summary.job_id))),
            items: input.state.items_of(&summary.job_id),
            groups: summary
                .groups
                .iter()
                .filter(|group| group.count > 0)
                .collect(),
        })
        .collect::<Vec<_>>();
    let limit = input.config.row_limit();
    let shapes = sections
        .iter()
        .map(|section| SectionShape {
            collapsed: section.collapsed,
            // The total, so "+N more" counts items not fetched yet too. A
            // grouped feed shows every group row, never a limited few.
            items: section.row_count(),
            row_limit: if section.is_grouped() {
                section.row_count()
            } else {
                limit
            },
        })
        .collect::<Vec<_>>();
    let lines = layout_lines(&shapes, budget);
    (!lines.is_empty()).then_some(FeedPlan { sections, lines })
}

#[derive(Default)]
pub(super) struct FeedHeaderHit {
    pub(super) rect: Rect,
    pub(super) job_id: String,
    pub(super) collapsed: bool,
}

pub(super) struct FeedGroupHit {
    pub(super) rect: Rect,
    pub(super) job_id: String,
    pub(super) group_id: String,
}

pub(super) struct FeedRowHit {
    pub(super) rect: Rect,
    pub(super) job_id: String,
    pub(super) item_id: String,
}

/// Where the section drew its lines this frame.
#[derive(Default)]
pub(super) struct FeedSidebarHits {
    pub(super) area: Rect,
    pub(super) headers: Vec<FeedHeaderHit>,
    pub(super) rows: Vec<FeedRowHit>,
    pub(super) groups: Vec<FeedGroupHit>,
    pub(super) more: Vec<(Rect, String)>,
}

fn state_dot(state: FeedItemState, palette: &Palette) -> (&'static str, ratatui::style::Color) {
    match state {
        FeedItemState::Unread => ("●", palette.accent),
        FeedItemState::Read => ("○", palette.overlay0),
        FeedItemState::Done => ("✓", palette.green),
        FeedItemState::Warn => ("▲", palette.peach),
        FeedItemState::Error => ("✖", palette.red),
    }
}

/// Draw `plan` into `area` (the bottom of the sidebar's detail area) and
/// record where everything is.
pub(super) fn render(
    buffer: &mut Buffer,
    area: Rect,
    plan: &FeedPlan<'_>,
    config: &ClientShellConfig,
    hits: &mut FeedSidebarHits,
) {
    let palette = &config.palette;
    hits.area = area;
    for (offset, line) in plan.lines.iter().enumerate() {
        let y = area.y + offset as u16;
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        match *line {
            FeedLine::Separator => put_text(
                buffer,
                rect.x,
                rect.y,
                rect.width,
                &"─".repeat(usize::from(rect.width)),
                Style::default().fg(palette.surface_dim),
            ),
            FeedLine::Header { section } => {
                let section = &plan.sections[section];
                render_header(buffer, rect, section, palette);
                hits.headers.push(FeedHeaderHit {
                    rect,
                    job_id: section.summary.job_id.clone(),
                    collapsed: section.collapsed,
                });
            }
            FeedLine::Item { section, index } => {
                let section = &plan.sections[section];
                if section.is_grouped() {
                    if let Some(group) = section.groups.get(index) {
                        render_group(buffer, rect, group, palette);
                        hits.groups.push(FeedGroupHit {
                            rect,
                            job_id: section.summary.job_id.clone(),
                            group_id: group.id.clone(),
                        });
                    }
                    continue;
                }
                match section.items.get(index) {
                    Some(item) => {
                        render_item(buffer, rect, item, palette);
                        hits.rows.push(FeedRowHit {
                            rect,
                            job_id: section.summary.job_id.clone(),
                            item_id: item.id.clone(),
                        });
                    }
                    // Items not fetched yet: a quiet placeholder.
                    None => put_text(
                        buffer,
                        rect.x,
                        rect.y,
                        rect.width,
                        "   …",
                        Style::default().fg(palette.overlay0),
                    ),
                }
            }
            FeedLine::More { section, hidden } => {
                let section = &plan.sections[section];
                put_text(
                    buffer,
                    rect.x,
                    rect.y,
                    rect.width,
                    &format!("   +{hidden} more"),
                    Style::default()
                        .fg(palette.overlay0)
                        .add_modifier(Modifier::ITALIC),
                );
                hits.more.push((rect, section.summary.job_id.clone()));
            }
        }
    }
}

fn render_header(buffer: &mut Buffer, rect: Rect, section: &FeedSection<'_>, palette: &Palette) {
    let summary = section.summary;
    let marker = if section.collapsed { "▸" } else { "▾" };
    let (count, count_style) = if summary.unread > 0 {
        (
            summary.unread.to_string(),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            format!("({})", summary.count),
            Style::default().fg(palette.overlay0),
        )
    };
    let flag = if summary.stale || (summary.error.is_some() && !summary.loaded) {
        " !"
    } else if summary.refreshing {
        " …"
    } else {
        ""
    };
    let flag_style = Style::default().fg(palette.peach);
    let suffix_width = display_width(&count) + display_width(flag) + 1;
    let title_width = usize::from(rect.width)
        .saturating_sub(3)
        .saturating_sub(usize::from(suffix_width));
    let title = crate::ui::truncate_end(&summary.title, title_width);
    let mut x = rect.x;
    let mut put = |text: &str, style: Style| {
        let room = rect.right().saturating_sub(x);
        put_text(buffer, x, rect.y, room, text, style);
        x = x.saturating_add(display_width(text));
    };
    put(" ", Style::default());
    put(
        marker,
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    put(" ", Style::default());
    put(
        &title,
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD),
    );
    put(" ", Style::default());
    put(&count, count_style);
    put(flag, flag_style);
}

/// `   New House PRs 9 ●3`: the group's short title, its total and its unread.
fn render_group(buffer: &mut Buffer, rect: Rect, group: &FeedGroupInfo, palette: &Palette) {
    let title = group.short_title.as_deref().unwrap_or(&group.title);
    let unread = (group.unread > 0).then(|| format!(" ●{}", group.unread));
    let total = group.count.to_string();
    let suffix_width = 1 + display_width(&total) + unread.as_deref().map_or(0, display_width);
    let title_room = usize::from(rect.width)
        .saturating_sub(3)
        .saturating_sub(usize::from(suffix_width));
    let title = crate::ui::truncate_end(title, title_room);
    let title_style = if group.unread > 0 {
        Style::default().fg(palette.text)
    } else {
        Style::default().fg(palette.subtext0)
    };
    let mut x = rect.x;
    let mut put = |text: &str, style: Style| {
        let room = rect.right().saturating_sub(x);
        put_text(buffer, x, rect.y, room, text, style);
        x = x.saturating_add(display_width(text).min(room));
    };
    put("   ", Style::default());
    put(&title, title_style);
    put(" ", Style::default());
    put(&total, Style::default().fg(palette.overlay0));
    if let Some(unread) = &unread {
        put(
            unread,
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        );
    }
}

fn render_item(buffer: &mut Buffer, rect: Rect, item: &FeedItemInfo, palette: &Palette) {
    let (dot, dot_color) = state_dot(item.state, palette);
    let title_style = if item.state == FeedItemState::Unread {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.subtext0)
    };
    // Keep room for at least a few characters of the title.
    let badge = item
        .badge
        .as_deref()
        .map(|badge| crate::ui::truncate_end(badge, usize::from(rect.width / 3).max(4)));
    let mut x = rect.x;
    let mut put = |buffer: &mut Buffer, text: &str, style: Style| {
        let room = rect.right().saturating_sub(x);
        put_text(buffer, x, rect.y, room, text, style);
        x = x.saturating_add(display_width(text).min(room));
    };
    put(buffer, "  ", Style::default());
    put(buffer, dot, Style::default().fg(dot_color));
    put(buffer, " ", Style::default());
    let mut used = 4;
    if let Some(badge) = &badge {
        put(buffer, badge, Style::default().fg(palette.overlay0));
        put(buffer, " ", Style::default());
        used += display_width(badge) + 1;
    }
    let room = usize::from(rect.width.saturating_sub(used));
    put(
        buffer,
        &crate::ui::truncate_end(&item.title, room),
        title_style,
    );
}

impl ClientShellState {
    fn feed_header_at(&self, point: (u16, u16)) -> Option<(String, bool)> {
        self.hits
            .feed
            .headers
            .iter()
            .find(|hit| contains(hit.rect, point))
            .map(|hit| (hit.job_id.clone(), hit.collapsed))
    }

    fn feed_row_at(&self, point: (u16, u16)) -> Option<(String, String)> {
        self.hits
            .feed
            .rows
            .iter()
            .find(|hit| contains(hit.rect, point))
            .map(|hit| (hit.job_id.clone(), hit.item_id.clone()))
    }

    fn feed_group_at(&self, point: (u16, u16)) -> Option<(String, String)> {
        self.hits
            .feed
            .groups
            .iter()
            .find(|hit| contains(hit.rect, point))
            .map(|hit| (hit.job_id.clone(), hit.group_id.clone()))
    }

    fn feed_more_at(&self, point: (u16, u16)) -> Option<String> {
        self.hits
            .feed
            .more
            .iter()
            .find(|(rect, _)| contains(*rect, point))
            .map(|(_, job_id)| job_id.clone())
    }

    pub(super) fn feed_item(&self, job_id: &str, item_id: &str) -> Option<FeedItemInfo> {
        self.feeds
            .items_of(job_id)
            .iter()
            .find(|item| item.id == item_id)
            .cloned()
    }

    /// Collapse or expand a feed section and remember the choice.
    pub(super) fn toggle_feed_section(&mut self, job_id: &str, outcome: &mut ClientShellInput) {
        let endpoint_id = self.active_endpoint_id.clone();
        self.toggle_collapsed_group(&endpoint_id, feed_key(job_id));
        self.persist_chrome_preferences(outcome);
        // Opening a section may need items the client has not fetched.
        self.feeds.poll_soon(std::time::Instant::now());
        outcome.repaint = true;
    }

    /// Left click in the sidebar's feed section. True when it was one.
    pub(super) fn handle_feed_sidebar_click(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !contains(self.hits.feed.area, point) {
            return false;
        }
        if let Some((job_id, _)) = self.feed_header_at(point) {
            self.toggle_feed_section(&job_id, outcome);
            return true;
        }
        if let Some(job_id) = self.feed_more_at(point) {
            self.open_feed_overlay(Some(job_id));
            outcome.repaint = true;
            return true;
        }
        if let Some((job_id, group_id)) = self.feed_group_at(point) {
            // The overlay opens on the category the row names.
            self.store_feed_group(&job_id, Some(&group_id), outcome);
            self.open_feed_overlay(Some(job_id));
            outcome.repaint = true;
            return true;
        }
        if let Some((job_id, item_id)) = self.feed_row_at(point) {
            let Some(item) = self.feed_item(&job_id, &item_id) else {
                return true;
            };
            match item.url {
                Some(url) => outcome.actions.push(ClientShellAction::OpenSafeWebUrl(url)),
                None if !item.actions.is_empty() => {
                    self.open_feed_context_menu(point);
                }
                None => {}
            }
            outcome.repaint = true;
            return true;
        }
        // The separator line: part of the section, nothing to do.
        true
    }

    /// Right click in the sidebar's feed section. True when it was one.
    pub(super) fn open_feed_context_menu(&mut self, point: (u16, u16)) -> bool {
        if !contains(self.hits.feed.area, point) {
            return false;
        }
        let target = if let Some((job_id, item_id)) = self.feed_row_at(point) {
            let Some(item) = self.feed_item(&job_id, &item_id) else {
                return true;
            };
            let collapsed = self.group_is_collapsed(&self.active_endpoint_id, &feed_key(&job_id));
            ClientContextMenuTarget::Feed {
                job_id,
                item: Some(ClientFeedMenuItem {
                    item_id: item.id,
                    url: item.url,
                    actions: item
                        .actions
                        .into_iter()
                        .map(|action| ClientFeedMenuAction {
                            key: action.key,
                            title: action.title,
                            confirm: action.confirm,
                        })
                        .collect(),
                }),
                group: None,
                collapsed,
            }
        } else if let Some((job_id, collapsed)) = self.feed_header_at(point) {
            ClientContextMenuTarget::Feed {
                job_id,
                item: None,
                group: None,
                collapsed,
            }
        } else if let Some((job_id, group_id)) = self.feed_group_at(point) {
            let collapsed = self.group_is_collapsed(&self.active_endpoint_id, &feed_key(&job_id));
            let title = self
                .feeds
                .summary(&job_id)
                .and_then(|feed| feed.groups.iter().find(|group| group.id == group_id))
                .map(|group| {
                    group
                        .short_title
                        .clone()
                        .unwrap_or_else(|| group.title.clone())
                })
                .unwrap_or_else(|| group_id.clone());
            ClientContextMenuTarget::Feed {
                job_id,
                item: None,
                group: Some((group_id, title)),
                collapsed,
            }
        } else if let Some(job_id) = self.feed_more_at(point) {
            let collapsed = self.group_is_collapsed(&self.active_endpoint_id, &feed_key(&job_id));
            ClientContextMenuTarget::Feed {
                job_id,
                item: None,
                group: None,
                collapsed,
            }
        } else {
            return true;
        };
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target,
            x: point.0,
            y: point.1,
            highlighted: 0,
        }));
        true
    }

    /// Run a chosen feed context menu entry.
    pub(super) fn activate_feed_context_action(
        &mut self,
        job_id: String,
        item: Option<ClientFeedMenuItem>,
        group: Option<(String, String)>,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::FeedWorkAll => {
                if let Some((group_id, _)) = group {
                    let ids: Vec<String> = self
                        .feeds
                        .items_of(&job_id)
                        .iter()
                        .filter(|item| item.in_group(&group_id) && is_workable(item))
                        .map(|item| item.id.clone())
                        .collect();
                    self.start_feed_work(job_id, ids, outcome);
                }
            }
            ClientContextMenuAction::FeedOpenUrl => {
                if let Some(url) = item.and_then(|item| item.url) {
                    outcome.actions.push(ClientShellAction::OpenSafeWebUrl(url));
                }
            }
            ClientContextMenuAction::FeedRunAction(index) => {
                if let Some(menu_item) = item {
                    let Some(action) = menu_item.actions.get(index).cloned() else {
                        outcome.repaint = true;
                        return;
                    };
                    // The menu is a snapshot from when it opened. If the item
                    // has gone since (a refresh dropped it), do not send an
                    // action for it.
                    let Some(live) = self.feed_item(&job_id, &menu_item.item_id) else {
                        self.push_endpoint_notice(
                            ClientEndpointNoticeKind::Rejected,
                            format!("feed.action:{job_id}"),
                            "Feed action not sent",
                            format!(
                                "{} is no longer in the feed; nothing was run",
                                menu_item.item_id
                            ),
                        );
                        outcome.repaint = true;
                        return;
                    };
                    // Ask if either the snapshot or the live item says to.
                    let question = action.confirm.or_else(|| {
                        live.actions
                            .into_iter()
                            .find(|candidate| candidate.key == action.key)
                            .and_then(|candidate| candidate.confirm)
                    });
                    self.run_feed_action_with_question(
                        job_id,
                        menu_item.item_id,
                        action.key,
                        question,
                        FeedOrigin::Elsewhere,
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::FeedRefresh => {
                self.refresh_feed(job_id, FeedOrigin::Elsewhere, outcome)
            }
            ClientContextMenuAction::FeedSyncNow => {
                self.sync_feed_now(job_id, FeedOrigin::Elsewhere, outcome)
            }
            ClientContextMenuAction::FeedToggle => self.toggle_feed_section(&job_id, outcome),
            ClientContextMenuAction::FeedOpenOverlay => self.open_feed_overlay(Some(job_id)),
            _ => {}
        }
        outcome.repaint = true;
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn shape(collapsed: bool, items: usize, row_limit: usize) -> SectionShape {
        SectionShape {
            collapsed,
            items,
            row_limit,
        }
    }

    #[test]
    fn everything_fits_with_a_separator_headers_items_and_a_more_row() {
        let lines = layout_lines(&[shape(false, 12, 3), shape(false, 1, 3)], 20);
        assert_eq!(
            lines,
            [
                FeedLine::Separator,
                FeedLine::Header { section: 0 },
                FeedLine::Item {
                    section: 0,
                    index: 0
                },
                FeedLine::Item {
                    section: 0,
                    index: 1
                },
                FeedLine::Item {
                    section: 0,
                    index: 2
                },
                FeedLine::More {
                    section: 0,
                    hidden: 9
                },
                FeedLine::Header { section: 1 },
                FeedLine::Item {
                    section: 1,
                    index: 0
                },
            ]
        );
    }

    #[test]
    fn collapsed_and_empty_sections_show_only_their_header() {
        let lines = layout_lines(&[shape(true, 5, 8), shape(false, 0, 8)], 10);
        assert_eq!(
            lines,
            [
                FeedLine::Separator,
                FeedLine::Header { section: 0 },
                FeedLine::Header { section: 1 },
            ]
        );
    }

    #[test]
    fn a_tight_budget_keeps_every_header_before_any_item() {
        // Budget 5: separator + two headers leave two spare rows.
        let lines = layout_lines(&[shape(false, 4, 8), shape(false, 4, 8)], 5);
        assert_eq!(lines.len(), 5);
        assert_eq!(
            lines,
            [
                FeedLine::Separator,
                FeedLine::Header { section: 0 },
                FeedLine::Item {
                    section: 0,
                    index: 0
                },
                FeedLine::More {
                    section: 0,
                    hidden: 3
                },
                FeedLine::Header { section: 1 },
            ]
        );
        // One spare row becomes a lone "+N more".
        let lines = layout_lines(&[shape(false, 4, 8)], 3);
        assert_eq!(
            lines,
            [
                FeedLine::Separator,
                FeedLine::Header { section: 0 },
                FeedLine::More {
                    section: 0,
                    hidden: 4
                },
            ]
        );
    }

    #[test]
    fn sections_that_cannot_get_a_header_are_left_out() {
        let lines = layout_lines(
            &[shape(false, 1, 8), shape(false, 1, 8), shape(false, 1, 8)],
            3,
        );
        assert_eq!(
            lines,
            [
                FeedLine::Separator,
                FeedLine::Header { section: 0 },
                FeedLine::Header { section: 1 },
            ]
        );
        assert!(layout_lines(&[shape(false, 1, 8)], 1).is_empty());
        assert!(layout_lines(&[], 10).is_empty());
    }
}
