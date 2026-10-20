// Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE.
use std::collections::HashMap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::agent_groups::{
    assign_tree_guides, group_agent_items, group_headers_enabled, item_layout, render_group_header,
    ungrouped_items, AgentGroupMember, AgentGroupSeed, AgentHeaderHit, AgentListItem,
    CollapsedGroups, TreeGuide,
};
use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
    /// Tree guide drawn left of the row; set only in grouped lists.
    pub(super) tree: TreeGuide,
}

/// Drop the machine and workspace labels a group header already shows. A line
/// that would be left with nothing but the state icon keeps its full form.
pub(super) fn strip_group_labels(row: &mut AgentRow, machine_too: bool) {
    use crate::ui::ResolvedTokenKind;
    let redundant = |kind: &ResolvedTokenKind| match kind {
        ResolvedTokenKind::Workspace(_) => true,
        ResolvedTokenKind::Machine(_) => machine_too,
        _ => false,
    };
    for line in &mut row.rows {
        if !line.iter().any(|token| redundant(&token.kind)) {
            continue;
        }
        let keeps_content = line.iter().any(|token| {
            !redundant(&token.kind) && !matches!(token.kind, ResolvedTokenKind::StateIcon)
        });
        if keeps_content {
            line.retain(|token| !redundant(&token.kind));
        }
    }
}

const GUIDE_FULL_MIN_WIDTH: u16 = 14;
const GUIDE_COMPACT_MIN_WIDTH: u16 = 8;

/// Guide text for line `index` of a row and its display width. Guides shrink
/// and then vanish on narrow sidebars so the state icon always fits.
fn tree_prefix(tree: TreeGuide, index: usize, width: u16) -> (&'static str, u16) {
    let first = index == 0;
    if tree == TreeGuide::None || width < GUIDE_COMPACT_MIN_WIDTH {
        return if first { (" ", 1) } else { ("   ", 3) };
    }
    let last = tree == TreeGuide::Last;
    if width >= GUIDE_FULL_MIN_WIDTH {
        match (first, last) {
            (true, false) => (" ├─ ", 4),
            (true, true) => (" └─ ", 4),
            (false, false) => (" │    ", 6),
            (false, true) => ("      ", 6),
        }
    } else {
        match (first, last) {
            (true, false) => (" ├ ", 3),
            (true, true) => (" └ ", 3),
            (false, false) => (" │   ", 5),
            (false, true) => ("     ", 5),
        }
    }
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    endpoint_id: &ClientEndpointId,
    collapsed: CollapsedGroups<'_>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
    ) {
        return;
    }

    let items = agent_items(snapshot, config, endpoint_id, collapsed, None);
    render_agent_list(
        buffer,
        area,
        &items,
        snapshot
            .agent_view_label
            .as_ref()
            .map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.rows.len(),
        |buffer, rect, row, hits| {
            hits.agents.push((rect, row.pane_id.clone()));
            render_agent_row(buffer, rect, row, config);
        },
    );
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) -> bool {
    if area.height == 0 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(config.palette.surface_dim),
    );
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "grouped",
        crate::config::AgentPanelSortConfig::Priority => "priority",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    items: &[AgentListItem<T>],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    hits.agent_body = body;
    if body.is_empty() || items.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let (row_heights, gaps) = item_layout(items, row_lines, config.agents.row_gap);
    let metrics =
        super::scroll::list_scroll_metrics(&row_heights, &gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, item) in items.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        match item {
            AgentListItem::Header(header) => {
                render_group_header(buffer, rect, header, config);
                hits.agent_headers.push(AgentHeaderHit {
                    rect,
                    endpoint_id: header.endpoint_id.clone(),
                    workspace_id: header.workspace_id.clone(),
                    collapsed: header.collapsed,
                });
            }
            AgentListItem::Row(row) => render_row(buffer, rect, row, hits),
        }
        y = y.saturating_add(height).saturating_add(gaps[index]);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

/// Flat agent rows in list order, without group headers.
pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

/// The agent list of one endpoint: rows under workspace headers when grouping
/// applies (collapsed groups keep only their header), otherwise plain rows.
pub(super) fn agent_items(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    endpoint_id: &ClientEndpointId,
    collapsed: CollapsedGroups<'_>,
    machine: Option<&str>,
) -> Vec<AgentListItem<AgentRow>> {
    let rows = agent_rows(snapshot, config, machine);
    if !group_headers_enabled(config, snapshot.agent_view_label.is_some()) {
        return ungrouped_items(rows);
    }
    let entries = rows
        .into_iter()
        .map(|row| {
            let workspace_id = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == row.pane_id)
                .map(|agent| agent.workspace_id.clone())
                .unwrap_or_default();
            let label = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| workspace.label.clone())
                .unwrap_or_default();
            let seed = AgentGroupSeed {
                endpoint_id: endpoint_id.clone(),
                workspace_id,
                label,
                stale: false,
            };
            (seed, row)
        })
        .collect();
    let mut items = group_agent_items(entries, collapsed, |row| AgentGroupMember {
        status: row.status,
        focused: row.focused,
        pane_id: &row.pane_id,
    });
    assign_tree_guides(&mut items, |row, tree| {
        row.tree = tree;
        strip_group_labels(row, true);
    });
    items
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    // The alias set through `agent.rename` wins over an integration-reported
    // display name, so a rename is always visible in the row.
    let agent_label = agent
        .name
        .as_deref()
        .or(agent.display_agent.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    // `label`: the user's name for this agent. The alias wins, then a tab label
    // the `tab` token would show, then a manual pane name. The workspace is not
    // a fallback: the `workspace` token or the group header already shows it.
    let pane_name = pane.and_then(|pane| pane.label.as_deref());
    let label = agent.name.as_deref().or(tab_label).or(pane_name);
    // `agent_kind`: what runs in the pane, never the alias.
    let agent_kind = agent.display_agent.as_deref().or(agent.agent.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            label,
            agent_kind,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
        tree: TreeGuide::None,
    })
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    let icon = (
        status_icon(row.status, config.status_indicators),
        Style::default().fg(status_color(row.status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    for (index, tokens) in rows.iter().take(rect.height as usize).enumerate() {
        let (prefix, indent) = tree_prefix(row.tree, index, rect.width);
        let mut spans = vec![if row.tree == TreeGuide::None {
            ratatui::text::Span::raw(prefix)
        } else {
            ratatui::text::Span::styled(prefix, secondary)
        }];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            rect.width.saturating_sub(indent) as usize,
        ));
        Paragraph::new(Line::from(spans)).style(row_style).render(
            Rect::new(rect.x, rect.y + index as u16, rect.width, 1),
            buffer,
        );
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}
