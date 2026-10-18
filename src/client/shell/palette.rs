//! Command palette: a fuzzy-searchable popup over keybinding actions, custom
//! commands, workspaces, tabs, panes, agents on every machine and settings.
//!
//! Everything shown is built from data the client already holds (the live
//! keybinding table and each endpoint's cached snapshot). Running an entry goes
//! through the same code paths as its keybinding or sidebar click.

use crossterm::event::MouseEvent;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use ratatui::style::Modifier;

use super::action_catalog::action_specs;
use super::popup_frame::{base_style, dim_style, draw_panel, key_style, selected_style};
use super::render::{display_width, put_text};
use super::*;

const MAX_WIDTH: u16 = 96;
const MAX_HEIGHT: u16 = 24;
const MIN_WIDTH: u16 = 30;
const MIN_HEIGHT: u16 = 8;
const KIND_WIDTH: u16 = 11;
const MAX_RECENT: usize = 12;
/// Added to matches that sit entirely inside the title so they outrank
/// matches that needed the kind or detail text.
const TITLE_MATCH_BONUS: u32 = 1000;

#[derive(Debug, Clone)]
pub(super) enum PaletteAction {
    Binding(crate::input::KeybindAction),
    Command(crate::config::CustomCommandKeybind),
    Focus {
        endpoint_id: ClientEndpointId,
        target: ClientEndpointFocusTarget,
    },
    Settings(ClientSettingsSection),
}

#[derive(Debug, Clone)]
pub(super) struct PaletteItem {
    /// Stable across redraws, used for the recently-used ordering.
    pub(super) id: String,
    pub(super) kind: &'static str,
    pub(super) title: String,
    /// Right-aligned secondary text: the key for actions, the machine and
    /// state for navigation targets.
    pub(super) detail: String,
    pub(super) action: PaletteAction,
}

#[derive(Debug)]
pub(super) struct ClientPaletteOverlay {
    pub(super) query: TextEditor,
    pub(super) selected: usize,
    pub(super) scroll: usize,
}

#[derive(Default)]
pub(super) struct PaletteHits {
    pub(super) popup: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RankedItem {
    pub(super) index: usize,
    /// Char offsets into the title that the query matched, ascending.
    pub(super) title_matches: Vec<u32>,
}

/// Filter and order `items` for `query`. An empty query keeps catalog order
/// but floats the recently used entries to the top, most recent first.
pub(super) fn rank_items(items: &[PaletteItem], query: &str, recent: &[String]) -> Vec<RankedItem> {
    let recent_rank = |item: &PaletteItem| recent.iter().position(|id| *id == item.id);
    let query = query.trim();
    if query.is_empty() {
        let mut order = (0..items.len()).collect::<Vec<_>>();
        order.sort_by_key(|index| (recent_rank(&items[*index]).unwrap_or(usize::MAX), *index));
        return order
            .into_iter()
            .map(|index| RankedItem {
                index,
                title_matches: Vec::new(),
            })
            .collect();
    }

    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buffer = Vec::new();
    let mut indices = Vec::new();
    let mut scored = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let title_chars = item.title.chars().count() as u32;
        let haystack = format!("{} {} {}", item.title, item.kind, item.detail);
        indices.clear();
        let Some(score) = pattern.indices(
            Utf32Str::new(&haystack, &mut buffer),
            &mut matcher,
            &mut indices,
        ) else {
            continue;
        };
        indices.sort_unstable();
        indices.dedup();
        let title_only = indices.iter().all(|offset| *offset < title_chars);
        let title_matches = indices
            .iter()
            .copied()
            .filter(|offset| *offset < title_chars)
            .collect::<Vec<_>>();
        let score = score + if title_only { TITLE_MATCH_BONUS } else { 0 };
        scored.push((score, recent_rank(item), index, title_matches));
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| {
                left.1
                    .unwrap_or(usize::MAX)
                    .cmp(&right.1.unwrap_or(usize::MAX))
            })
            .then_with(|| left.2.cmp(&right.2))
    });
    scored
        .into_iter()
        .map(|(_, _, index, title_matches)| RankedItem {
            index,
            title_matches,
        })
        .collect()
}

fn agent_title(agent: &crate::protocol::ClientShellAgent) -> &str {
    agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref())
        .unwrap_or("agent")
}

impl ClientShellState {
    /// Every entry the palette can run right now.
    pub(super) fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = Vec::new();
        let keybinds = &self.config.keybinds.keybinds;
        for spec in action_specs(keybinds) {
            if spec.action == crate::input::KeybindAction::CommandPalette {
                continue;
            }
            items.push(PaletteItem {
                id: format!("action:{}", spec.label),
                kind: spec.category.label(),
                title: spec.label.to_owned(),
                detail: spec.bindings.label().unwrap_or_default(),
                action: PaletteAction::Binding(spec.action),
            });
        }
        for command in &keybinds.custom_commands {
            let title = command
                .description
                .clone()
                .unwrap_or_else(|| command.command.clone());
            let upstream = command.command.contains("upstream review");
            items.push(PaletteItem {
                id: format!("command:{}", command.command),
                kind: if upstream { "upstream" } else { "command" },
                title,
                detail: command.bindings.label().unwrap_or_default(),
                action: PaletteAction::Command(command.clone()),
            });
        }
        for section in ClientSettingsSection::ALL {
            items.push(PaletteItem {
                id: format!("settings:{}", section.label()),
                kind: "settings",
                title: section.label().to_owned(),
                detail: String::new(),
                action: PaletteAction::Settings(*section),
            });
        }

        let federated = self.endpoints.len() > 1;
        for endpoint in &self.endpoints {
            if endpoint.status != ClientEndpointStatus::Online {
                continue;
            }
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            let key = endpoint.endpoint_id.storage_key();
            let machine = |rest: &str| match (federated, rest.is_empty()) {
                (true, true) => endpoint.label.clone(),
                (true, false) => format!("{} · {rest}", endpoint.label),
                (false, _) => rest.to_owned(),
            };
            let workspace_label = |workspace_id: &str| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == workspace_id)
                    .map_or("workspace", |workspace| workspace.label.as_str())
            };
            let tab_label = |tab_id: &str| {
                snapshot
                    .tabs
                    .iter()
                    .find(|tab| tab.tab_id == tab_id)
                    .map_or("tab", |tab| tab.label.as_str())
            };
            for workspace in &snapshot.workspaces {
                items.push(PaletteItem {
                    id: format!("workspace:{key}:{}", workspace.workspace_id),
                    kind: "workspace",
                    title: workspace.label.clone(),
                    detail: machine(workspace.branch.as_deref().unwrap_or_default()),
                    action: PaletteAction::Focus {
                        endpoint_id: endpoint.endpoint_id.clone(),
                        target: ClientEndpointFocusTarget::Workspace(
                            workspace.workspace_id.clone(),
                        ),
                    },
                });
            }
            for tab in &snapshot.tabs {
                items.push(PaletteItem {
                    id: format!("tab:{key}:{}", tab.tab_id),
                    kind: "tab",
                    title: format!("{} / {}", workspace_label(&tab.workspace_id), tab.label),
                    detail: machine(""),
                    action: PaletteAction::Focus {
                        endpoint_id: endpoint.endpoint_id.clone(),
                        target: ClientEndpointFocusTarget::Tab(tab.tab_id.clone()),
                    },
                });
            }
            for agent in &snapshot.agents {
                items.push(PaletteItem {
                    id: format!("agent:{key}:{}", agent.pane_id),
                    kind: "agent",
                    title: format!(
                        "{} · {}",
                        agent_title(agent),
                        workspace_label(&agent.workspace_id)
                    ),
                    detail: machine(status_text(agent.agent_status)),
                    action: PaletteAction::Focus {
                        endpoint_id: endpoint.endpoint_id.clone(),
                        target: ClientEndpointFocusTarget::Pane(agent.pane_id.clone()),
                    },
                });
            }
            for pane in &snapshot.panes {
                if snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane.pane_id)
                {
                    continue;
                }
                let name = pane
                    .label
                    .as_deref()
                    .map(str::to_owned)
                    .or_else(|| {
                        let cwd = pane.foreground_cwd.as_deref().or(pane.cwd.as_deref())?;
                        std::path::Path::new(cwd)
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| pane.pane_id.clone());
                items.push(PaletteItem {
                    id: format!("pane:{key}:{}", pane.pane_id),
                    kind: "pane",
                    title: format!(
                        "{} / {} / {name}",
                        workspace_label(&pane.workspace_id),
                        tab_label(&pane.tab_id)
                    ),
                    detail: machine(""),
                    action: PaletteAction::Focus {
                        endpoint_id: endpoint.endpoint_id.clone(),
                        target: ClientEndpointFocusTarget::Pane(pane.pane_id.clone()),
                    },
                });
            }
        }
        items
    }

    pub(super) fn open_command_palette(&mut self) {
        self.overlay = Some(ClientShellOverlay::Palette(ClientPaletteOverlay {
            query: TextEditor::default(),
            selected: 0,
            scroll: 0,
        }));
    }

    fn palette_ranked(&self) -> (Vec<PaletteItem>, Vec<RankedItem>) {
        let items = self.palette_items();
        let query = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Palette(palette)) => palette.query.as_str(),
            _ => "",
        };
        let ranked = rank_items(&items, query, &self.palette_recent);
        (items, ranked)
    }

    fn palette_viewport_rows(&self) -> usize {
        self.hits.palette.viewport_rows.max(1)
    }

    fn move_palette_selection(&mut self, delta: isize) {
        let (_, ranked) = self.palette_ranked();
        let viewport = self.palette_viewport_rows();
        let Some(ClientShellOverlay::Palette(palette)) = self.overlay.as_mut() else {
            return;
        };
        if ranked.is_empty() {
            palette.selected = 0;
            palette.scroll = 0;
            return;
        }
        palette.selected =
            (palette.selected as isize + delta).clamp(0, ranked.len() as isize - 1) as usize;
        if palette.selected < palette.scroll {
            palette.scroll = palette.selected;
        } else if palette.selected >= palette.scroll + viewport {
            palette.scroll = palette.selected + 1 - viewport;
        }
    }

    pub(super) fn insert_palette_text(&mut self, text: &str) -> bool {
        let Some(ClientShellOverlay::Palette(palette)) = self.overlay.as_mut() else {
            return false;
        };
        if palette.query.insert(text) {
            palette.selected = 0;
            palette.scroll = 0;
        }
        true
    }

    /// Handle a key while the palette is open. Returns false when it is not.
    pub(super) fn route_palette_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::KeyModifiers;

        if !matches!(self.overlay, Some(ClientShellOverlay::Palette(_))) {
            return false;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        outcome.repaint = true;
        let ctrl = modifiers == KeyModifiers::CONTROL;
        let plain = modifiers.is_empty();
        match code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Enter if plain => self.run_palette_selection(outcome),
            KeyCode::Up if plain => self.move_palette_selection(-1),
            KeyCode::Char('p') if ctrl => self.move_palette_selection(-1),
            KeyCode::Down if plain => self.move_palette_selection(1),
            KeyCode::Char('n') if ctrl => self.move_palette_selection(1),
            KeyCode::PageUp if plain => {
                let page = self.palette_viewport_rows() as isize;
                self.move_palette_selection(-page);
            }
            KeyCode::PageDown if plain => {
                let page = self.palette_viewport_rows() as isize;
                self.move_palette_selection(page);
            }
            _ => {
                if let Some(ClientShellOverlay::Palette(palette)) = self.overlay.as_mut() {
                    if let Some(changed) = palette.query.handle_key(key) {
                        if changed {
                            palette.selected = 0;
                            palette.scroll = 0;
                        }
                    } else {
                        outcome.repaint = false;
                    }
                }
            }
        }
        true
    }

    /// Handle a mouse event while the palette is open. Returns false when it
    /// is not, so the event continues to the regular routing.
    pub(super) fn route_palette_mouse(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};

        if !matches!(self.overlay, Some(ClientShellOverlay::Palette(_))) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let row = self
                    .hits
                    .palette
                    .rows
                    .iter()
                    .find(|(rect, _)| contains(*rect, point))
                    .map(|(_, index)| *index);
                if let Some(index) = row {
                    if let Some(ClientShellOverlay::Palette(palette)) = self.overlay.as_mut() {
                        palette.selected = index;
                    }
                    self.run_palette_selection(outcome);
                } else if !contains(self.hits.palette.popup, point) {
                    self.overlay = None;
                }
                outcome.repaint = true;
            }
            MouseEventKind::ScrollUp => {
                self.move_palette_selection(-3);
                outcome.repaint = true;
            }
            MouseEventKind::ScrollDown => {
                self.move_palette_selection(3);
                outcome.repaint = true;
            }
            _ => {}
        }
        true
    }

    fn record_palette_use(&mut self, id: &str) {
        self.palette_recent.retain(|recent| recent != id);
        self.palette_recent.insert(0, id.to_owned());
        self.palette_recent.truncate(MAX_RECENT);
    }

    fn run_palette_selection(&mut self, outcome: &mut ClientShellInput) {
        let (items, ranked) = self.palette_ranked();
        let selected = match self.overlay.as_ref() {
            Some(ClientShellOverlay::Palette(palette)) => palette.selected,
            _ => return,
        };
        let Some(item) = ranked
            .get(selected)
            .and_then(|ranked| items.get(ranked.index))
            .cloned()
        else {
            return;
        };
        self.record_palette_use(&item.id);
        self.overlay = None;
        self.run_palette_action(item.action, outcome);
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }

    pub(super) fn run_palette_action(
        &mut self,
        action: PaletteAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            PaletteAction::Binding(action) => {
                self.record_binding(crate::input::KeybindMatch::Action(action), outcome);
            }
            PaletteAction::Command(command) => {
                self.record_binding(crate::input::KeybindMatch::Command(command), outcome);
            }
            PaletteAction::Focus {
                endpoint_id,
                target,
            } => {
                self.focus_or_activate(endpoint_id, target, outcome);
            }
            PaletteAction::Settings(section) => {
                self.open_settings_overlay();
                self.select_settings_section(section, outcome);
            }
        }
    }
}

pub(super) struct PaletteRender {
    pub(super) area: Rect,
    pub(super) rows: Vec<(Rect, usize)>,
    pub(super) viewport_rows: usize,
    pub(super) cursor: Option<crate::protocol::CursorState>,
}

fn popup_rect(area: Rect) -> Option<Rect> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }
    let width = area.width.saturating_sub(4).min(MAX_WIDTH);
    let height = area.height.saturating_sub(2).min(MAX_HEIGHT);
    Some(Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 4,
        width,
        height,
    ))
}

/// Draw `text` with the matched char offsets highlighted. Returns the width used.
fn put_highlighted(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    matches: &[u32],
    base: Style,
    highlight: Style,
) {
    let mut cursor = x;
    let right = x.saturating_add(width);
    for (offset, ch) in text.chars().enumerate() {
        let mut encoded = [0u8; 4];
        let symbol = ch.encode_utf8(&mut encoded);
        let cell_width = display_width(symbol);
        if cursor + cell_width > right {
            break;
        }
        let style = if matches.binary_search(&(offset as u32)).is_ok() {
            highlight
        } else {
            base
        };
        put_text(buffer, cursor, y, cell_width, symbol, style);
        cursor += cell_width;
    }
}

pub(super) fn render_palette(
    buffer: &mut Buffer,
    palette_state: &ClientPaletteOverlay,
    items: &[PaletteItem],
    ranked: &[RankedItem],
    palette: &Palette,
) -> Option<PaletteRender> {
    let outer = popup_rect(buffer.area)?;
    let inner = draw_panel(buffer, outer, "command palette", palette)?;
    if inner.height < 4 || inner.width < 10 {
        return Some(PaletteRender {
            area: outer,
            rows: Vec::new(),
            viewport_rows: 1,
            cursor: None,
        });
    }

    let count = format!("{}/{}", ranked.len(), items.len());
    let count_width = display_width(&count) + 1;
    put_text(buffer, inner.x + 1, inner.y, 1, "›", key_style(palette));
    let field = Rect::new(
        inner.x + 3,
        inner.y,
        inner.width.saturating_sub(3 + count_width),
        1,
    );
    let cursor = text_editor::render(buffer, field, &palette_state.query, base_style(palette));
    if palette_state.query.is_empty() {
        put_text(
            buffer,
            field.x,
            field.y,
            field.width,
            "type to search actions, agents, workspaces, tabs",
            dim_style(palette),
        );
    }
    put_text(
        buffer,
        inner.right().saturating_sub(count_width),
        inner.y,
        count_width,
        &count,
        dim_style(palette),
    );
    for x in inner.x..inner.right() {
        put_text(
            buffer,
            x,
            inner.y + 1,
            1,
            "─",
            Style::default().fg(palette.surface1).bg(palette.panel_bg),
        );
    }

    let list = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(3),
    );
    let viewport_rows = usize::from(list.height).max(1);
    let selected = palette_state.selected.min(ranked.len().saturating_sub(1));
    let mut scroll = palette_state
        .scroll
        .min(ranked.len().saturating_sub(viewport_rows));
    if selected < scroll {
        scroll = selected;
    } else if selected >= scroll + viewport_rows {
        scroll = selected + 1 - viewport_rows;
    }
    if ranked.is_empty() {
        put_text(
            buffer,
            list.x + 1,
            list.y,
            list.width.saturating_sub(1),
            "No matching entries",
            dim_style(palette),
        );
    }
    let mut rows = Vec::new();
    for (position, entry) in ranked.iter().enumerate().skip(scroll).take(viewport_rows) {
        let Some(item) = items.get(entry.index) else {
            continue;
        };
        let y = list.y + (position - scroll) as u16;
        let row = Rect::new(list.x, y, list.width, 1);
        rows.push((row, position));
        let is_selected = position == selected;
        let (row_style, kind_style, detail_style, highlight) = if is_selected {
            (
                selected_style(palette),
                selected_style(palette),
                selected_style(palette),
                selected_style(palette).add_modifier(Modifier::UNDERLINED),
            )
        } else {
            (
                base_style(palette),
                dim_style(palette),
                dim_style(palette),
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )
        };
        buffer.set_style(row, row_style);
        put_text(
            buffer,
            row.x + 1,
            y,
            KIND_WIDTH,
            &crate::ui::truncate_end(item.kind, usize::from(KIND_WIDTH) - 1),
            kind_style,
        );
        let detail_width = display_width(&item.detail).min(row.width / 3);
        let title_x = row.x + 1 + KIND_WIDTH;
        let title_width = row
            .right()
            .saturating_sub(title_x + detail_width + u16::from(detail_width > 0) * 2);
        let title = crate::ui::truncate_end(&item.title, usize::from(title_width));
        put_highlighted(
            buffer,
            title_x,
            y,
            title_width,
            &title,
            &entry.title_matches,
            row_style,
            highlight,
        );
        if detail_width > 0 {
            let detail = crate::ui::truncate_end(&item.detail, usize::from(detail_width));
            put_text(
                buffer,
                row.right().saturating_sub(display_width(&detail) + 1),
                y,
                detail_width,
                &detail,
                detail_style,
            );
        }
    }
    let hint_width = usize::from(inner.width.saturating_sub(2));
    let hint = [
        "↑/↓ ctrl+n/p move   enter run   esc close",
        "↑/↓ move   enter run   esc close",
        "enter run   esc close",
    ]
    .into_iter()
    .find(|hint| hint.chars().count() <= hint_width)
    .unwrap_or("esc close");
    put_text(
        buffer,
        inner.x + 1,
        inner.bottom() - 1,
        inner.width.saturating_sub(2),
        hint,
        dim_style(palette),
    );
    Some(PaletteRender {
        area: outer,
        rows,
        viewport_rows,
        cursor,
    })
}

impl ClientShellState {
    pub(super) fn render_palette_overlay(&self, buffer: &mut Buffer) -> Option<PaletteRender> {
        let Some(ClientShellOverlay::Palette(palette_state)) = self.overlay.as_ref() else {
            return None;
        };
        let (items, ranked) = self.palette_ranked();
        render_palette(buffer, palette_state, &items, &ranked, &self.config.palette)
    }
}

#[cfg(test)]
mod tests {
    use super::super::popup_frame::buffer_row;
    use super::super::tests::{frame_rows, snapshot, surface};
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::client::endpoint::{ClientEndpointStatus, ProfileId, SavedSshEndpoint};
    use crate::config::Config;
    use crate::input::{TerminalKey, TextCommit};
    use crate::protocol::{ClientShellAgent, ClientShellWorkspace};
    use crate::raw_input::RawInputEvent;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

    fn item(kind: &'static str, title: &str, detail: &str) -> PaletteItem {
        PaletteItem {
            id: format!("{kind}:{title}"),
            kind,
            title: title.to_owned(),
            detail: detail.to_owned(),
            action: PaletteAction::Binding(crate::input::KeybindAction::NewTab),
        }
    }

    fn titles(items: &[PaletteItem], ranked: &[RankedItem]) -> Vec<String> {
        ranked
            .iter()
            .map(|ranked| items[ranked.index].title.clone())
            .collect()
    }

    fn agent(pane_id: &str, name: &str, status: AgentStatus) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
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

    fn workspace(id: &str, label: &str) -> ClientShellWorkspace {
        let mut workspace = snapshot().workspaces.remove(0);
        workspace.workspace_id = id.into();
        workspace.label = label.into();
        workspace.focused = false;
        workspace
    }

    fn palette_state() -> ClientShellState {
        let mut config = ClientShellConfig::from_config(&Config::default());
        config.preferences_path = None;
        let mut state = ClientShellState::new(config);
        let mut projection = snapshot();
        projection
            .workspaces
            .push(workspace("ws_2", "second-space"));
        projection
            .agents
            .push(agent("pane_1", "reviewer", AgentStatus::Working));
        state.set_snapshot(Box::new(projection));
        state.set_pane_surface(surface());
        state
    }

    fn state_with_remote() -> (ClientShellState, ClientEndpointId) {
        let mut state = palette_state();
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
        remote.workspaces[0].label = "remote-space".into();
        remote
            .agents
            .push(agent("pane_1", "builder", AgentStatus::Blocked));
        let mut shell_pane = remote.panes[0].clone();
        shell_pane.pane_id = "pane_2".into();
        shell_pane.cwd = Some("/repo/build".into());
        shell_pane.foreground_cwd = None;
        shell_pane.focused = false;
        remote.panes.push(shell_pane);
        state.set_endpoint_snapshot(&endpoint_id, Box::new(remote));
        (state, endpoint_id)
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> RawInputEvent {
        RawInputEvent::Key(TerminalKey::new(code, modifiers))
    }

    fn type_text(state: &mut ClientShellState, text: &str) {
        state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new(text.to_owned()))]);
    }

    fn open(state: &mut ClientShellState) {
        state.handle_raw_events(vec![
            key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            key(KeyCode::Char(':'), KeyModifiers::SHIFT),
        ]);
        assert!(
            matches!(state.overlay, Some(ClientShellOverlay::Palette(_))),
            "prefix+: opens the palette"
        );
    }

    fn selected_title(state: &ClientShellState) -> String {
        let (items, ranked) = state.palette_ranked();
        let Some(ClientShellOverlay::Palette(palette)) = state.overlay.as_ref() else {
            panic!("palette closed");
        };
        items[ranked[palette.selected].index].title.clone()
    }

    #[test]
    fn default_palette_binding_is_free_and_resolves_for_both_colon_encodings() {
        let config = Config::default();
        assert!(config.collect_diagnostics().is_empty());
        let keybinds = config.keybinds();
        for key in [
            TerminalKey::new(KeyCode::Char(':'), KeyModifiers::SHIFT)
                .with_generated_text(Some(":".to_owned())),
            TerminalKey::new(KeyCode::Char(':'), KeyModifiers::empty()),
            TerminalKey::new(KeyCode::Char(':'), KeyModifiers::SHIFT),
            TerminalKey::new(KeyCode::Char(';'), KeyModifiers::SHIFT)
                .with_generated_text(Some(":".to_owned())),
        ] {
            assert!(
                matches!(
                    crate::input::resolve_prefix_binding(&keybinds, &key),
                    Some(crate::input::KeybindMatch::Action(
                        crate::input::KeybindAction::CommandPalette
                    ))
                ),
                "{key:?}"
            );
        }
    }

    #[test]
    fn palette_binding_can_be_rebound_or_unbound() {
        let config: Config = toml::from_str("[keys]\ncommand_palette = \"ctrl+k\"\n").unwrap();
        assert!(config.collect_diagnostics().is_empty());
        let mut keybinds = config.keybinds();
        assert!(matches!(
            crate::input::resolve_direct_binding(
                &keybinds,
                &TerminalKey::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
            ),
            Some(crate::input::KeybindMatch::Action(
                crate::input::KeybindAction::CommandPalette
            ))
        ));
        keybinds.command_palette = crate::config::ActionKeybinds::default();
        assert!(crate::input::resolve_prefix_binding(
            &keybinds,
            &TerminalKey::new(KeyCode::Char(':'), KeyModifiers::SHIFT)
        )
        .is_none());
    }

    #[test]
    fn fuzzy_ranking_filters_and_orders_by_match_quality() {
        let items = vec![
            item("tabs", "new tab", "prefix+c"),
            item("tabs", "rename tab", "prefix+shift+t"),
            item("tabs", "next tab", "prefix+n"),
            item("workspaces", "new workspace", "prefix+shift+n"),
            item("panes", "zoom pane", "prefix+z"),
        ];
        let ranked = rank_items(&items, "rtab", &[]);
        assert_eq!(titles(&items, &ranked)[0], "rename tab");
        assert!(!titles(&items, &ranked).contains(&"zoom pane".to_owned()));

        let ranked = rank_items(&items, "zm", &[]);
        assert_eq!(titles(&items, &ranked), ["zoom pane"]);

        assert!(rank_items(&items, "qqq", &[]).is_empty());
        // Smart case: an uppercase letter makes the match case-sensitive.
        assert!(rank_items(&items, "Zoom", &[]).is_empty());
        assert_eq!(rank_items(&items, "zoom", &[]).len(), 1);
    }

    #[test]
    fn kind_and_detail_are_searchable_and_title_matches_outrank_them() {
        let items = vec![
            item("workspace", "alpha", "Build · main"),
            item("agent", "workspace notes", "idle"),
            item("tabs", "alpha", "prefix+c"),
        ];
        let ranked = rank_items(&items, "workspace", &[]);
        let ordered = titles(&items, &ranked);
        assert_eq!(ordered[0], "workspace notes", "title match first");
        assert!(ordered.contains(&"alpha".to_owned()), "kind match kept");
        assert_eq!(ranked.len(), 2);
        // Matching on kind alone highlights nothing in the title.
        assert!(ranked[1].title_matches.is_empty());
    }

    #[test]
    fn matched_title_offsets_are_ascending_and_name_the_query_characters() {
        let items = vec![item("tabs", "new tab", "prefix+c")];
        let ranked = rank_items(&items, "nt", &[]);
        let matches = &ranked[0].title_matches;
        assert_eq!(matches.len(), 2);
        assert!(matches.windows(2).all(|pair| pair[0] < pair[1]));
        let chars = "new tab".chars().collect::<Vec<_>>();
        let found = matches
            .iter()
            .map(|index| chars[*index as usize])
            .collect::<String>();
        assert_eq!(found, "nt");
    }

    #[test]
    fn empty_query_lists_recent_entries_first_in_recency_order() {
        let items = vec![
            item("tabs", "new tab", ""),
            item("tabs", "close tab", ""),
            item("panes", "zoom pane", ""),
            item("session", "detach", ""),
        ];
        let recent = vec!["session:detach".to_owned(), "tabs:close tab".to_owned()];
        let ranked = rank_items(&items, "  ", &recent);
        assert_eq!(
            titles(&items, &ranked),
            ["detach", "close tab", "new tab", "zoom pane"]
        );
        assert!(ranked.iter().all(|ranked| ranked.title_matches.is_empty()));
        // Recency breaks ties between equally good matches.
        let ranked = rank_items(&items, "tab", &recent);
        assert_eq!(titles(&items, &ranked)[0], "close tab");
    }

    #[test]
    fn items_cover_actions_commands_navigation_targets_and_settings() {
        let (mut state, _) = state_with_remote();
        let config: Config = toml::from_str(
            "[[keys.command]]\nkey = \"prefix+y\"\ncommand = \"echo hi\"\ndescription = \"say hi\"\n",
        )
        .unwrap();
        state.config.keybinds = config.live_keybinds_with_diagnostics().unwrap().0;
        let items = state.palette_items();
        let find = |kind: &str, title: &str| {
            items
                .iter()
                .find(|item| item.kind == kind && item.title == title)
                .unwrap_or_else(|| panic!("{kind} {title} missing"))
        };
        assert_eq!(find("tabs", "new tab").detail, "prefix+c");
        assert_eq!(find("agents", "rename agent").detail, "prefix+shift+a");
        assert_eq!(
            find("agents", "next agent").detail,
            "",
            "unbound actions are listed"
        );
        assert_eq!(find("command", "say hi").detail, "prefix+y");
        assert_eq!(
            find("upstream", "Review upstream herdr updates").detail,
            "prefix+u"
        );
        assert_eq!(find("settings", "tones").detail, "");
        assert!(items.iter().all(|item| item.title != "command palette"));
        // Agents and workspaces from every machine, labelled with it.
        assert_eq!(
            find("agent", "reviewer · client-shell").detail,
            "Local · working"
        );
        assert_eq!(
            find("agent", "builder · remote-space").detail,
            "Build · blocked"
        );
        assert_eq!(find("workspace", "second-space").detail, "Local · main");
        assert_eq!(find("workspace", "remote-space").detail, "Build · main");
        assert_eq!(find("tab", "client-shell / 1").detail, "Local");
        assert_eq!(find("pane", "remote-space / 1 / build").detail, "Build");
        // Agent panes are listed once, as agents.
        assert!(!items
            .iter()
            .any(|item| item.kind == "pane" && item.id.ends_with(":pane_1")));
    }

    #[test]
    fn prefix_colon_opens_and_esc_closes_without_touching_the_pane() {
        let mut state = palette_state();
        open(&mut state);
        let outcome = state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
        assert!(outcome.repaint && outcome.requests.is_empty() && outcome.actions.is_empty());
    }

    #[test]
    fn typing_filters_and_navigation_keys_move_the_selection() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "tab");
        let (items, ranked) = state.palette_ranked();
        assert!(ranked.len() >= 3 && ranked.len() < items.len());
        let first = selected_title(&state);
        for events in [
            vec![key(KeyCode::Down, KeyModifiers::empty())],
            vec![key(KeyCode::Char('n'), KeyModifiers::CONTROL)],
        ] {
            let before = match state.overlay.as_ref() {
                Some(ClientShellOverlay::Palette(palette)) => palette.selected,
                _ => panic!("palette closed"),
            };
            state.handle_raw_events(events);
            let after = match state.overlay.as_ref() {
                Some(ClientShellOverlay::Palette(palette)) => palette.selected,
                _ => panic!("palette closed"),
            };
            assert_eq!(after, before + 1);
        }
        for events in [
            vec![key(KeyCode::Up, KeyModifiers::empty())],
            vec![key(KeyCode::Char('p'), KeyModifiers::CONTROL)],
        ] {
            state.handle_raw_events(events);
        }
        assert_eq!(selected_title(&state), first);
        // The selection never leaves the list.
        state.handle_raw_events(vec![key(KeyCode::Up, KeyModifiers::empty())]);
        assert_eq!(selected_title(&state), first);
        for _ in 0..200 {
            state.handle_raw_events(vec![key(KeyCode::Down, KeyModifiers::empty())]);
        }
        let Some(ClientShellOverlay::Palette(palette)) = state.overlay.as_ref() else {
            panic!("palette closed");
        };
        assert_eq!(palette.selected, ranked.len() - 1);
        // Editing the query resets the selection to the best match.
        state.handle_raw_events(vec![key(KeyCode::Backspace, KeyModifiers::empty())]);
        let Some(ClientShellOverlay::Palette(palette)) = state.overlay.as_ref() else {
            panic!("palette closed");
        };
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn enter_runs_a_binding_action_exactly_like_its_key() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "split vertical");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(request.method, crate::api::schema::Method::PaneSplit(_))
            )),
            "{:?}",
            outcome.actions
        );

        // Actions that open UI open it from the palette too.
        open(&mut state);
        type_text(&mut state, "rename tab");
        state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::Tab { .. },
                ..
            }))
        ));
        state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);

        open(&mut state);
        type_text(&mut state, "rename agent");
        state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::Agent { .. },
                ..
            }))
        ));
    }

    #[test]
    fn enter_switches_workspace_and_focuses_agents_on_every_machine() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "second-space");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(
            outcome.actions.iter().any(|action| matches!(
                action,
                ClientShellAction::Endpoint { request, .. }
                    if matches!(
                        &request.method,
                        crate::api::schema::Method::WorkspaceFocus(target)
                            if target.workspace_id == "ws_2"
                    )
            )),
            "{:?}",
            outcome.actions
        );

        let (mut state, remote) = state_with_remote();
        open(&mut state);
        type_text(&mut state, "builder");
        assert_eq!(selected_title(&state), "builder · remote-space");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
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
    fn enter_opens_settings_at_the_chosen_section() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "tones");
        state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Tones,
                ..
            }))
        ));
    }

    #[test]
    fn enter_with_no_match_keeps_the_palette_open() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "qqqq");
        let outcome = state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Palette(_))
        ));
        assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
    }

    #[test]
    fn recently_used_entries_lead_and_survive_a_restart() {
        let path =
            std::env::temp_dir().join(format!("flockr-palette-recent-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let build = || {
            let mut config = ClientShellConfig::from_config(&Config::default())
                .with_preferences_path(path.clone());
            config.local_config_path = path.with_extension("toml");
            let mut state = ClientShellState::new(config);
            state.set_snapshot(Box::new(snapshot()));
            state.set_pane_surface(surface());
            state
        };
        let mut state = build();
        for query in ["zoom pane", "detach"] {
            open(&mut state);
            type_text(&mut state, query);
            state.handle_raw_events(vec![key(KeyCode::Enter, KeyModifiers::empty())]);
        }
        assert_eq!(state.palette_recent, ["action:detach", "action:zoom pane"]);

        let mut restarted = build();
        assert_eq!(
            restarted.palette_recent,
            ["action:detach", "action:zoom pane"]
        );
        open(&mut restarted);
        let (items, ranked) = restarted.palette_ranked();
        assert_eq!(
            titles(&items, &ranked)[..2],
            ["detach".to_owned(), "zoom pane".to_owned()]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recents_are_deduplicated_and_capped() {
        let mut state = palette_state();
        for index in 0..30 {
            state.record_palette_use(&format!("action:{index}"));
        }
        state.record_palette_use("action:5");
        assert_eq!(state.palette_recent.len(), MAX_RECENT);
        assert_eq!(state.palette_recent[0], "action:5");
        assert_eq!(
            state
                .palette_recent
                .iter()
                .filter(|id| *id == "action:5")
                .count(),
            1
        );
    }

    fn rendered(state: &mut ClientShellState, cols: u16, rows: u16) -> FrameData {
        state.compose(cols, rows).expect("frame").frame
    }

    #[test]
    fn popup_renders_prompt_rows_key_and_hint_at_several_sizes() {
        for (cols, rows) in [(160, 50), (100, 30), (80, 24), (44, 12)] {
            let mut state = palette_state();
            open(&mut state);
            type_text(&mut state, "tab");
            let frame = rendered(&mut state, cols, rows);
            let text = frame_rows(&frame).join("\n");
            assert!(text.contains("command palette"), "{cols}x{rows}\n{text}");
            assert!(text.contains("› tab"), "{cols}x{rows}\n{text}");
            assert!(text.contains("esc close"), "{cols}x{rows}\n{text}");
            let popup = state.hits.palette.popup;
            assert!(popup.right() <= cols && popup.bottom() <= rows, "{popup:?}");
            assert!(!state.hits.palette.rows.is_empty());
        }
        // No room: nothing is drawn but the keyboard still works.
        let mut state = palette_state();
        open(&mut state);
        let frame = rendered(&mut state, 24, 6);
        assert!(!frame_rows(&frame).join("\n").contains("command palette"));
        state.handle_raw_events(vec![key(KeyCode::Esc, KeyModifiers::empty())]);
        assert!(state.overlay.is_none());
    }

    #[test]
    fn rows_right_align_the_key_and_highlight_matched_characters() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "zoom");
        let frame = rendered(&mut state, 100, 30);
        let rows = frame_rows(&frame);
        let (rect, _) = state.hits.palette.rows[0];
        let row = &rows[usize::from(rect.y)];
        let chars = row.chars().collect::<Vec<_>>();
        let line = chars[usize::from(rect.x)..usize::from(rect.right())]
            .iter()
            .collect::<String>();
        assert!(line.contains("zoom pane"), "{line:?}");
        assert!(line.trim_end().ends_with("prefix+z"), "{line:?}");
        // First row is selected: accent background, matched chars underlined.
        let palette = &state.config.palette;
        let buffer = frame.to_ratatui_buffer().unwrap();
        let x = rect.x + 1 + KIND_WIDTH;
        assert_eq!(buffer[(x, rect.y)].symbol(), "z");
        assert_eq!(buffer[(x, rect.y)].bg, palette.accent);
        assert!(buffer[(x, rect.y)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED));
        let unmatched = x + 5; // the "p" of "pane"
        assert!(!buffer[(unmatched, rect.y)]
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED));
        let _ = buffer_row;
    }

    #[test]
    fn mouse_click_runs_a_row_wheel_scrolls_and_outside_click_closes() {
        let mut state = palette_state();
        open(&mut state);
        type_text(&mut state, "tab");
        rendered(&mut state, 100, 30);
        let before = state.hits.palette.rows.len();
        assert!(before > 2);

        let wheel = |state: &mut ClientShellState, kind| {
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 50,
                row: 10,
                modifiers: KeyModifiers::empty(),
            })])
        };
        wheel(&mut state, MouseEventKind::ScrollDown);
        let Some(ClientShellOverlay::Palette(palette)) = state.overlay.as_ref() else {
            panic!("palette closed");
        };
        assert_eq!(palette.selected, 3.min(before - 1));
        wheel(&mut state, MouseEventKind::ScrollUp);
        let Some(ClientShellOverlay::Palette(palette)) = state.overlay.as_ref() else {
            panic!("palette closed");
        };
        assert_eq!(palette.selected, 0);

        // Click the second row: it runs.
        let (rect, _) = state.hits.palette.rows[1];
        let (items, ranked) = state.palette_ranked();
        let expected = items[ranked[1].index].title.clone();
        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + 2,
                row: rect.y,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(
            state.overlay.is_none()
                || !matches!(state.overlay, Some(ClientShellOverlay::Palette(_)))
        );
        assert!(outcome.repaint);
        assert_eq!(state.palette_recent.len(), 1);
        assert!(state.palette_recent[0].ends_with(&expected), "{expected}");

        // A click outside the popup closes it without running anything.
        state.overlay = None;
        open(&mut state);
        rendered(&mut state, 100, 30);
        let outcome =
            state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 29,
                modifiers: KeyModifiers::empty(),
            })]);
        assert!(state.overlay.is_none());
        assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
    }

    #[test]
    fn paste_goes_into_the_query() {
        let mut state = palette_state();
        open(&mut state);
        state.handle_raw_events(vec![RawInputEvent::Paste("zoom".to_owned())]);
        assert_eq!(selected_title(&state), "zoom pane");
    }
}
