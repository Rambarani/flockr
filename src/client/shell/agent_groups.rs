//! Workspace groups in the expanded agent list.
//!
//! A group is a header item followed by that workspace's agent rows. Headers
//! are real list items: they have their own height, scroll position and hit
//! rect, and they carry the collapse toggle. Collapsed state is client-local
//! presentation state and reuses the per-endpoint `collapsed_groups` sets that
//! already persist the workspace-sidebar worktree groups, under a distinct key
//! prefix.

use super::render::put_text;
use super::*;

/// Prefix that keeps agent-group keys apart from worktree group keys (paths)
/// in the shared `collapsed_groups` sets.
const GROUP_KEY_PREFIX: &str = "agents:";

pub(super) fn agent_group_key(workspace_id: &str) -> String {
    format!("{GROUP_KEY_PREFIX}{workspace_id}")
}

/// Collapsed agent groups of every endpoint, borrowed from the shell state.
#[derive(Clone, Copy)]
pub(super) struct CollapsedGroups<'a> {
    pub(super) local: &'a HashSet<String>,
    pub(super) remote: &'a HashMap<ClientEndpointId, HashSet<String>>,
}

impl CollapsedGroups<'_> {
    pub(super) fn is_collapsed(&self, endpoint_id: &ClientEndpointId, workspace_id: &str) -> bool {
        let key = agent_group_key(workspace_id);
        if endpoint_id.is_local() {
            self.local.contains(&key)
        } else {
            self.remote
                .get(endpoint_id)
                .is_some_and(|groups| groups.contains(&key))
        }
    }
}

/// Headers only make sense when the list is ordered by workspace; the
/// priority sort and agent views interleave workspaces on purpose.
pub(super) fn group_headers_enabled(config: &ClientShellConfig, agent_view_active: bool) -> bool {
    config.group_agents_by_workspace
        && !agent_view_active
        && config.agent_panel_sort == crate::config::AgentPanelSortConfig::Spaces
}

/// Workspace header of an agent group.
pub(super) struct AgentGroupHeader {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    label: String,
    /// Agents per display state (blocked, working, done, idle), zero counts
    /// omitted. Counts include agents hidden by a collapsed group.
    counts: Vec<(crate::api::schema::AgentStatus, usize)>,
    pub(super) collapsed: bool,
    /// The focused agent belongs to this group.
    focused: bool,
    stale: bool,
    /// Every agent of the group, including hidden ones, so reveal-on-focus can
    /// land on the header of a collapsed group.
    pane_ids: Vec<String>,
}

pub(super) enum AgentListItem<T> {
    Header(AgentGroupHeader),
    Row(T),
}

/// Where and how a group header is drawn.
pub(super) struct AgentHeaderHit {
    pub(super) rect: Rect,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    pub(super) collapsed: bool,
}

pub(super) struct AgentGroupSeed {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    pub(super) label: String,
    pub(super) stale: bool,
}

pub(super) struct AgentGroupMember<'a> {
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) pane_id: &'a str,
}

pub(super) fn display_status(
    status: crate::api::schema::AgentStatus,
) -> crate::api::schema::AgentStatus {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Unknown => AgentStatus::Idle,
        other => other,
    }
}

pub(super) fn ungrouped_items<T>(rows: Vec<T>) -> Vec<AgentListItem<T>> {
    rows.into_iter().map(AgentListItem::Row).collect()
}

/// Insert a header before every run of entries with the same endpoint and
/// workspace, and drop the rows of collapsed groups. Runs break on the
/// endpoint as well as the workspace id, because ids and labels are only
/// unique within one endpoint.
pub(super) fn group_agent_items<T>(
    entries: Vec<(AgentGroupSeed, T)>,
    collapsed: CollapsedGroups<'_>,
    member: impl Fn(&T) -> AgentGroupMember<'_>,
) -> Vec<AgentListItem<T>> {
    let mut items = Vec::with_capacity(entries.len() + 4);
    let mut run: Vec<T> = Vec::new();
    let mut current: Option<AgentGroupSeed> = None;
    let flush = |seed: Option<AgentGroupSeed>, run: &mut Vec<T>, items: &mut Vec<_>| {
        let Some(seed) = seed else {
            return;
        };
        let members = run.iter().map(&member).collect::<Vec<_>>();
        let mut counts = Vec::new();
        for status in [
            crate::api::schema::AgentStatus::Blocked,
            crate::api::schema::AgentStatus::Working,
            crate::api::schema::AgentStatus::Done,
            crate::api::schema::AgentStatus::Idle,
        ] {
            let count = members
                .iter()
                .filter(|member| display_status(member.status) == status)
                .count();
            if count > 0 {
                counts.push((status, count));
            }
        }
        let is_collapsed = collapsed.is_collapsed(&seed.endpoint_id, &seed.workspace_id);
        items.push(AgentListItem::Header(AgentGroupHeader {
            collapsed: is_collapsed,
            focused: members.iter().any(|member| member.focused),
            pane_ids: members
                .iter()
                .map(|member| member.pane_id.to_owned())
                .collect(),
            endpoint_id: seed.endpoint_id,
            workspace_id: seed.workspace_id,
            label: seed.label,
            counts,
            stale: seed.stale,
        }));
        if is_collapsed {
            run.clear();
        } else {
            items.extend(run.drain(..).map(AgentListItem::Row));
        }
    };
    for (seed, row) in entries {
        let same_group = current.as_ref().is_some_and(|c| {
            c.endpoint_id == seed.endpoint_id && c.workspace_id == seed.workspace_id
        });
        if !same_group {
            flush(current.take(), &mut run, &mut items);
            current = Some(seed);
        }
        run.push(row);
    }
    flush(current.take(), &mut run, &mut items);
    items
}

/// Index of the list item that shows `pane_id`: its own row, or the header of
/// its group when the group is collapsed.
pub(super) fn reveal_index<T>(
    items: &[AgentListItem<T>],
    endpoint_id: &ClientEndpointId,
    pane_id: &str,
    is_target_row: impl Fn(&T) -> bool,
) -> Option<usize> {
    items
        .iter()
        .position(|item| matches!(item, AgentListItem::Row(row) if is_target_row(row)))
        .or_else(|| {
            items.iter().position(|item| {
                matches!(item, AgentListItem::Header(header)
                    if &header.endpoint_id == endpoint_id
                        && header.pane_ids.iter().any(|id| id == pane_id))
            })
        })
}

/// Line heights and trailing gaps of the list items. Headers never take the
/// configured row gap, so a header stays attached to its first agent.
pub(super) fn item_layout<T>(
    items: &[AgentListItem<T>],
    row_lines: impl Fn(&T) -> usize,
    row_gap: u16,
) -> (Vec<u16>, Vec<u16>) {
    let heights = items
        .iter()
        .map(|item| match item {
            AgentListItem::Header(_) => 1,
            AgentListItem::Row(row) => row_lines(row).max(1).min(u16::MAX as usize) as u16,
        })
        .collect();
    let gaps = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if index + 1 < items.len() && matches!(item, AgentListItem::Row(_)) {
                row_gap
            } else {
                0
            }
        })
        .collect();
    (heights, gaps)
}

/// Step to the next or previous visible agent, wrapping. Agents in collapsed
/// groups are skipped, but stepping can start from one (focused elsewhere).
/// When every agent is hidden the list is walked unfiltered, so the binding
/// never silently does nothing.
pub(super) fn step_visible_agent<T>(
    targets: &[T],
    is_visible: impl Fn(&T) -> bool,
    current: Option<usize>,
    reverse: bool,
) -> Option<usize> {
    let count = targets.len();
    if count == 0 {
        return None;
    }
    let start = current.unwrap_or(if reverse { 0 } else { count - 1 });
    let step_to = |step: usize| {
        if reverse {
            (start + count - step) % count
        } else {
            (start + step) % count
        }
    };
    (1..=count)
        .map(step_to)
        .find(|index| is_visible(&targets[*index]))
        .or_else(|| Some(step_to(1)))
}

pub(super) fn render_group_header(
    buffer: &mut Buffer,
    rect: Rect,
    header: &AgentGroupHeader,
    config: &ClientShellConfig,
) {
    use crate::api::schema::AgentStatus;

    let palette = &config.palette;
    let blocked = header.collapsed
        && header
            .counts
            .iter()
            .any(|(status, _)| *status == AgentStatus::Blocked);
    let mut label_style = Style::default()
        .fg(if blocked {
            // A hidden blocked agent needs attention more than focus does.
            status_color(AgentStatus::Blocked, palette)
        } else if header.focused {
            palette.text
        } else {
            palette.overlay0
        })
        .add_modifier(Modifier::BOLD);
    if header.focused {
        buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        label_style = label_style.bg(palette.active_row_bg);
    }
    let width = rect.width as usize;
    // Right-aligned per-state counts, e.g. "1● 2●"; dropped before the label is
    // squeezed below a readable width.
    let mut counts = Vec::new();
    let mut counts_width = 0;
    for (index, (status, count)) in header.counts.iter().enumerate() {
        let text = format!(
            "{}{count}{}",
            if index > 0 { " " } else { "" },
            status_icon(*status, config.status_indicators)
        );
        counts_width += text.chars().count();
        counts.push((text, *status));
    }
    // " ▾ " precedes the label.
    let prefix_width = 3;
    let show_counts = width >= prefix_width + counts_width + 1 + 4;
    let label_width =
        width.saturating_sub(prefix_width + if show_counts { counts_width + 1 } else { 0 });
    let label = crate::ui::truncate_end(&header.label, label_width);
    let glyph = if header.collapsed { '▸' } else { '▾' };
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        &format!(" {glyph} {label}"),
        label_style,
    );
    if show_counts {
        let mut x = rect.right().saturating_sub(counts_width as u16);
        for (text, status) in counts {
            let text_width = text.chars().count() as u16;
            let mut style = Style::default().fg(status_color(status, palette));
            if header.focused {
                style = style.bg(palette.active_row_bg);
            }
            put_text(buffer, x, rect.y, text_width, &text, style);
            x = x.saturating_add(text_width);
        }
    }
    if header.stale {
        buffer.set_style(
            rect,
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        );
    }
}

impl ClientShellState {
    pub(super) fn collapsed_group_sets(&self) -> CollapsedGroups<'_> {
        CollapsedGroups {
            local: &self.collapsed_groups,
            remote: &self.remote_collapsed_groups,
        }
    }

    /// Whether the agent list currently shows group headers.
    pub(super) fn agent_groups_active(&self) -> bool {
        group_headers_enabled(
            &self.config,
            self.snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.agent_view_label.is_some()),
        )
    }

    /// Whether the agent's group is collapsed, so the row is not listed.
    pub(super) fn agent_group_hides(
        &self,
        endpoint_id: &ClientEndpointId,
        workspace_id: &str,
    ) -> bool {
        self.agent_groups_active()
            && self
                .collapsed_group_sets()
                .is_collapsed(endpoint_id, workspace_id)
    }

    fn toggle_agent_group(&mut self, endpoint_id: &ClientEndpointId, workspace_id: &str) {
        self.toggle_collapsed_group(endpoint_id, agent_group_key(workspace_id));
    }

    /// Collapse or expand the group of `workspace_id` and persist the choice.
    pub(super) fn toggle_agent_group_and_persist(
        &mut self,
        endpoint_id: &ClientEndpointId,
        workspace_id: &str,
        outcome: &mut ClientShellInput,
    ) {
        self.toggle_agent_group(endpoint_id, workspace_id);
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }

    /// Collapse every workspace group that has agents, or expand all of them.
    pub(super) fn set_all_agent_groups(&mut self, collapse: bool, outcome: &mut ClientShellInput) {
        if collapse {
            let groups = self
                .endpoints
                .iter()
                .filter_map(|endpoint| {
                    endpoint.snapshot.as_deref().map(|snapshot| {
                        snapshot
                            .agents
                            .iter()
                            .map(|agent| agent.workspace_id.clone())
                            .collect::<HashSet<_>>()
                            .into_iter()
                            .map(|workspace_id| (endpoint.endpoint_id.clone(), workspace_id))
                            .collect::<Vec<_>>()
                    })
                })
                .flatten()
                .collect::<Vec<_>>();
            for (endpoint_id, workspace_id) in groups {
                let key = agent_group_key(&workspace_id);
                if endpoint_id.is_local() {
                    self.collapsed_groups.insert(key);
                } else {
                    self.remote_collapsed_groups
                        .entry(endpoint_id)
                        .or_default()
                        .insert(key);
                }
            }
        } else {
            self.collapsed_groups
                .retain(|key| !key.starts_with(GROUP_KEY_PREFIX));
            for groups in self.remote_collapsed_groups.values_mut() {
                groups.retain(|key| !key.starts_with(GROUP_KEY_PREFIX));
            }
        }
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }

    /// The group of the focused agent on the active endpoint.
    fn focused_agent_group(&self) -> Option<(ClientEndpointId, String)> {
        let snapshot = self.snapshot.as_deref()?;
        let pane_id = snapshot.focused_pane_id.as_deref()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)?;
        Some((self.active_endpoint_id.clone(), agent.workspace_id.clone()))
    }

    /// Keyboard actions for the agent list groups. Bindings act on the group of
    /// the focused agent because the list has no separate selection cursor.
    pub(super) fn apply_agent_group_action(
        &mut self,
        action: crate::input::KeybindAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::KeybindAction;

        if !self.agent_groups_active() {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "agent_groups_inactive",
                "Agent groups are off",
                "Groups show with agent_panel_sort = \"spaces\" and ui.sidebar.group_agents_by_workspace",
            );
            outcome.repaint = true;
            return;
        }
        match action {
            KeybindAction::ToggleAgentGroup => {
                if let Some((endpoint_id, workspace_id)) = self.focused_agent_group() {
                    self.toggle_agent_group_and_persist(&endpoint_id, &workspace_id, outcome);
                }
            }
            KeybindAction::CollapseAgentGroups => self.set_all_agent_groups(true, outcome),
            KeybindAction::ExpandAgentGroups => self.set_all_agent_groups(false, outcome),
            _ => {}
        }
    }

    /// Left click on a group header toggles the group. A double click toggles
    /// twice, which restores the state, and opens the workspace rename prompt.
    pub(super) fn handle_agent_header_click(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some((endpoint_id, workspace_id)) = self
            .hits
            .agent_headers
            .iter()
            .find(|hit| super::contains(hit.rect, point))
            .map(|hit| (hit.endpoint_id.clone(), hit.workspace_id.clone()))
        else {
            return false;
        };
        let double = self.register_agent_list_click(ClientAgentListTarget::Group {
            endpoint_id: endpoint_id.clone(),
            workspace_id: workspace_id.clone(),
        });
        self.toggle_agent_group_and_persist(&endpoint_id, &workspace_id, outcome);
        if double && endpoint_id == self.active_endpoint_id {
            self.open_rename_workspace_overlay_for(workspace_id);
        }
        true
    }

    pub(super) fn open_agent_group_context_menu(&mut self, point: (u16, u16)) -> bool {
        let Some(hit) = self
            .hits
            .agent_headers
            .iter()
            .find(|hit| super::contains(hit.rect, point))
        else {
            return false;
        };
        let target = ClientContextMenuTarget::AgentGroup {
            can_rename: hit.endpoint_id == self.active_endpoint_id,
            endpoint_id: hit.endpoint_id.clone(),
            workspace_id: hit.workspace_id.clone(),
            collapsed: hit.collapsed,
        };
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target,
            x: point.0,
            y: point.1,
            highlighted: 0,
        }));
        true
    }

    /// Scroll the single-endpoint agent list so `pane_id` (or its collapsed
    /// group header) is visible.
    pub(super) fn reveal_snapshot_agent(&mut self, pane_id: &str) {
        let body_height = self.hits.agent_body.height;
        if body_height == 0 {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let items = super::agent_sidebar::agent_items(
            snapshot,
            &self.config,
            &self.active_endpoint_id,
            self.collapsed_group_sets(),
            None,
        );
        let Some(target) = reveal_index(&items, &self.active_endpoint_id, pane_id, |row| {
            row.pane_id == pane_id
        }) else {
            return;
        };
        let (heights, gaps) = item_layout(&items, |row| row.rows.len(), self.config.agents.row_gap);
        self.agent_scroll = super::scroll::list_scroll_start_to_reveal(
            &heights,
            &gaps,
            body_height,
            self.agent_scroll,
            target,
        );
    }
}
