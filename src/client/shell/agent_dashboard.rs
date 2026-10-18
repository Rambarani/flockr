//! Agent dashboard: a full-screen table of every agent on every machine with
//! its state, time in state and last output line. Sort with `s`, filter with
//! `/`, Enter jumps to the agent's pane, `r` renames, Esc closes.
//!
//! Rows come from each endpoint's cached snapshot. The last output line of a
//! pane in the visible surface is read from the frame the client already has;
//! other panes on the active machine are read one at a time with the existing
//! `pane.read` method while the dashboard is open. Other machines have no
//! command lane, so their rows fall back to the agent's reported title.
//!
//! There is no activity sparkline: neither the snapshot nor the surface
//! carries an output-rate or timestamp series (only a state change counter,
//! and content revisions for visible panes), so a chart would be invented.

use std::time::{Duration, Instant};

use crossterm::event::MouseEvent;
use ratatui::layout::Constraint;
use ratatui::style::Modifier;
use ratatui::text::Span;
use ratatui::widgets::{Cell, Row, StatefulWidget, Table, TableState};

use super::agent_activity::{format_age, StateAge};
use super::agent_preview::{tail_lines, AgentTarget, PaneReadPurpose};
use super::popup_frame::{
    base_style, dim_style, draw_panel, heading_style, key_style, selected_style,
};
use super::render::{display_width, put_text};
use super::*;

const POLL_GAP: Duration = Duration::from_millis(250);
const LINE_MAX_AGE: Duration = Duration::from_secs(3);
const REPAINT_INTERVAL: Duration = Duration::from_secs(1);
const READ_WINDOW_LINES: u32 = 60;
const MIN_PANEL_WIDTH: u16 = 30;
const MIN_PANEL_HEIGHT: u16 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DashboardSort {
    #[default]
    Blocked,
    Idle,
    Workspace,
    Name,
}

impl DashboardSort {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Blocked => Self::Idle,
            Self::Idle => Self::Workspace,
            Self::Workspace => Self::Name,
            Self::Name => Self::Blocked,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked first",
            Self::Idle => "longest idle",
            Self::Workspace => "workspace",
            Self::Name => "name",
        }
    }
}

#[derive(Debug)]
struct LastLine {
    text: String,
    at: Instant,
}

#[derive(Debug)]
pub(super) struct ClientDashboardOverlay {
    pub(super) sort: DashboardSort,
    pub(super) filter: TextEditor,
    pub(super) filter_focused: bool,
    /// The selected agent, tracked by identity so live re-sorting does not
    /// move the selection onto a different agent.
    pub(super) selected: Option<AgentTarget>,
    pub(super) scroll: usize,
    lines: HashMap<(String, String), LastLine>,
    lines_endpoint: ClientEndpointId,
    last_poll: Option<Instant>,
    last_paint: Option<Instant>,
    reads_unsupported: bool,
}

#[derive(Default)]
pub(super) struct DashboardHits {
    pub(super) popup: Rect,
    pub(super) header: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DashboardRow {
    pub(super) target: AgentTarget,
    pub(super) machine: String,
    pub(super) workspace: String,
    pub(super) tab: String,
    pub(super) pane: String,
    pub(super) name: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) age: Option<StateAge>,
    pub(super) last_line: Option<String>,
    /// The last line is the agent's reported title, not screen output.
    pub(super) last_line_is_title: bool,
    pub(super) stale: bool,
    order: (usize, usize, usize),
}

fn idle_like(status: crate::api::schema::AgentStatus) -> bool {
    use crate::api::schema::AgentStatus;
    matches!(status, AgentStatus::Idle | AgentStatus::Done)
}

/// Case-insensitive match of every filter word against the row's text.
fn row_matches(row: &DashboardRow, filter: &str) -> bool {
    let words = filter.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {} {} {}",
        row.name,
        row.machine,
        row.workspace,
        row.tab,
        row.pane,
        status_text(row.status),
        row.last_line.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    words
        .iter()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

pub(super) fn sort_rows(rows: &mut [DashboardRow], sort: DashboardSort) {
    let elapsed = |row: &DashboardRow| row.age.map_or(Duration::ZERO, |age| age.elapsed);
    match sort {
        DashboardSort::Blocked => rows.sort_by(|left, right| {
            status_priority(right.status)
                .cmp(&status_priority(left.status))
                .then_with(|| elapsed(right).cmp(&elapsed(left)))
                .then_with(|| left.order.cmp(&right.order))
        }),
        DashboardSort::Idle => rows.sort_by(|left, right| {
            idle_like(right.status)
                .cmp(&idle_like(left.status))
                .then_with(|| {
                    if idle_like(left.status) {
                        elapsed(right).cmp(&elapsed(left))
                    } else {
                        status_priority(right.status)
                            .cmp(&status_priority(left.status))
                            .then_with(|| elapsed(right).cmp(&elapsed(left)))
                    }
                })
                .then_with(|| left.order.cmp(&right.order))
        }),
        DashboardSort::Workspace => rows.sort_by_key(|row| row.order),
        DashboardSort::Name => rows.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.order.cmp(&right.order))
        }),
    }
}

impl ClientShellState {
    pub(super) fn open_agent_dashboard(&mut self) {
        self.overlay = Some(ClientShellOverlay::Dashboard(ClientDashboardOverlay {
            sort: self.dashboard_sort,
            filter: TextEditor::default(),
            filter_focused: false,
            selected: None,
            scroll: 0,
            lines: HashMap::new(),
            lines_endpoint: self.active_endpoint_id.clone(),
            last_poll: None,
            last_paint: None,
            reads_unsupported: false,
        }));
    }

    /// Every agent on every machine, filtered and sorted as the overlay asks.
    pub(super) fn dashboard_rows(
        &self,
        overlay: &ClientDashboardOverlay,
        now: Instant,
    ) -> Vec<DashboardRow> {
        let federated = self.endpoints.len() > 1;
        let mut rows = Vec::new();
        for endpoint in super::aggregate_navigation::cached_endpoint_snapshots(&self.endpoints) {
            let snapshot = endpoint.snapshot;
            for (agent_index, agent) in snapshot.agents.iter().enumerate() {
                let workspace_index = snapshot
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.workspace_id == agent.workspace_id);
                let workspace = workspace_index.map_or("workspace", |index| {
                    snapshot.workspaces[index].label.as_str()
                });
                let tab = snapshot
                    .tabs
                    .iter()
                    .find(|tab| tab.tab_id == agent.tab_id)
                    .map_or("tab", |tab| tab.label.as_str());
                let target = AgentTarget {
                    endpoint_id: endpoint.endpoint_id.clone(),
                    pane_id: agent.pane_id.clone(),
                };
                let output = self
                    .local_surface_lines(&target, 1)
                    .and_then(|mut lines| lines.pop())
                    .or_else(|| {
                        (overlay.lines_endpoint == target.endpoint_id)
                            .then(|| {
                                overlay
                                    .lines
                                    .get(&(target.endpoint_id.storage_key(), agent.pane_id.clone()))
                            })
                            .flatten()
                            .map(|line| line.text.clone())
                    })
                    .filter(|line| !line.is_empty());
                let title = agent
                    .title
                    .as_deref()
                    .or(agent.terminal_title_stripped.as_deref())
                    .map(str::to_owned)
                    .filter(|title| !title.is_empty());
                let (last_line, last_line_is_title) = match (output, title) {
                    (Some(line), _) => (Some(line), false),
                    (None, Some(title)) => (Some(title), true),
                    (None, None) => (None, false),
                };
                rows.push(DashboardRow {
                    machine: endpoint.label.to_owned(),
                    workspace: if federated {
                        format!("{} · {workspace}", endpoint.label)
                    } else {
                        workspace.to_owned()
                    },
                    tab: tab.to_owned(),
                    pane: agent.pane_id.clone(),
                    name: agent
                        .name
                        .as_deref()
                        .or(agent.display_agent.as_deref())
                        .or(agent.agent.as_deref())
                        .or(agent.title.as_deref())
                        .unwrap_or("agent")
                        .to_owned(),
                    status: agent.agent_status,
                    age: self
                        .agent_clock
                        .age(&target.endpoint_id, &agent.pane_id, now),
                    last_line,
                    last_line_is_title,
                    stale: endpoint.stale(),
                    order: (
                        endpoint.endpoint_index,
                        workspace_index.unwrap_or(usize::MAX),
                        agent_index,
                    ),
                    target,
                });
            }
        }
        rows.retain(|row| row_matches(row, &overlay.filter));
        sort_rows(&mut rows, overlay.sort);
        rows
    }

    fn dashboard_view(&self) -> Option<(&ClientDashboardOverlay, Vec<DashboardRow>)> {
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_ref() else {
            return None;
        };
        Some((overlay, self.dashboard_rows(overlay, Instant::now())))
    }

    fn selected_dashboard_index(rows: &[DashboardRow], overlay: &ClientDashboardOverlay) -> usize {
        overlay
            .selected
            .as_ref()
            .and_then(|target| rows.iter().position(|row| row.target == *target))
            .unwrap_or(0)
    }

    fn move_dashboard_selection(&mut self, delta: isize) {
        let Some((overlay, rows)) = self.dashboard_view() else {
            return;
        };
        let viewport = self.hits.dashboard.viewport_rows.max(1);
        let offset = self.hits.dashboard.offset;
        if rows.is_empty() {
            return;
        }
        let current = Self::selected_dashboard_index(&rows, overlay);
        let next = (current as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        let target = rows[next].target.clone();
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() else {
            return;
        };
        overlay.selected = Some(target);
        // The renderer may have adjusted the offset (resize, selection
        // clamping), so scroll relative to what is actually on screen.
        overlay.scroll = if next < offset {
            next
        } else if next >= offset + viewport {
            next + 1 - viewport
        } else {
            offset
        };
    }

    fn move_dashboard_selection_to(&mut self, index: usize) {
        let Some((_, rows)) = self.dashboard_view() else {
            return;
        };
        let Some(target) = rows.get(index).map(|row| row.target.clone()) else {
            return;
        };
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() else {
            return;
        };
        overlay.selected = Some(target);
    }

    fn jump_to_dashboard_selection(&mut self, outcome: &mut ClientShellInput) {
        let Some((overlay, rows)) = self.dashboard_view() else {
            return;
        };
        let Some(row) = rows.get(Self::selected_dashboard_index(&rows, overlay)) else {
            return;
        };
        let target = row.target.clone();
        self.dashboard_sort = overlay.sort;
        self.overlay = None;
        self.focus_or_activate(
            target.endpoint_id,
            ClientEndpointFocusTarget::Pane(target.pane_id),
            outcome,
        );
        outcome.repaint = true;
    }

    fn rename_dashboard_selection(&mut self, outcome: &mut ClientShellInput) {
        let Some((overlay, rows)) = self.dashboard_view() else {
            return;
        };
        let Some(row) = rows.get(Self::selected_dashboard_index(&rows, overlay)) else {
            return;
        };
        let target = row.target.clone();
        let machine = row.machine.clone();
        if target.endpoint_id != self.active_endpoint_id {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "dashboard_rename_other_machine",
                "Select the machine first",
                format!("Agents on {machine} can only be renamed after you switch to it"),
            );
            outcome.repaint = true;
            return;
        }
        self.dashboard_sort = overlay.sort;
        if self.open_rename_agent_overlay(&target.pane_id) {
            outcome.repaint = true;
        }
    }

    pub(super) fn insert_dashboard_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() else {
            return false;
        };
        if overlay.filter_focused {
            overlay.filter.insert(text);
            overlay.scroll = 0;
        }
        true
    }

    /// Handle a key while the dashboard is open. Returns false when it is not.
    pub(super) fn route_dashboard_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::KeyModifiers;

        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_ref() else {
            return false;
        };
        let focused = overlay.filter_focused;
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        let plain = modifiers.is_empty();
        let ctrl = modifiers == KeyModifiers::CONTROL;
        let page = self.hits.dashboard.viewport_rows.max(1) as isize;
        outcome.repaint = true;
        match code {
            KeyCode::Enter if plain => self.jump_to_dashboard_selection(outcome),
            KeyCode::Up if plain => self.move_dashboard_selection(-1),
            KeyCode::Down if plain => self.move_dashboard_selection(1),
            KeyCode::Char('p') if ctrl => self.move_dashboard_selection(-1),
            KeyCode::Char('n') if ctrl => self.move_dashboard_selection(1),
            KeyCode::PageUp if plain => self.move_dashboard_selection(-page),
            KeyCode::PageDown if plain => self.move_dashboard_selection(page),
            KeyCode::Esc => {
                if focused {
                    self.set_dashboard_filter_focus(false);
                } else {
                    if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_ref() {
                        self.dashboard_sort = overlay.sort;
                    }
                    self.overlay = None;
                }
            }
            _ if focused => {
                if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
                    match overlay.filter.handle_key(key) {
                        Some(true) => overlay.scroll = 0,
                        Some(false) => {}
                        None => outcome.repaint = false,
                    }
                }
            }
            KeyCode::Home if plain => self.move_dashboard_selection_to(0),
            KeyCode::End if plain => self.move_dashboard_selection(isize::MAX / 2),
            KeyCode::Char('u') if ctrl => {
                if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
                    overlay.filter.clear();
                    overlay.scroll = 0;
                }
            }
            _ => {
                // Match the typed character so layouts where `/` needs Shift
                // work when the host reports the key as shifted.
                let shifted_without_evidence = key.modifiers.contains(KeyModifiers::SHIFT)
                    && key.generated_text.is_none()
                    && key.shifted_codepoint.is_none();
                let typed = (!shifted_without_evidence)
                    .then(|| crate::input::keybind_help_text_char(key))
                    .flatten();
                match typed {
                    Some('s') => {
                        if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut()
                        {
                            overlay.sort = overlay.sort.next();
                            overlay.scroll = 0;
                        }
                    }
                    Some('/') => self.set_dashboard_filter_focus(true),
                    Some('r') => self.rename_dashboard_selection(outcome),
                    Some('j') => self.move_dashboard_selection(1),
                    Some('k') => self.move_dashboard_selection(-1),
                    Some('g') => self.move_dashboard_selection_to(0),
                    Some('G') => self.move_dashboard_selection(isize::MAX / 2),
                    _ => outcome.repaint = false,
                }
            }
        }
        true
    }

    fn set_dashboard_filter_focus(&mut self, focused: bool) {
        if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
            overlay.filter_focused = focused;
        }
    }

    /// Handle a mouse event while the dashboard is open. Returns false when it
    /// is not, so the event continues to the regular routing.
    pub(super) fn route_dashboard_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        if !matches!(self.overlay, Some(ClientShellOverlay::Dashboard(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let row = self
                    .hits
                    .dashboard
                    .rows
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, index)| *index);
                if let Some(index) = row {
                    self.move_dashboard_selection_to(index);
                    self.jump_to_dashboard_selection(outcome);
                } else if contains(self.hits.dashboard.header, point) {
                    if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
                        overlay.sort = overlay.sort.next();
                        overlay.scroll = 0;
                    }
                } else if !contains(self.hits.dashboard.popup, point) {
                    if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_ref() {
                        self.dashboard_sort = overlay.sort;
                    }
                    self.overlay = None;
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollUp => {
                self.move_dashboard_selection(-3);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                self.move_dashboard_selection(3);
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }

    /// While open: read the next stale pane's last line, one request at a
    /// time, and repaint every second so times in state advance.
    pub(crate) fn tick_agent_dashboard(
        &mut self,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_ref() else {
            return false;
        };
        let mut repaint = overlay
            .last_paint
            .is_none_or(|at| now.saturating_duration_since(at) >= REPAINT_INTERVAL);
        let in_flight = self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::PaneRead {
                    purpose: PaneReadPurpose::Dashboard,
                    ..
                }
            )
        });
        let poll_due = !in_flight
            && !overlay.reads_unsupported
            && overlay
                .last_poll
                .is_none_or(|at| now.saturating_duration_since(at) >= POLL_GAP);
        let mut request = None;
        if poll_due {
            let rows = self.dashboard_rows(overlay, now);
            let first = overlay.scroll.min(rows.len());
            // Visible rows first, then the rest, each at most every few seconds.
            request = rows[first..]
                .iter()
                .chain(&rows[..first])
                .filter(|row| {
                    row.target.endpoint_id == self.active_endpoint_id
                        && self.local_surface_lines(&row.target, 1).is_none()
                })
                .filter_map(|row| {
                    let fetched = overlay
                        .lines
                        .get(&(row.target.endpoint_id.storage_key(), row.pane.clone()))
                        .map(|line| line.at);
                    fetched
                        .is_none_or(|at| now.saturating_duration_since(at) >= LINE_MAX_AGE)
                        .then_some((fetched, row.pane.clone()))
                })
                .min_by_key(|(fetched, _)| *fetched)
                .map(|(_, pane_id)| pane_id);
        }
        if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
            if repaint {
                overlay.last_paint = Some(now);
            }
            if poll_due {
                overlay.last_poll = Some(now);
            }
        }
        if let Some(pane_id) = request {
            let method = crate::api::schema::Method::PaneRead(crate::api::schema::PaneReadParams {
                pane_id: pane_id.clone(),
                source: crate::api::schema::ReadSource::Visible,
                lines: Some(READ_WINDOW_LINES),
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
                intent: crate::api::schema::ReadIntent::Interactive,
            });
            if self.supports_endpoint_method(&method) {
                self.push_endpoint_method_with_kind(
                    method,
                    PendingEndpointKind::PaneRead {
                        pane_id,
                        purpose: PaneReadPurpose::Dashboard,
                    },
                    outcome,
                );
            } else if let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() {
                overlay.reads_unsupported = true;
                repaint = true;
            }
        }
        repaint
    }

    /// Store a background `pane.read` result as the pane's last output line.
    pub(super) fn complete_dashboard_read(
        &mut self,
        pane_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let endpoint_key = self.active_endpoint_id.storage_key();
        let endpoint_id = self.active_endpoint_id.clone();
        let Some(ClientShellOverlay::Dashboard(overlay)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if overlay.lines_endpoint != endpoint_id {
            overlay.lines.clear();
            overlay.lines_endpoint = endpoint_id;
        }
        let text = match result {
            Ok(crate::api::schema::ResponseResult::PaneRead { read }) => {
                tail_lines(&read.text, 1).pop().unwrap_or_default()
            }
            // Remember the failure so a gone pane is not retried every tick.
            _ => String::new(),
        };
        overlay.lines.insert(
            (endpoint_key, pane_id),
            LastLine {
                text,
                at: Instant::now(),
            },
        );
        (true, Vec::new())
    }
}

pub(super) struct DashboardRender {
    pub(super) area: Rect,
    pub(super) header: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) offset: usize,
    pub(super) cursor: Option<crate::protocol::CursorState>,
}

struct Columns {
    name: u16,
    location: u16,
    tab: Option<u16>,
    time: Option<u16>,
    last: Option<u16>,
}

/// Pick column widths for `width` cells, dropping the least important columns
/// first on narrow terminals. The marker and state columns always stay.
fn choose_columns(width: u16) -> Columns {
    const MARKER: u16 = 1;
    const STATE: u16 = 8;
    const SPACING: u16 = 1;
    let total =
        |name: u16, location: u16, tab: Option<u16>, time: Option<u16>, last: Option<u16>| {
            let columns = 4 + [tab, time, last].iter().flatten().count() as u16;
            MARKER
                + name
                + location
                + STATE
                + [tab, time, last].iter().flatten().sum::<u16>()
                + SPACING * (columns - 1)
        };
    for (name, location, tab, time, last) in [
        (18, 26, Some(12), Some(7), Some(12)),
        (14, 18, Some(12), Some(7), Some(12)),
        (14, 18, None, Some(7), Some(12)),
        (12, 14, None, Some(7), None),
        (12, 14, None, None, None),
        (8, 8, None, None, None),
    ] {
        if total(name, location, tab, time, last) <= width {
            return Columns {
                name,
                location,
                tab,
                time,
                last,
            };
        }
    }
    Columns {
        name: 6,
        location: 6,
        tab: None,
        time: None,
        last: None,
    }
}

pub(super) fn render_dashboard(
    buffer: &mut Buffer,
    overlay: &ClientDashboardOverlay,
    rows: &[DashboardRow],
    total: usize,
    selected: usize,
    config: &ClientShellConfig,
) -> Option<DashboardRender> {
    let palette = &config.palette;
    let area = buffer.area;
    if area.width < MIN_PANEL_WIDTH || area.height < MIN_PANEL_HEIGHT {
        return None;
    }
    // Full screen: nothing of the panes underneath shows through.
    let outer = area;
    let inner = draw_panel(buffer, outer, "agent dashboard", palette)?;
    if inner.height < 4 || inner.width < 20 {
        return Some(DashboardRender {
            area: outer,
            header: Rect::default(),
            rows: Vec::new(),
            viewport_rows: 1,
            offset: 0,
            cursor: None,
        });
    }

    // Filter prompt on the left, sort and counts on the right.
    let counts = if rows.len() == total {
        format!("{total} agents")
    } else {
        format!("{} of {total} agents", rows.len())
    };
    let right = format!("sort: {} · {counts}", overlay.sort.label());
    let right_width = display_width(&right).min(inner.width.saturating_sub(12));
    put_text(
        buffer,
        inner.right().saturating_sub(right_width + 1),
        inner.y,
        right_width,
        &crate::ui::truncate_end(&right, usize::from(right_width)),
        dim_style(palette),
    );
    let prompt_style = if overlay.filter_focused {
        key_style(palette)
    } else {
        dim_style(palette)
    };
    put_text(buffer, inner.x + 1, inner.y, 1, "/", prompt_style);
    let field = Rect::new(
        inner.x + 3,
        inner.y,
        inner.width.saturating_sub(5 + right_width),
        1,
    );
    let cursor = if overlay.filter_focused || !overlay.filter.is_empty() {
        let cursor = text_editor::render(buffer, field, &overlay.filter, base_style(palette));
        cursor.filter(|_| overlay.filter_focused)
    } else {
        put_text(
            buffer,
            field.x,
            field.y,
            field.width,
            "type / to filter",
            dim_style(palette),
        );
        None
    };

    let table_area = Rect::new(
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(2),
    );
    let columns = choose_columns(table_area.width);
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Length(columns.name),
        Constraint::Length(columns.location),
    ];
    let mut header = vec!["", "agent", "machine · workspace"];
    if let Some(width) = columns.tab {
        widths.push(Constraint::Length(width));
        header.push("tab/pane");
    }
    widths.push(Constraint::Length(8));
    header.push("state");
    if let Some(width) = columns.time {
        widths.push(Constraint::Length(width));
        header.push("time");
    }
    if columns.last.is_some() {
        widths.push(Constraint::Fill(1));
        header.push("last output");
    }

    let fit = |text: &str, width: u16| crate::ui::truncate_end(text, usize::from(width));
    let column_count = widths.len() as u16;
    let fixed: u16 = 1
        + columns.name
        + columns.location
        + columns.tab.unwrap_or(0)
        + 8
        + columns.time.unwrap_or(0);
    let last_width = table_area
        .width
        .saturating_sub(fixed + (column_count - 1))
        .max(1);
    let table_rows = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let is_selected = index == selected;
            let plain = if row.stale {
                dim_style(palette).add_modifier(Modifier::DIM)
            } else {
                base_style(palette)
            };
            let style_for = |color: Option<ratatui::style::Color>| {
                if is_selected {
                    selected_style(palette)
                } else if let Some(color) = color.filter(|_| !row.stale) {
                    Style::default().fg(color).bg(palette.panel_bg)
                } else {
                    plain
                }
            };
            let status_style = style_for(Some(status_color(row.status, palette)));
            let mut cells = vec![
                Cell::from(Span::styled(
                    status_icon(row.status, config.status_indicators),
                    status_style,
                )),
                Cell::from(Span::styled(fit(&row.name, columns.name), style_for(None))),
                Cell::from(Span::styled(
                    fit(&row.workspace, columns.location),
                    if is_selected {
                        selected_style(palette)
                    } else {
                        plain
                    },
                )),
            ];
            if let Some(width) = columns.tab {
                cells.push(Cell::from(Span::styled(
                    fit(&format!("{}/{}", row.tab, row.pane), width),
                    if is_selected {
                        selected_style(palette)
                    } else {
                        dim_style(palette)
                    },
                )));
            }
            cells.push(Cell::from(Span::styled(
                status_text(row.status),
                status_style,
            )));
            if columns.time.is_some() {
                cells.push(Cell::from(Span::styled(
                    format_age(row.age),
                    if is_selected {
                        selected_style(palette)
                    } else {
                        dim_style(palette)
                    },
                )));
            }
            if columns.last.is_some() {
                let text = row.last_line.as_deref().unwrap_or("—");
                let mut style = if is_selected {
                    selected_style(palette)
                } else {
                    plain
                };
                if row.last_line_is_title && !is_selected {
                    style = dim_style(palette).add_modifier(Modifier::ITALIC);
                }
                cells.push(Cell::from(Span::styled(fit(text, last_width), style)));
            }
            let row_style = if is_selected {
                selected_style(palette)
            } else {
                base_style(palette)
            };
            Row::new(cells).style(row_style)
        })
        .collect::<Vec<_>>();
    let table =
        Table::new(table_rows, widths)
            .header(
                Row::new(header.into_iter().map(|title| {
                    Cell::from(Span::styled(title.to_owned(), heading_style(palette)))
                }))
                .style(base_style(palette)),
            )
            .column_spacing(1)
            .style(base_style(palette));
    let mut state = TableState::default()
        .with_offset(overlay.scroll)
        .with_selected((!rows.is_empty()).then_some(selected));
    StatefulWidget::render(table, table_area, buffer, &mut state);
    let offset = state.offset();
    let viewport_rows = usize::from(table_area.height.saturating_sub(1)).max(1);
    let mut hit_rows = Vec::new();
    for (index, _) in rows.iter().enumerate().skip(offset).take(viewport_rows) {
        hit_rows.push((
            Rect::new(
                table_area.x,
                table_area.y + 1 + (index - offset) as u16,
                table_area.width,
                1,
            ),
            index,
        ));
    }
    if rows.is_empty() {
        put_text(
            buffer,
            table_area.x,
            table_area.y + 2,
            table_area.width,
            if total == 0 {
                "No agents yet"
            } else {
                "No agents match the filter"
            },
            dim_style(palette),
        );
    }
    let hint_width = usize::from(inner.width.saturating_sub(2));
    let hint = [
        "↑/↓ j/k move   enter jump   s sort   / filter   r rename   esc close",
        "enter jump   s sort   / filter   r rename   esc close",
        "enter jump   s sort   esc close",
        "esc close",
    ]
    .into_iter()
    .find(|hint| hint.chars().count() <= hint_width)
    .unwrap_or("");
    put_text(
        buffer,
        inner.x + 1,
        inner.bottom() - 1,
        inner.width.saturating_sub(2),
        hint,
        dim_style(palette),
    );
    Some(DashboardRender {
        area: outer,
        header: Rect::new(table_area.x, table_area.y, table_area.width, 1),
        rows: hit_rows,
        viewport_rows,
        offset,
        cursor,
    })
}

impl ClientShellState {
    pub(super) fn render_dashboard_overlay(&self, buffer: &mut Buffer) -> Option<DashboardRender> {
        let (overlay, rows) = self.dashboard_view()?;
        let total = self
            .endpoints
            .iter()
            .filter_map(|endpoint| endpoint.snapshot.as_deref())
            .map(|snapshot| snapshot.agents.len())
            .sum();
        let selected = Self::selected_dashboard_index(&rows, overlay);
        render_dashboard(buffer, overlay, &rows, total, selected, &self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{frame_rows, snapshot, surface};
    use super::*;
    use crate::api::schema::{AgentStatus, ResponseResult};
    use crate::client::endpoint::{ClientEndpointStatus, ProfileId, SavedSshEndpoint};
    use crate::config::Config;
    use crate::input::TerminalKey;
    use crate::protocol::ClientShellAgent;
    use crate::raw_input::RawInputEvent;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

    const BOOT: &str = "boot-1";

    fn agent(
        pane_id: &str,
        workspace_id: &str,
        name: &str,
        status: AgentStatus,
        title: Option<&str>,
    ) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: workspace_id.into(),
            tab_id: "tab_1".into(),
            name: Some(name.into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: title.map(str::to_owned),
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    /// Local: reviewer (working, visible pane), worker (blocked) and docs
    /// (idle, second workspace).
    fn local_snapshot() -> crate::protocol::ClientShellSnapshot {
        let mut projection = snapshot();
        let mut second = projection.workspaces[0].clone();
        second.workspace_id = "ws_2".into();
        second.label = "docs-site".into();
        second.focused = false;
        projection.workspaces.push(second);
        for pane_id in ["pane_2", "pane_3"] {
            let mut pane = projection.panes[0].clone();
            pane.pane_id = pane_id.into();
            pane.focused = false;
            projection.panes.push(pane);
        }
        projection.panes[2].workspace_id = "ws_2".into();
        projection.agents = vec![
            agent("pane_1", "ws_1", "reviewer", AgentStatus::Working, None),
            agent(
                "pane_2",
                "ws_1",
                "worker",
                AgentStatus::Blocked,
                Some("needs approval"),
            ),
            agent("pane_3", "ws_2", "docs", AgentStatus::Idle, None),
        ];
        projection
    }

    fn dashboard_state() -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(local_snapshot()));
        state.set_pane_surface(surface());
        state.compose(140, 30).expect("frame");
        state
    }

    fn state_with_remote() -> (ClientShellState, ClientEndpointId) {
        let mut state = dashboard_state();
        let profile = SavedSshEndpoint {
            id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            label: "Build".into(),
            target: "dev@build.example".into(),
            session: "agents".into(),
            enabled: true,
        };
        let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
        state.set_endpoint_catalog(&[profile]);
        state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
        let mut remote = snapshot();
        remote.boot_id = "remote-boot".into();
        remote.workspaces[0].label = "ci".into();
        remote.agents = vec![
            agent(
                "pane_1",
                "ws_1",
                "builder",
                AgentStatus::Blocked,
                Some("waiting on build"),
            ),
            agent("pane_9", "ws_1", "tester", AgentStatus::Idle, None),
        ];
        state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
        (state, endpoint_id)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> RawInputEvent {
        RawInputEvent::Key(TerminalKey::new(code, modifiers))
    }

    fn press(state: &mut ClientShellState, character: char) -> ClientShellInput {
        state.handle_raw_events(vec![key(KeyCode::Char(character), KeyModifiers::empty())])
    }

    fn open(state: &mut ClientShellState) {
        state.handle_raw_events(vec![
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            key(KeyCode::Char('d'), KeyModifiers::empty()),
        ]);
        assert!(
            matches!(state.overlay, Some(ClientShellOverlay::Dashboard(_))),
            "prefix+d opens the dashboard"
        );
    }

    fn names(state: &ClientShellState) -> Vec<String> {
        let (overlay, rows) = state.dashboard_view().expect("dashboard open");
        let _ = overlay;
        rows.into_iter().map(|row| row.name).collect()
    }

    fn screen(state: &mut ClientShellState, cols: u16, rows: u16) -> String {
        frame_rows(&state.compose(cols, rows).expect("frame").frame).join("\n")
    }

    fn selected_name(state: &ClientShellState) -> String {
        let (overlay, rows) = state.dashboard_view().expect("dashboard open");
        rows[ClientShellState::selected_dashboard_index(&rows, overlay)]
            .name
            .clone()
    }

    fn mouse_down(state: &mut ClientShellState, column: u16, row: u16) -> ClientShellInput {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })])
    }

    fn row(name: &str, status: AgentStatus, seconds: u64, order: usize) -> DashboardRow {
        DashboardRow {
            target: AgentTarget {
                endpoint_id: ClientEndpointId::Local,
                pane_id: format!("p{order}"),
            },
            machine: "Local".into(),
            workspace: "ws".into(),
            tab: "1".into(),
            pane: format!("p{order}"),
            name: name.into(),
            status,
            age: Some(StateAge {
                elapsed: Duration::from_secs(seconds),
                witnessed: true,
            }),
            last_line: None,
            last_line_is_title: false,
            stale: false,
            order: (0, order, 0),
        }
    }

    fn sorted(sort: DashboardSort) -> Vec<String> {
        let mut rows = vec![
            row("delta", AgentStatus::Working, 5, 0),
            row("alpha", AgentStatus::Idle, 30, 1),
            row("Charlie", AgentStatus::Blocked, 10, 2),
            row("bravo", AgentStatus::Blocked, 90, 3),
            row("echo", AgentStatus::Done, 300, 4),
            row("foxtrot", AgentStatus::Idle, 600, 5),
        ];
        sort_rows(&mut rows, sort);
        rows.into_iter().map(|row| row.name).collect()
    }

    #[test]
    fn sort_modes_order_rows() {
        // Blocked first (longest blocked on top), then done, working, idle.
        assert_eq!(
            sorted(DashboardSort::Blocked),
            ["bravo", "Charlie", "echo", "delta", "foxtrot", "alpha"]
        );
        // Idle and done agents by how long they have sat there, then the rest.
        assert_eq!(
            sorted(DashboardSort::Idle),
            ["foxtrot", "echo", "alpha", "bravo", "Charlie", "delta"]
        );
        assert_eq!(
            sorted(DashboardSort::Workspace),
            ["delta", "alpha", "Charlie", "bravo", "echo", "foxtrot"]
        );
        assert_eq!(
            sorted(DashboardSort::Name),
            ["alpha", "bravo", "Charlie", "delta", "echo", "foxtrot"]
        );
    }

    #[test]
    fn sort_cycle_visits_every_mode_and_returns() {
        let mut sort = DashboardSort::default();
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(sort.label());
            sort = sort.next();
        }
        assert_eq!(seen, ["blocked first", "longest idle", "workspace", "name"]);
        assert_eq!(sort, DashboardSort::default());
    }

    #[test]
    fn filter_matches_every_word_across_the_row_text() {
        let mut agent_row = row("reviewer", AgentStatus::Blocked, 1, 0);
        agent_row.workspace = "Build · ci".into();
        agent_row.last_line = Some("Allow this command?".into());
        for (filter, matches) in [
            ("", true),
            ("review", true),
            ("BLOCKED ci", true),
            ("allow command", true),
            ("review idle", false),
            ("nothing", false),
        ] {
            assert_eq!(row_matches(&agent_row, filter), matches, "{filter:?}");
        }
    }

    #[test]
    fn prefix_d_opens_and_esc_closes_and_the_sort_is_remembered() {
        let mut state = dashboard_state();
        open(&mut state);
        press(&mut state, 's');
        press(&mut state, 's');
        let outcome = state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
        assert!(outcome.repaint && outcome.requests.is_empty() && outcome.actions.is_empty());
        assert_eq!(state.dashboard_sort, DashboardSort::Workspace);
        open(&mut state);
        let Some(ClientShellOverlay::Dashboard(overlay)) = state.overlay.as_ref() else {
            panic!("dashboard closed");
        };
        assert_eq!(overlay.sort, DashboardSort::Workspace);
    }

    #[test]
    fn default_binding_is_free_and_resolves() {
        let config = Config::default();
        assert!(config.collect_diagnostics().is_empty());
        assert!(matches!(
            crate::input::resolve_prefix_binding(
                &config.keybinds(),
                &TerminalKey::new(KeyCode::Char('d'), KeyModifiers::empty())
            ),
            Some(crate::input::KeybindMatch::Action(
                crate::input::KeybindAction::AgentDashboard
            ))
        ));
    }

    #[test]
    fn lists_agents_from_every_machine_sorted_blocked_first() {
        let (mut state, _) = state_with_remote();
        open(&mut state);
        // Blocked agents lead; the rest follow by state.
        let names = names(&state);
        assert_eq!(names.len(), 5);
        assert_eq!(
            names[..2]
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            2
        );
        assert!(names[..2].contains(&"worker".to_owned()));
        assert!(names[..2].contains(&"builder".to_owned()));
        assert_eq!(names[2], "reviewer");
        let text = screen(&mut state, 140, 30);
        for expected in [
            "agent dashboard",
            "machine · workspace",
            "tab/pane",
            "last output",
            "sort: blocked first · 5 agents",
            "Build · ci",
            "Local · client-shell",
            "Local · docs-site",
            "reviewer",
            "tester",
        ] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
    }

    #[test]
    fn s_cycles_the_sort_and_the_rows_follow() {
        let mut state = dashboard_state();
        open(&mut state);
        assert_eq!(names(&state), ["worker", "reviewer", "docs"]);
        press(&mut state, 's');
        assert!(screen(&mut state, 140, 30).contains("sort: longest idle"));
        assert_eq!(names(&state), ["docs", "worker", "reviewer"]);
        press(&mut state, 's');
        assert!(screen(&mut state, 140, 30).contains("sort: workspace"));
        assert_eq!(names(&state), ["reviewer", "worker", "docs"]);
        press(&mut state, 's');
        assert!(screen(&mut state, 140, 30).contains("sort: name"));
        assert_eq!(names(&state), ["docs", "reviewer", "worker"]);
        press(&mut state, 's');
        assert_eq!(names(&state), ["worker", "reviewer", "docs"]);
    }

    #[test]
    fn slash_filters_and_esc_leaves_the_filter_before_closing() {
        let mut state = dashboard_state();
        open(&mut state);
        press(&mut state, '/');
        for character in "rev".chars() {
            press(&mut state, character);
        }
        assert_eq!(names(&state), ["reviewer"]);
        let text = screen(&mut state, 140, 30);
        assert!(text.contains("/ rev"), "{text}");
        assert!(text.contains("1 of 3 agents"), "{text}");
        // Typed letters went into the filter, not into the sort or rename keys.
        let Some(ClientShellOverlay::Dashboard(overlay)) = state.overlay.as_ref() else {
            panic!("dashboard closed");
        };
        assert_eq!(overlay.sort, DashboardSort::Blocked);

        state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Dashboard(_))
        ));
        assert_eq!(names(&state), ["reviewer"], "the filter stays applied");
        // `s` is a command again.
        press(&mut state, 's');
        let Some(ClientShellOverlay::Dashboard(overlay)) = state.overlay.as_ref() else {
            panic!("dashboard closed");
        };
        assert_eq!(overlay.sort, DashboardSort::Idle);
        state.handle_raw_events(vec![key(KeyCode::Char('u'), KeyModifiers::CONTROL)]);
        assert_eq!(names(&state).len(), 3);
        state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn filter_matches_state_words_and_pasted_text() {
        let mut state = dashboard_state();
        open(&mut state);
        press(&mut state, '/');
        state.handle_raw_events(vec![RawInputEvent::Paste("blocked".to_owned())]);
        assert_eq!(names(&state), ["worker"]);
        let text = screen(&mut state, 140, 30);
        assert!(text.contains("1 of 3 agents"), "{text}");
        state.handle_raw_events(vec![RawInputEvent::Paste(" nothing".to_owned())]);
        assert!(names(&state).is_empty());
        assert!(screen(&mut state, 140, 30).contains("No agents match the filter"));
    }

    #[test]
    fn navigation_moves_the_selection_by_identity_and_clamps() {
        let mut state = dashboard_state();
        open(&mut state);
        assert_eq!(selected_name(&state), "worker");
        press(&mut state, 'j');
        assert_eq!(selected_name(&state), "reviewer");
        state.handle_raw_events(vec![key(KeyCode::Down, KeyModifiers::empty())]);
        state.handle_raw_events(vec![key(KeyCode::Down, KeyModifiers::empty())]);
        assert_eq!(selected_name(&state), "docs");
        state.handle_raw_events(vec![key(KeyCode::Char('p'), KeyModifiers::CONTROL)]);
        assert_eq!(selected_name(&state), "reviewer");
        press(&mut state, 'G');
        assert_eq!(selected_name(&state), "docs");
        press(&mut state, 'g');
        assert_eq!(selected_name(&state), "worker");

        // Re-sorting keeps the same agent selected.
        press(&mut state, 'j');
        assert_eq!(selected_name(&state), "reviewer");
        press(&mut state, 's');
        assert_eq!(selected_name(&state), "reviewer");
        // A selected agent that disappears falls back to the first row.
        let mut projection = local_snapshot();
        projection.revision = 2;
        projection
            .agents
            .retain(|agent| agent.name.as_deref() != Some("reviewer"));
        state.set_snapshot(Box::new(projection));
        assert_eq!(selected_name(&state), names(&state)[0]);
    }

    #[test]
    fn enter_jumps_to_the_agents_pane_and_closes() {
        let mut state = dashboard_state();
        open(&mut state);
        press(&mut state, 'j');
        assert_eq!(selected_name(&state), "reviewer");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(&request.method, crate::api::schema::Method::PaneFocus(target)
                        if target.pane_id == "pane_1")
            )),
            "{:?}",
            outcome.actions
        );
    }

    #[test]
    fn enter_activates_the_other_machine_for_its_agents() {
        let (mut state, remote) = state_with_remote();
        open(&mut state);
        press(&mut state, '/');
        for character in "builder".chars() {
            press(&mut state, character);
        }
        assert_eq!(names(&state), ["builder"]);
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::ActivateEndpoint {
                    endpoint_id,
                    target: Some(ClientEndpointFocusTarget::Pane(pane)),
                } if *endpoint_id == remote && pane == "pane_1"
            )),
            "{:?}",
            outcome.actions
        );
    }

    #[test]
    fn r_renames_the_selected_agent_on_the_active_machine() {
        let mut state = dashboard_state();
        open(&mut state);
        press(&mut state, 'j');
        press(&mut state, 'r');
        let Some(ClientShellOverlay::Rename(rename)) = state.overlay.as_ref() else {
            panic!("rename prompt expected");
        };
        assert!(matches!(
            &rename.target,
            ClientRenameTarget::Agent { pane_id } if pane_id == "pane_1"
        ));
        assert_eq!(rename.title, "rename agent");
    }

    #[test]
    fn r_on_another_machine_explains_instead_of_renaming() {
        let (mut state, _) = state_with_remote();
        open(&mut state);
        press(&mut state, '/');
        for character in "tester".chars() {
            press(&mut state, character);
        }
        state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        press(&mut state, 'r');
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Dashboard(_))
        ));
        assert_eq!(
            state
                .visible_endpoint_notice
                .as_ref()
                .map(|notice| notice.title.as_str()),
            Some("Select the machine first")
        );
    }

    #[test]
    fn mouse_click_jumps_header_sorts_wheel_scrolls_and_small_screens_close() {
        let mut state = dashboard_state();
        open(&mut state);
        screen(&mut state, 140, 30);
        assert_eq!(state.hits.dashboard.rows.len(), 3);

        let wheel = |state: &mut ClientShellState, kind| {
            state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind,
                column: 40,
                row: 10,
                modifiers: KeyModifiers::empty(),
            })]);
        };
        wheel(&mut state, MouseEventKind::ScrollDown);
        assert_eq!(selected_name(&state), "docs");
        wheel(&mut state, MouseEventKind::ScrollUp);
        assert_eq!(selected_name(&state), "worker");

        let header = state.hits.dashboard.header;
        mouse_down(&mut state, header.x + 3, header.y);
        let Some(ClientShellOverlay::Dashboard(overlay)) = state.overlay.as_ref() else {
            panic!("dashboard closed");
        };
        assert_eq!(overlay.sort, DashboardSort::Idle);
        screen(&mut state, 140, 30);

        let (rect, index) = state.hits.dashboard.rows[1];
        assert_eq!(index, 1);
        let outcome = mouse_down(&mut state, rect.x + 4, rect.y);
        assert!(state.overlay.is_none());
        assert!(outcome
            .actions
            .iter()
            .any(|action| matches!(action, ClientShellAction::Endpoint { .. })));

        // The dashboard fills the screen, so there is no outside to click; when
        // it is too small to draw, any click closes it.
        open(&mut state);
        screen(&mut state, 20, 5);
        let outcome = mouse_down(&mut state, 3, 3);
        assert!(state.overlay.is_none());
        assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
    }

    #[test]
    fn last_output_comes_from_the_surface_title_fallback_or_a_background_read() {
        let mut state = dashboard_state();
        open(&mut state);
        let (overlay, rows) = state.dashboard_view().expect("open");
        let _ = overlay;
        let find = |name: &str| rows.iter().find(|row| row.name == name).unwrap().clone();
        // Visible pane: straight from the frame ("LIVE" / "PANE").
        assert_eq!(find("reviewer").last_line.as_deref(), Some("PANE"));
        assert!(!find("reviewer").last_line_is_title);
        // Hidden pane with a reported title: the title until a read arrives.
        assert_eq!(find("worker").last_line.as_deref(), Some("needs approval"));
        assert!(find("worker").last_line_is_title);
        assert_eq!(find("docs").last_line, None);

        let mut outcome = ClientShellInput::default();
        let now = Instant::now();
        assert!(state.tick_agent_dashboard(now, &mut outcome));
        let reads = outcome
            .actions
            .iter()
            .filter_map(|action| match action {
                ClientShellAction::Endpoint { request, .. } => match &request.method {
                    crate::api::schema::Method::PaneRead(params) => {
                        Some((request.id.clone(), params.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1, "one read at a time");
        let (request_id, params) = &reads[0];
        assert_eq!(params.source, crate::api::schema::ReadSource::Visible);
        assert_eq!(params.lines, Some(READ_WINDOW_LINES));
        assert_ne!(params.pane_id, "pane_1", "visible panes need no read");

        // No second request while one is in flight.
        let mut again = ClientShellInput::default();
        state.tick_agent_dashboard(now + Duration::from_millis(400), &mut again);
        assert!(again.actions.is_empty());

        let pane = params.pane_id.clone();
        let (repaint, _) = state.handle_endpoint_result(
            BOOT,
            request_id,
            Ok(ResponseResult::PaneRead {
                read: crate::api::schema::PaneReadResult {
                    pane_id: pane.clone(),
                    workspace_id: "ws_1".into(),
                    tab_id: "tab_1".into(),
                    source: crate::api::schema::ReadSource::Visible,
                    format: crate::api::schema::ReadFormat::Text,
                    text: "building...\nall tests passed\n\n".into(),
                    revision: 0,
                    truncated: false,
                },
            }),
        );
        assert!(repaint);
        let (overlay, rows) = state.dashboard_view().expect("open");
        let _ = overlay;
        let read_row = rows.iter().find(|row| row.pane == pane).unwrap();
        assert_eq!(read_row.last_line.as_deref(), Some("all tests passed"));
        assert!(!read_row.last_line_is_title);
        assert!(screen(&mut state, 140, 30).contains("all tests passed"));

        // The next poll goes to a different pane, and the fresh one waits.
        let mut next = ClientShellInput::default();
        state.tick_agent_dashboard(now + Duration::from_millis(800), &mut next);
        let next_pane = next.actions.iter().find_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::PaneRead(params) => Some(params.pane_id.clone()),
                _ => None,
            },
            _ => None,
        });
        assert!(next_pane.is_some_and(|next_pane| next_pane != pane));
    }

    #[test]
    fn servers_without_pane_read_fall_back_to_titles_without_requests() {
        let mut state = dashboard_state();
        state.endpoints[0].methods = Some(HashSet::from(["pane.focus".to_owned()]));
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_agent_dashboard(Instant::now(), &mut outcome);
        assert!(outcome.actions.is_empty());
        assert!(state.visible_endpoint_notice.is_none());
        assert!(screen(&mut state, 140, 30).contains("needs approval"));
    }

    #[test]
    fn rows_on_other_machines_never_trigger_reads_and_use_titles() {
        let (mut state, _) = state_with_remote();
        state.endpoints[0].methods = Some(HashSet::from(["pane.focus".to_owned()]));
        open(&mut state);
        let mut outcome = ClientShellInput::default();
        state.tick_agent_dashboard(Instant::now(), &mut outcome);
        assert!(outcome.actions.is_empty());
        assert!(screen(&mut state, 140, 30).contains("waiting on build"));
    }

    #[test]
    fn layout_drops_columns_on_narrow_terminals_and_fits_small_ones() {
        let wide = choose_columns(150);
        assert!(wide.tab.is_some() && wide.time.is_some() && wide.last.is_some());
        let medium = choose_columns(70);
        assert!(medium.tab.is_none() && medium.time.is_some());
        let small = choose_columns(30);
        assert!(small.tab.is_none() && small.time.is_none() && small.last.is_none());

        let mut state = dashboard_state();
        open(&mut state);
        for (cols, rows) in [(180, 50), (100, 24), (60, 14), (34, 8)] {
            let text = screen(&mut state, cols, rows);
            assert!(text.contains("agent dashboard"), "{cols}x{rows}\n{text}");
            assert!(text.contains("worker"), "{cols}x{rows}\n{text}");
            assert!(text.contains("blocked"), "{cols}x{rows}\n{text}");
            let popup = state.hits.dashboard.popup;
            assert!(popup.right() <= cols && popup.bottom() <= rows, "{popup:?}");
        }
        let text = screen(&mut state, 60, 14);
        assert!(!text.contains("last output"), "{text}");
        // Too small to draw: keys still work.
        screen(&mut state, 20, 5);
        press(&mut state, 'j');
        assert_eq!(selected_name(&state), "reviewer");
    }

    #[test]
    fn selected_row_uses_the_selection_style_and_states_use_status_colors() {
        let mut state = dashboard_state();
        open(&mut state);
        let frame = state.compose(140, 30).expect("frame").frame;
        let buffer = frame.to_ratatui_buffer().unwrap();
        let palette = &state.config.palette;
        let (selected, _) = state.hits.dashboard.rows[0];
        assert_eq!(buffer[(selected.x + 4, selected.y)].bg, palette.accent);
        let (other, _) = state.hits.dashboard.rows[1];
        assert_ne!(buffer[(other.x + 4, other.y)].bg, palette.accent);
        // The marker of an unselected working agent carries the working color.
        assert_eq!(
            buffer[(other.x, other.y)].fg,
            status_color(AgentStatus::Working, palette)
        );
    }

    #[test]
    fn empty_dashboard_says_so_and_the_global_menu_and_palette_open_it() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        open(&mut state);
        assert!(screen(&mut state, 100, 24).contains("No agents yet"));

        let (items, _) = {
            let snapshot = state.snapshot.as_deref().unwrap();
            (super::super::global_menu::global_menu_items(snapshot), ())
        };
        let position = items
            .iter()
            .position(|(label, _)| *label == "dashboard")
            .expect("menu entry");
        assert_eq!(items.last().map(|(label, _)| *label), Some("detach"));
        state.overlay = None;
        state.overlay = Some(ClientShellOverlay::GlobalMenu(ClientGlobalMenuOverlay {
            highlighted: position,
        }));
        state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Dashboard(_))
        ));

        state.overlay = None;
        let palette_item = state
            .palette_items()
            .into_iter()
            .find(|item| item.title == "agent dashboard")
            .expect("palette entry");
        assert_eq!(palette_item.detail, "prefix+d");
        let mut outcome = ClientShellInput::default();
        state.run_palette_action(palette_item.action, &mut outcome);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Dashboard(_))
        ));
    }

    #[test]
    fn times_in_state_advance_with_repaints_every_second() {
        let mut state = dashboard_state();
        open(&mut state);
        let start = Instant::now();
        let mut outcome = ClientShellInput::default();
        state.agent_clock.observe(&state.endpoints, start);
        assert!(state.tick_agent_dashboard(start, &mut outcome));
        assert!(!state.tick_agent_dashboard(start + Duration::from_millis(500), &mut outcome));
        assert!(state.tick_agent_dashboard(start + Duration::from_millis(1_000), &mut outcome));
        let (overlay, rows) = state.dashboard_view().expect("open");
        let _ = overlay;
        assert!(rows
            .iter()
            .all(|row| row.age.is_some_and(|age| !age.witnessed)));
        // Closed dashboard: nothing to do.
        state.overlay = None;
        assert!(!state.tick_agent_dashboard(start + Duration::from_secs(5), &mut outcome));
    }
}
