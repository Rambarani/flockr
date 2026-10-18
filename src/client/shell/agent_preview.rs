//! Agent preview popup: the last screen lines of an agent plus its state,
//! workspace and time in state, opened by hovering its sidebar row or with
//! `keys.preview_agent`.
//!
//! Screen text comes from data the client already holds when it can: panes in
//! the visible surface are read straight from the composed frame. Other panes
//! on the active machine are fetched with the existing `pane.read` method over
//! the client command lane, refreshed while the popup is open. Agents on
//! machines that are not active have no command lane of their own, so the
//! popup says so and still shows their cached state.

use std::time::{Duration, Instant};

use crossterm::event::{KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::style::Modifier;

use super::agent_activity::format_age;
use super::popup_frame::{base_style, dim_style, draw_panel, heading_style};
use super::render::{display_width, put_text};
use super::*;

const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const REPAINT_INTERVAL: Duration = Duration::from_secs(1);
/// Screen rows requested per read. The popup keeps the last non-blank ones.
const READ_WINDOW_LINES: u32 = 80;
const MAX_WIDTH: u16 = 100;
const MIN_WIDTH: u16 = 30;
const MIN_HEIGHT: u16 = 7;
/// Border (2), two header rows and a separator.
const CHROME_ROWS: u16 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaneReadPurpose {
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentTarget {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) pane_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewOrigin {
    Hover,
    Key,
}

#[derive(Debug)]
struct HoverCandidate {
    target: AgentTarget,
    since: Instant,
    anchor: Rect,
}

#[derive(Debug)]
pub(super) struct OpenPreview {
    pub(super) target: AgentTarget,
    pub(super) origin: PreviewOrigin,
    anchor: Rect,
    last_request: Option<Instant>,
    last_paint: Instant,
    fetched_lines: Vec<String>,
    notice: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct AgentPreviewState {
    hover: Option<HoverCandidate>,
    pub(super) open: Option<OpenPreview>,
}

#[derive(Default)]
pub(super) struct PreviewHits {
    pub(super) popup: Rect,
}

/// The last `limit` lines of `text` with trailing blank lines dropped and
/// control characters made safe to draw.
pub(super) fn tail_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines = text
        .lines()
        .map(|line| {
            line.chars()
                .map(|ch| match ch {
                    '\t' => ' ',
                    ch if ch.is_control() => '·',
                    ch => ch,
                })
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(limit);
    lines.split_off(start)
}

/// Where the popup goes: right of the sidebar beside the anchor row, or
/// overlaying the pane area when there is no room next to the sidebar.
fn preview_rect(bounds: Rect, sidebar: Rect, anchor: Rect, body_lines: usize) -> Option<Rect> {
    let height = (body_lines as u16)
        .saturating_add(CHROME_ROWS)
        .min(bounds.height.saturating_sub(2));
    if height < MIN_HEIGHT {
        return None;
    }
    let beside = bounds.right().saturating_sub(sidebar.right() + 1);
    let (x, width) = if sidebar.width > 0 && beside >= MIN_WIDTH {
        (sidebar.right(), beside.min(MAX_WIDTH))
    } else {
        let width = bounds.width.saturating_sub(2).min(MAX_WIDTH);
        (
            bounds.x + (bounds.width - width.min(bounds.width)) / 2,
            width,
        )
    };
    if width < MIN_WIDTH {
        return None;
    }
    let wanted_y = if anchor.is_empty() {
        bounds.y + 1
    } else {
        anchor.y.saturating_sub(1)
    };
    let y = wanted_y
        .min(bounds.bottom().saturating_sub(height + 1))
        .max(bounds.y);
    Some(Rect::new(x, y, width, height))
}

enum PreviewBody {
    Lines(Vec<String>),
    Message(String),
}

impl ClientShellState {
    pub(super) fn agent_preview_open(&self) -> bool {
        self.agent_preview.open.is_some()
    }

    fn close_agent_preview(&mut self) -> bool {
        self.agent_preview.hover = None;
        self.agent_preview.open.take().is_some()
    }

    fn agent_exists(&self, target: &AgentTarget) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == target.endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .is_some_and(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == target.pane_id)
            })
    }

    /// The agent row under `point`, with its on-screen rectangle.
    fn agent_row_rect_at(&self, point: (u16, u16)) -> Option<(Rect, AgentTarget)> {
        self.hits
            .endpoint_agents
            .iter()
            .find(|(rect, _, _)| contains(*rect, point))
            .map(|(rect, endpoint_id, pane_id)| (*rect, endpoint_id.clone(), pane_id.clone()))
            .or_else(|| {
                self.hits
                    .agents
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(rect, pane_id)| {
                        (*rect, self.active_endpoint_id.clone(), pane_id.clone())
                    })
            })
            .map(|(rect, endpoint_id, pane_id)| {
                (
                    rect,
                    AgentTarget {
                        endpoint_id,
                        pane_id,
                    },
                )
            })
    }

    fn open_agent_preview(
        &mut self,
        target: AgentTarget,
        origin: PreviewOrigin,
        anchor: Rect,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) {
        self.agent_clock.observe(&self.endpoints, now);
        self.agent_preview.hover = None;
        self.agent_preview.open = Some(OpenPreview {
            target,
            origin,
            anchor,
            last_request: None,
            last_paint: now,
            fetched_lines: Vec::new(),
            notice: None,
        });
        self.refresh_agent_preview(now, outcome);
        outcome.repaint = true;
    }

    /// `keys.preview_agent`: preview the agent in the focused pane.
    pub(super) fn preview_agent_for_focused_pane(&mut self, outcome: &mut ClientShellInput) {
        outcome.repaint = true;
        let target = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_pane_id.clone())
            .map(|pane_id| AgentTarget {
                endpoint_id: self.active_endpoint_id.clone(),
                pane_id,
            })
            .filter(|target| self.agent_exists(target));
        let Some(target) = target else {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "preview_agent_no_agent",
                "No agent to preview",
                "Focus a pane with a detected agent first",
            );
            return;
        };
        if !self.config.agent_preview.enabled {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "preview_agent_disabled",
                "Agent preview is off",
                "Set ui.agent_preview.enabled = true to use it",
            );
            return;
        }
        self.open_agent_preview(
            target,
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            outcome,
        );
    }

    /// Track the pointer for hover previews. Never consumes the event.
    pub(super) fn update_agent_preview_pointer(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) {
        if !self.config.agent_preview.enabled {
            return;
        }
        let hover_origin = self
            .agent_preview
            .open
            .as_ref()
            .is_some_and(|open| open.origin == PreviewOrigin::Hover);
        match mouse.kind {
            MouseEventKind::Moved => {
                if self.overlay.is_some() || self.chrome_drag.is_some() {
                    outcome.repaint |= self.close_agent_preview();
                    return;
                }
                let point = (mouse.column, mouse.row);
                let row = self.agent_row_rect_at(point);
                let over_popup = contains(self.hits.preview.popup, point);
                match row {
                    Some((_, target))
                        if self
                            .agent_preview
                            .open
                            .as_ref()
                            .is_some_and(|open| open.target == target) => {}
                    Some((anchor, target)) => {
                        // A different row: the old hover preview goes away and
                        // the new row starts its own delay.
                        if hover_origin {
                            outcome.repaint |= self.close_agent_preview();
                        }
                        let restart = self
                            .agent_preview
                            .hover
                            .as_ref()
                            .is_none_or(|hover| hover.target != target);
                        if restart && self.agent_preview.open.is_none() {
                            self.agent_preview.hover = Some(HoverCandidate {
                                target,
                                since: Instant::now(),
                                anchor,
                            });
                        }
                    }
                    None => {
                        self.agent_preview.hover = None;
                        if hover_origin && !over_popup {
                            outcome.repaint |= self.close_agent_preview();
                        }
                    }
                }
            }
            MouseEventKind::Down(_)
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                self.agent_preview.hover = None;
                outcome.repaint |= self.close_agent_preview();
            }
            MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {}
        }
    }

    /// Esc closes an open preview and is consumed. Any other key also closes
    /// it but still reaches its normal handler. Returns true when consumed.
    pub(super) fn dismiss_agent_preview_for_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.agent_preview.hover = None;
        if key.kind != KeyEventKind::Press || self.agent_preview.open.is_none() {
            return false;
        }
        self.close_agent_preview();
        outcome.repaint = true;
        key.code == KeyCode::Esc
    }

    fn preview_read_in_flight(&self) -> bool {
        self.pending_requests.values().any(|pending| {
            matches!(
                pending.kind,
                PendingEndpointKind::PaneRead {
                    purpose: PaneReadPurpose::Preview,
                    ..
                }
            )
        })
    }

    /// Fetch fresh screen text for a pane that is not in the visible surface.
    fn refresh_agent_preview(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        let Some(open) = self.agent_preview.open.as_ref() else {
            return;
        };
        let target = open.target.clone();
        let due = open
            .last_request
            .is_none_or(|last| now.saturating_duration_since(last) >= REFRESH_INTERVAL);
        if !due
            || target.endpoint_id != self.active_endpoint_id
            || self.local_surface_lines(&target, 1).is_some()
            || self.preview_read_in_flight()
        {
            return;
        }
        let method = crate::api::schema::Method::PaneRead(crate::api::schema::PaneReadParams {
            pane_id: target.pane_id.clone(),
            source: crate::api::schema::ReadSource::Visible,
            lines: Some(READ_WINDOW_LINES),
            format: crate::api::schema::ReadFormat::Text,
            strip_ansi: true,
            intent: crate::api::schema::ReadIntent::Interactive,
        });
        let notice = if self.supports_endpoint_method(&method) {
            None
        } else {
            Some(
                "Preview unavailable: this machine cannot read other panes yet. Update and restart its flockr server."
                    .to_owned(),
            )
        };
        if let Some(open) = self.agent_preview.open.as_mut() {
            open.last_request = Some(now);
            if notice.is_some() {
                open.notice = notice;
                return;
            }
        }
        self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::PaneRead {
                pane_id: target.pane_id,
                purpose: PaneReadPurpose::Preview,
            },
            outcome,
        );
    }

    /// Open a due hover preview, keep the open one fresh and drop it when its
    /// agent is gone. Returns true when the frame must be repainted.
    pub(crate) fn tick_agent_preview(
        &mut self,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) -> bool {
        self.agent_clock.observe(&self.endpoints, now);
        let mut repaint = false;
        if !self.config.agent_preview.enabled || self.overlay.is_some() {
            repaint |= self.close_agent_preview();
            return repaint;
        }
        let due = self.agent_preview.hover.as_ref().is_some_and(|hover| {
            self.agent_preview.open.is_none()
                && now.saturating_duration_since(hover.since)
                    >= self.config.agent_preview.hover_delay()
        });
        if due {
            if let Some(hover) = self.agent_preview.hover.take() {
                if self.agent_exists(&hover.target) {
                    self.open_agent_preview(
                        hover.target,
                        PreviewOrigin::Hover,
                        hover.anchor,
                        now,
                        outcome,
                    );
                    repaint = true;
                }
            }
        }
        let gone = self
            .agent_preview
            .open
            .as_ref()
            .is_some_and(|open| !self.agent_exists(&open.target));
        if gone {
            return self.close_agent_preview() || repaint;
        }
        if self.agent_preview.open.is_some() {
            self.refresh_agent_preview(now, outcome);
            if let Some(open) = self.agent_preview.open.as_mut() {
                if now.saturating_duration_since(open.last_paint) >= REPAINT_INTERVAL {
                    open.last_paint = now;
                    repaint = true;
                }
            }
        }
        repaint
    }

    pub(super) fn next_agent_preview_deadline(&self) -> Option<Instant> {
        self.agent_preview
            .hover
            .as_ref()
            .filter(|_| self.agent_preview.open.is_none())
            .map(|hover| hover.since + self.config.agent_preview.hover_delay())
    }

    /// Screen rows of a pane in the visible surface, read from the composed
    /// frame. `None` when the pane is not visible or is on another machine.
    pub(super) fn local_surface_lines(
        &self,
        target: &AgentTarget,
        limit: usize,
    ) -> Option<Vec<String>> {
        if target.endpoint_id != self.active_endpoint_id {
            return None;
        }
        let surface = self.pane_surface.as_ref()?;
        let pane = surface
            .panes
            .iter()
            .find(|pane| pane.pane_id == target.pane_id)?;
        let frame = &surface.frame;
        let rect = pane.inner_rect;
        let mut text = String::new();
        for y in rect.y..rect.y.saturating_add(rect.height) {
            for x in rect.x..rect.x.saturating_add(rect.width) {
                let index = usize::from(y) * usize::from(frame.width) + usize::from(x);
                if let Some(cell) = frame.cells.get(index) {
                    text.push_str(&cell.symbol);
                }
            }
            text.push('\n');
        }
        Some(tail_lines(&text, limit))
    }

    pub(super) fn complete_pane_read(
        &mut self,
        pane_id: String,
        purpose: PaneReadPurpose,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match purpose {
            PaneReadPurpose::Preview => self.complete_preview_read(pane_id, result),
        }
    }

    /// Store the result of a background `pane.read` for the preview.
    pub(super) fn complete_preview_read(
        &mut self,
        pane_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let limit = self.config.agent_preview.line_count();
        let Some(open) = self
            .agent_preview
            .open
            .as_mut()
            .filter(|open| open.target.pane_id == pane_id)
        else {
            return (false, Vec::new());
        };
        match result {
            Ok(crate::api::schema::ResponseResult::PaneRead { read }) => {
                open.fetched_lines = tail_lines(&read.text, limit);
                open.notice = None;
            }
            Ok(_) => {
                open.notice = Some("Preview unavailable: unexpected reply from the server".into());
            }
            Err(error) => {
                open.notice = Some(format!("Preview unavailable: {}", error.message));
            }
        }
        (true, Vec::new())
    }

    fn preview_body(&self, open: &OpenPreview) -> PreviewBody {
        let limit = self.config.agent_preview.line_count();
        if open.target.endpoint_id != self.active_endpoint_id {
            return PreviewBody::Message(format!(
                "Preview unavailable: {} is not the active machine. Select it to preview its agents.",
                self.endpoint_label(&open.target.endpoint_id)
            ));
        }
        if let Some(lines) = self.local_surface_lines(&open.target, limit) {
            return PreviewBody::Lines(lines);
        }
        if let Some(notice) = open.notice.as_ref() {
            return PreviewBody::Message(notice.clone());
        }
        if open.fetched_lines.is_empty() && open.last_request.is_some() {
            return PreviewBody::Message("reading screen…".to_owned());
        }
        let start = open.fetched_lines.len().saturating_sub(limit);
        PreviewBody::Lines(open.fetched_lines[start..].to_vec())
    }

    /// Draw the popup. Returns its area, or `None` when nothing is open or
    /// the terminal is too small.
    pub(super) fn render_agent_preview_popup(
        &self,
        buffer: &mut Buffer,
        bounds: Rect,
        sidebar: Rect,
        now: Instant,
    ) -> Option<Rect> {
        let open = self.agent_preview.open.as_ref()?;
        let snapshot = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == open.target.endpoint_id)?
            .snapshot
            .as_deref()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == open.target.pane_id)?;
        let palette = &self.config.palette;
        let rect = preview_rect(
            bounds.intersection(buffer.area),
            sidebar,
            open.anchor,
            self.config.agent_preview.line_count(),
        )?;
        let inner = draw_panel(buffer, rect, "agent preview", palette)?;
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == agent.workspace_id)
            .map_or("workspace", |workspace| workspace.label.as_str());
        let tab = snapshot
            .tabs
            .iter()
            .find(|tab| tab.tab_id == agent.tab_id)
            .map_or("tab", |tab| tab.label.as_str());
        let name = agent
            .name
            .as_deref()
            .or(agent.display_agent.as_deref())
            .or(agent.agent.as_deref())
            .or(agent.title.as_deref())
            .unwrap_or("agent");
        let width = inner.width.saturating_sub(2);
        let x = inner.x + 1;

        let status = agent.agent_status;
        let icon = status_icon(status, self.config.status_indicators);
        let age = format_age(self.agent_clock.age(
            &open.target.endpoint_id,
            &open.target.pane_id,
            now,
        ));
        let age_width = display_width(&age);
        put_text(
            buffer,
            x,
            inner.y,
            1,
            icon,
            Style::default()
                .fg(status_color(status, palette))
                .bg(palette.panel_bg),
        );
        let title = format!("{name} · {}", status_text(status));
        put_text(
            buffer,
            x + 2,
            inner.y,
            width.saturating_sub(2 + age_width + 1),
            &crate::ui::truncate_end(&title, usize::from(width.saturating_sub(2 + age_width + 1))),
            heading_style(palette),
        );
        put_text(
            buffer,
            inner.right().saturating_sub(age_width + 1),
            inner.y,
            age_width,
            &age,
            dim_style(palette),
        );
        let machine = if self.endpoints.len() > 1 {
            format!("{} · ", self.endpoint_label(&open.target.endpoint_id))
        } else {
            String::new()
        };
        let location = format!("{machine}{workspace} › {tab} › {}", open.target.pane_id);
        put_text(
            buffer,
            x,
            inner.y + 1,
            width,
            &crate::ui::truncate_end(&location, usize::from(width)),
            dim_style(palette),
        );
        for column in inner.x..inner.right() {
            put_text(
                buffer,
                column,
                inner.y + 2,
                1,
                "─",
                Style::default().fg(palette.surface1).bg(palette.panel_bg),
            );
        }
        let body = Rect::new(x, inner.y + 3, width, inner.height.saturating_sub(3));
        match self.preview_body(open) {
            PreviewBody::Lines(lines) if lines.is_empty() => {
                put_text(
                    buffer,
                    body.x,
                    body.y,
                    body.width,
                    "(screen is empty)",
                    dim_style(palette),
                );
            }
            PreviewBody::Lines(lines) => {
                let shown = usize::from(body.height);
                let start = lines.len().saturating_sub(shown);
                for (offset, line) in lines[start..].iter().enumerate() {
                    put_text(
                        buffer,
                        body.x,
                        body.y + offset as u16,
                        body.width,
                        line,
                        base_style(palette),
                    );
                }
            }
            PreviewBody::Message(message) => {
                for (row, chunk) in wrap_message(&message, usize::from(body.width))
                    .iter()
                    .take(usize::from(body.height))
                    .enumerate()
                {
                    put_text(
                        buffer,
                        body.x,
                        body.y + row as u16,
                        body.width,
                        chunk,
                        dim_style(palette).add_modifier(Modifier::ITALIC),
                    );
                }
            }
        }
        Some(rect)
    }
}

/// Greedy word wrap for the short unavailable messages.
fn wrap_message(message: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in message.split_whitespace() {
        if !line.is_empty()
            && display_width(&line) as usize + 1 + display_width(word) as usize > width
        {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::popup_frame::buffer_row;
    use super::super::tests::{frame_rows, snapshot, surface};
    use super::*;
    use crate::api::schema::{AgentStatus, ResponseResult};
    use crate::client::endpoint::{ClientEndpointStatus, ProfileId, SavedSshEndpoint};
    use crate::config::Config;
    use crate::input::TerminalKey;
    use crate::protocol::ClientShellAgent;
    use crate::raw_input::RawInputEvent;
    use crossterm::event::{KeyModifiers, MouseButton};

    const BOOT: &str = "boot-1";

    fn agent(pane_id: &str, tab_id: &str, name: &str, status: AgentStatus) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "ws_1".into(),
            tab_id: tab_id.into(),
            name: Some(name.into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    /// `pane_1` (agent "reviewer") is in the visible surface; `pane_2` (agent
    /// "worker") lives in a second tab that is not.
    fn two_agent_snapshot() -> crate::protocol::ClientShellSnapshot {
        let mut projection = snapshot();
        let mut tab = projection.tabs[0].clone();
        tab.tab_id = "tab_2".into();
        tab.number = 2;
        tab.label = "build".into();
        tab.custom_label = true;
        tab.focused = false;
        projection.tabs.push(tab);
        let mut pane = projection.panes[0].clone();
        pane.pane_id = "pane_2".into();
        pane.tab_id = "tab_2".into();
        pane.focused = false;
        projection.panes.push(pane);
        projection
            .agents
            .push(agent("pane_1", "tab_1", "reviewer", AgentStatus::Working));
        projection
            .agents
            .push(agent("pane_2", "tab_2", "worker", AgentStatus::Blocked));
        projection
    }

    fn preview_state() -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(two_agent_snapshot()));
        state.set_pane_surface(surface());
        state.compose(120, 30).expect("frame");
        state
    }

    fn target(pane_id: &str) -> AgentTarget {
        AgentTarget {
            endpoint_id: ClientEndpointId::Local,
            pane_id: pane_id.into(),
        }
    }

    fn row_rect(state: &ClientShellState, pane_id: &str) -> Rect {
        state
            .hits
            .agents
            .iter()
            .find(|(_, id)| id == pane_id)
            .map(|(rect, _)| *rect)
            .unwrap_or_else(|| panic!("{pane_id} row in {:?}", state.hits.agents))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
        RawInputEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        })
    }

    fn hover(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
        state.handle_raw_events(vec![mouse(MouseEventKind::Moved, rect.x + 2, rect.y)])
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> RawInputEvent {
        RawInputEvent::Key(TerminalKey::new(code, modifiers))
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn popup_text(state: &mut ClientShellState) -> String {
        frame_rows(&state.compose(120, 30).expect("frame").frame).join("\n")
    }

    fn read_actions(
        outcome: &ClientShellInput,
    ) -> Vec<(String, crate::api::schema::PaneReadParams)> {
        outcome
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
            .collect()
    }

    fn read_result(pane_id: &str, text: &str) -> ResponseResult {
        ResponseResult::PaneRead {
            read: crate::api::schema::PaneReadResult {
                pane_id: pane_id.into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_2".into(),
                source: crate::api::schema::ReadSource::Visible,
                format: crate::api::schema::ReadFormat::Text,
                text: text.into(),
                revision: 0,
                truncated: false,
            },
        }
    }

    #[test]
    fn hover_opens_after_the_delay_and_mouse_out_closes() {
        let mut state = preview_state();
        let rect = row_rect(&state, "pane_1");
        hover(&mut state, rect);
        let since = state.agent_preview.hover.as_ref().expect("candidate").since;
        assert!(!state.agent_preview_open());
        assert_eq!(
            state.next_agent_preview_deadline(),
            Some(since + ms(600)),
            "the timer wakes for the hover deadline"
        );

        let mut outcome = ClientShellInput::default();
        assert!(!state.tick_agent_preview(since + ms(599), &mut outcome));
        assert!(!state.agent_preview_open());
        assert!(state.tick_agent_preview(since + ms(600), &mut outcome));
        let open = state.agent_preview.open.as_ref().expect("open");
        assert_eq!(open.target, target("pane_1"));
        assert_eq!(open.origin, PreviewOrigin::Hover);
        assert_eq!(state.next_agent_preview_deadline(), None);

        let text = popup_text(&mut state);
        assert!(text.contains("agent preview"), "{text}");
        assert!(text.contains("reviewer · working"), "{text}");
        assert!(text.contains("LIVE"), "{text}");
        assert!(text.contains("PANE"), "{text}");
        assert!(state.hits.preview.popup.width > 0);

        // Staying on the row keeps it open.
        let outcome = hover(&mut state, rect);
        assert!(state.agent_preview_open() && !outcome.repaint);
        // Leaving the row closes it.
        let outcome = state.handle_raw_events(vec![mouse(MouseEventKind::Moved, 90, 3)]);
        assert!(!state.agent_preview_open());
        assert!(outcome.repaint);
        assert!(!popup_text(&mut state).contains("agent preview"));
    }

    #[test]
    fn moving_to_another_row_restarts_the_delay() {
        let mut state = preview_state();
        let first = row_rect(&state, "pane_1");
        let second = row_rect(&state, "pane_2");
        hover(&mut state, first);
        let mut outcome = ClientShellInput::default();
        let since = state.agent_preview.hover.as_ref().unwrap().since;
        state.tick_agent_preview(since + ms(700), &mut outcome);
        assert_eq!(
            state.agent_preview.open.as_ref().unwrap().target,
            target("pane_1")
        );

        hover(&mut state, second);
        assert!(!state.agent_preview_open(), "the old preview goes away");
        let hovered = state.agent_preview.hover.as_ref().expect("candidate");
        assert_eq!(hovered.target, target("pane_2"));
        let mut outcome = ClientShellInput::default();
        assert!(!state.tick_agent_preview(hovered.since + ms(100), &mut outcome));
        assert!(!state.agent_preview_open());
    }

    #[test]
    fn clicking_the_row_closes_the_preview_and_still_focuses_the_agent() {
        let mut state = preview_state();
        let rect = row_rect(&state, "pane_2");
        hover(&mut state, rect);
        let since = state.agent_preview.hover.as_ref().unwrap().since;
        state.tick_agent_preview(since + ms(700), &mut ClientShellInput::default());
        assert!(state.agent_preview_open());

        let outcome = state.handle_raw_events(vec![mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 2,
            rect.y,
        )]);
        assert!(!state.agent_preview_open());
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(&request.method, crate::api::schema::Method::PaneFocus(target)
                        if target.pane_id == "pane_2")
            )),
            "{:?}",
            outcome.actions
        );
    }

    #[test]
    fn hover_does_nothing_when_disabled_or_covered_by_an_overlay() {
        let mut state = preview_state();
        state.config.agent_preview.enabled = false;
        let rect = row_rect(&state, "pane_1");
        hover(&mut state, rect);
        assert!(state.agent_preview.hover.is_none());
        assert_eq!(state.next_agent_preview_deadline(), None);

        let mut state = preview_state();
        let rect = row_rect(&state, "pane_1");
        hover(&mut state, rect);
        let since = state.agent_preview.hover.as_ref().unwrap().since;
        state.tick_agent_preview(since + ms(700), &mut ClientShellInput::default());
        assert!(state.agent_preview_open());
        state.open_command_palette();
        assert!(state.tick_agent_preview(since + ms(800), &mut ClientShellInput::default()));
        assert!(!state.agent_preview_open());
    }

    #[test]
    fn preview_key_opens_the_focused_agent_and_esc_closes_without_reaching_the_pane() {
        let mut state = preview_state();
        let outcome = state.handle_raw_events(vec![
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            key(KeyCode::Char('i'), KeyModifiers::empty()),
        ]);
        assert!(outcome.repaint);
        let open = state.agent_preview.open.as_ref().expect("open");
        assert_eq!(open.target, target("pane_1"));
        assert_eq!(open.origin, PreviewOrigin::Key);
        assert!(popup_text(&mut state).contains("reviewer · working"));

        let outcome = state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(!state.agent_preview_open());
        assert!(outcome.repaint);
        assert!(
            outcome.requests.is_empty() && outcome.actions.is_empty(),
            "esc is consumed: {} requests, {} actions",
            outcome.requests.len(),
            outcome.actions.len()
        );
    }

    #[test]
    fn any_other_key_closes_the_preview_and_is_still_delivered() {
        let mut state = preview_state();
        state.handle_raw_events(vec![
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            key(KeyCode::Char('i'), KeyModifiers::empty()),
        ]);
        assert!(state.agent_preview_open());
        let outcome = state.handle_raw_events(vec![key(KeyCode::Char('x'), KeyModifiers::empty())]);
        assert!(!state.agent_preview_open());
        assert!(
            outcome
                .requests
                .iter()
                .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })),
            "the key reaches the focused pane"
        );
    }

    #[test]
    fn esc_without_a_preview_still_reaches_the_pane() {
        let mut state = preview_state();
        let outcome = state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })));
    }

    #[test]
    fn preview_key_without_an_agent_explains_instead_of_opening() {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state.handle_raw_events(vec![
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            key(KeyCode::Char('i'), KeyModifiers::empty()),
        ]);
        assert!(!state.agent_preview_open());
        assert_eq!(
            state
                .visible_endpoint_notice
                .as_ref()
                .map(|notice| notice.title.as_str()),
            Some("No agent to preview")
        );
    }

    #[test]
    fn visible_panes_are_read_from_the_composed_frame_without_a_request() {
        let mut state = preview_state();
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            target("pane_1"),
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            &mut outcome,
        );
        assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
        assert_eq!(
            state.local_surface_lines(&target("pane_1"), 15),
            Some(vec!["LIVE".to_owned(), "PANE".to_owned()])
        );
        assert_eq!(state.local_surface_lines(&target("pane_2"), 15), None);
    }

    #[test]
    fn surface_text_keeps_the_last_lines_and_drops_trailing_blanks() {
        let mut state = preview_state();
        let rows = (0..30)
            .map(|index| format!("line {index:02}"))
            .chain(["".to_owned(), "".to_owned()])
            .collect::<Vec<_>>();
        let buffer = Buffer::with_lines(rows.iter().map(String::as_str));
        let mut tall = surface();
        tall.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        let width = tall.frame.width;
        tall.panes[0].rect.width = width;
        tall.panes[0].rect.height = 32;
        tall.panes[0].inner_rect.width = width;
        tall.panes[0].inner_rect.height = 32;
        state.pane_surface = Some(tall);
        let lines = state.local_surface_lines(&target("pane_1"), 15).unwrap();
        assert_eq!(lines.len(), 15);
        assert_eq!(lines[0], "line 15");
        assert_eq!(lines[14], "line 29");
        assert_eq!(
            state
                .local_surface_lines(&target("pane_1"), 3)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn hidden_panes_are_fetched_with_pane_read_and_refreshed_while_open() {
        let mut state = preview_state();
        let mut outcome = ClientShellInput::default();
        let now = Instant::now();
        state.open_agent_preview(
            target("pane_2"),
            PreviewOrigin::Key,
            Rect::default(),
            now,
            &mut outcome,
        );
        let reads = read_actions(&outcome);
        assert_eq!(reads.len(), 1, "{:?}", outcome.actions);
        let (request_id, params) = &reads[0];
        assert_eq!(params.pane_id, "pane_2");
        assert_eq!(params.source, crate::api::schema::ReadSource::Visible);
        assert_eq!(params.lines, Some(READ_WINDOW_LINES));
        assert_eq!(params.format, crate::api::schema::ReadFormat::Text);
        assert!(params.strip_ansi);
        assert!(popup_text(&mut state).contains("reading screen"));

        // One read at a time.
        let mut again = ClientShellInput::default();
        state.tick_agent_preview(now + ms(900), &mut again);
        assert!(read_actions(&again).is_empty());

        // The reply fills the popup with the last lines.
        let text = (0..40).map(|n| format!("out {n:02}\n")).collect::<String>() + "\n\n";
        let (repaint, actions) =
            state.handle_endpoint_result(BOOT, request_id, Ok(read_result("pane_2", &text)));
        assert!(repaint && actions.is_empty());
        let shown = popup_text(&mut state);
        assert!(
            shown.contains("out 39") && shown.contains("out 25"),
            "{shown}"
        );
        assert!(!shown.contains("out 24"), "{shown}");
        assert!(shown.contains("worker · blocked"), "{shown}");
        assert!(shown.contains("client-shell › build › pane_2"), "{shown}");

        // Not yet due, then due again.
        let mut early = ClientShellInput::default();
        state.tick_agent_preview(now + ms(700), &mut early);
        assert!(read_actions(&early).is_empty());
        let mut later = ClientShellInput::default();
        state.tick_agent_preview(now + ms(800), &mut later);
        assert_eq!(read_actions(&later).len(), 1);
    }

    #[test]
    fn line_count_follows_the_config() {
        let mut state = preview_state();
        state.config.agent_preview.lines = 3;
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            target("pane_2"),
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            &mut outcome,
        );
        let (request_id, _) = read_actions(&outcome).remove(0);
        let text = (0..10).map(|n| format!("out {n}\n")).collect::<String>();
        state.handle_endpoint_result(BOOT, &request_id, Ok(read_result("pane_2", &text)));
        let shown = popup_text(&mut state);
        assert!(
            shown.contains("out 9") && shown.contains("out 7"),
            "{shown}"
        );
        assert!(!shown.contains("out 6"), "{shown}");
    }

    #[test]
    fn servers_without_pane_read_get_a_clear_message_and_no_request() {
        let mut state = preview_state();
        state.endpoints[0].methods = Some(HashSet::from(["pane.focus".to_owned()]));
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            target("pane_2"),
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            &mut outcome,
        );
        assert!(outcome.actions.is_empty());
        assert!(state.visible_endpoint_notice.is_none(), "no stray toast");
        let shown = popup_text(&mut state);
        assert!(shown.contains("Preview unavailable"), "{shown}");
        assert!(shown.contains("cannot read other panes"), "{shown}");
        // State and location still show.
        assert!(shown.contains("worker · blocked"), "{shown}");
        // Visible panes keep working: they never need the server.
        state.agent_preview.open = None;
        state.open_agent_preview(
            target("pane_1"),
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            &mut ClientShellInput::default(),
        );
        assert!(popup_text(&mut state).contains("LIVE"));
    }

    #[test]
    fn failed_reads_show_the_reason_quietly() {
        let mut state = preview_state();
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            target("pane_2"),
            PreviewOrigin::Key,
            Rect::default(),
            Instant::now(),
            &mut outcome,
        );
        let (request_id, _) = read_actions(&outcome).remove(0);
        let (repaint, _) = state.handle_endpoint_result(
            BOOT,
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("pane_not_found".into()),
                message: "pane is gone".into(),
            }),
        );
        assert!(repaint);
        assert!(state.visible_endpoint_notice.is_none());
        assert!(popup_text(&mut state).contains("Preview unavailable: pane is gone"));
    }

    #[test]
    fn agents_on_other_machines_say_why_there_is_no_screen() {
        let mut state = preview_state();
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
        let mut remote = two_agent_snapshot();
        remote.boot_id = "remote-boot".into();
        remote.workspaces[0].label = "remote-space".into();
        state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            AgentTarget {
                endpoint_id,
                pane_id: "pane_2".into(),
            },
            PreviewOrigin::Hover,
            Rect::default(),
            Instant::now(),
            &mut outcome,
        );
        assert!(
            outcome.actions.is_empty(),
            "no command lane to that machine"
        );
        let shown = popup_text(&mut state);
        assert!(shown.contains("Build is not the active machine"), "{shown}");
        assert!(shown.contains("worker · blocked"), "{shown}");
        assert!(
            shown.contains("Build · remote-space › build › pane_2"),
            "{shown}"
        );
    }

    #[test]
    fn time_in_state_counts_from_the_observed_change() {
        let mut state = preview_state();
        let start = Instant::now();
        let mut outcome = ClientShellInput::default();
        state.tick_agent_preview(start, &mut outcome);
        state.open_agent_preview(
            target("pane_1"),
            PreviewOrigin::Key,
            Rect::default(),
            start,
            &mut outcome,
        );
        let area = Rect::new(0, 0, 120, 30);
        let mut buffer = Buffer::empty(area);
        state
            .render_agent_preview_popup(
                &mut buffer,
                area,
                Rect::new(0, 0, 26, 30),
                start + Duration::from_secs(125),
            )
            .expect("popup");
        let text = (0..30)
            .map(|y| buffer_row(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("≥2m 05s"), "{text}");

        // A witnessed change starts a fresh clock.
        let mut projection = two_agent_snapshot();
        projection.revision = 2;
        projection.agents[0].agent_status = AgentStatus::Idle;
        projection.agents[0].state_change_seq = 2;
        state.set_snapshot(Box::new(projection));
        state.tick_agent_preview(start + Duration::from_secs(200), &mut outcome);
        let area = Rect::new(0, 0, 120, 30);
        let mut buffer = Buffer::empty(area);
        state
            .render_agent_preview_popup(
                &mut buffer,
                area,
                Rect::new(0, 0, 26, 30),
                start + Duration::from_secs(207),
            )
            .expect("popup");
        let text = (0..30)
            .map(|y| buffer_row(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        // Working to idle shows as done until the agent is seen, as in the sidebar.
        assert!(text.contains("reviewer · done"), "{text}");
        assert!(text.contains(" 7s"), "{text}");
        assert!(!text.contains("≥7s"), "{text}");
    }

    #[test]
    fn the_popup_repaints_each_second_and_closes_when_the_agent_exits() {
        let mut state = preview_state();
        let start = Instant::now();
        let mut outcome = ClientShellInput::default();
        state.open_agent_preview(
            target("pane_1"),
            PreviewOrigin::Key,
            Rect::default(),
            start,
            &mut outcome,
        );
        assert!(!state.tick_agent_preview(start + ms(500), &mut outcome));
        assert!(state.tick_agent_preview(start + ms(1_000), &mut outcome));
        assert!(!state.tick_agent_preview(start + ms(1_500), &mut outcome));

        let mut projection = two_agent_snapshot();
        projection.revision = 2;
        projection.agents.remove(0);
        state.set_snapshot(Box::new(projection));
        assert!(state.tick_agent_preview(start + ms(1_600), &mut outcome));
        assert!(!state.agent_preview_open());
    }

    #[test]
    fn popup_placement_fits_large_and_small_terminals() {
        let bounds = Rect::new(0, 0, 160, 50);
        let sidebar = Rect::new(0, 0, 26, 50);
        let anchor = Rect::new(0, 10, 25, 1);
        let rect = preview_rect(bounds, sidebar, anchor, 15).unwrap();
        assert_eq!((rect.x, rect.y, rect.height), (26, 9, 20));
        assert_eq!(rect.width, MAX_WIDTH);

        // Near the bottom it slides up instead of running off the screen.
        let low = preview_rect(bounds, sidebar, Rect::new(0, 48, 25, 1), 15).unwrap();
        assert!(low.bottom() < bounds.bottom());

        // Without room beside the sidebar it overlays the pane area.
        let narrow =
            preview_rect(Rect::new(0, 0, 50, 24), Rect::new(0, 0, 26, 24), anchor, 15).unwrap();
        assert!(narrow.right() <= 50 && narrow.width >= MIN_WIDTH);

        // Short terminals shrink the body; tiny ones skip the popup.
        let short = preview_rect(Rect::new(0, 0, 120, 12), sidebar, anchor, 15).unwrap();
        assert_eq!(short.height, 10);
        assert_eq!(
            preview_rect(Rect::new(0, 0, 120, 8), sidebar, anchor, 15),
            None
        );
        assert_eq!(
            preview_rect(
                Rect::new(0, 0, 28, 30),
                Rect::default(),
                Rect::default(),
                15
            ),
            None
        );

        // No sidebar and no anchor (key-opened): centered near the top.
        let keyed = preview_rect(bounds, Rect::default(), Rect::default(), 15).unwrap();
        assert_eq!(keyed.y, 1);
        assert_eq!(keyed.x, (160 - keyed.width) / 2);
    }

    #[test]
    fn tail_lines_makes_text_safe_to_draw() {
        assert_eq!(tail_lines("a\nb\tc\u{1b}d\n\n  \n", 5), ["a", "b c·d"]);
        assert_eq!(tail_lines("1\n2\n3\n4\n", 2), ["3", "4"]);
        assert!(tail_lines("\n\n", 3).is_empty());
    }
}
