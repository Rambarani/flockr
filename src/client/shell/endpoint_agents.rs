use super::agent_groups::{
    group_agent_items, group_headers_enabled, item_layout, reveal_index, ungrouped_items,
    AgentGroupMember, AgentGroupSeed, AgentListItem, CollapsedGroups,
};
use super::render::put_text;
use super::*;

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    for (index, row) in rows.into_iter().take(area.height as usize).enumerate() {
        let rect = Rect::new(area.x, area.y + index as u16, area.width, 1);
        if row.agent.focused {
            buffer.set_style(rect, Style::default().bg(config.palette.active_row_bg));
        }
        let initial = row.machine_label.chars().next().unwrap_or('?');
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(
                "{initial}{}",
                status_icon(row.agent.status, config.status_indicators)
            ),
            Style::default()
                .fg(if row.stale {
                    config.palette.overlay0
                } else {
                    status_color(row.agent.status, &config.palette)
                })
                .add_modifier(if row.stale {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }),
        );
        hits.endpoint_agents
            .push((rect, row.endpoint_id, row.agent.pane_id));
    }
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    collapsed: CollapsedGroups<'_>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !super::agent_sidebar::render_agent_panel_header(
        buffer,
        area,
        agent_view_label,
        config,
        hits,
    ) {
        return;
    }
    let items = agent_items(endpoints, active_endpoint_id, config, collapsed);
    super::agent_sidebar::render_agent_list(
        buffer,
        area,
        &items,
        agent_view_label.map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.agent.rows.len(),
        |buffer, rect, row, hits| {
            super::agent_sidebar::render_agent_row(buffer, rect, &row.agent, config);
            if row.stale {
                buffer.set_style(
                    rect,
                    Style::default()
                        .fg(config.palette.overlay0)
                        .add_modifier(Modifier::DIM),
                );
            }
            hits.endpoint_agents
                .push((rect, row.endpoint_id.clone(), row.agent.pane_id.clone()));
        },
    );
}

impl ClientShellState {
    pub(super) fn reveal_endpoint_agent(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        body_height: u16,
    ) {
        if body_height == 0 {
            return;
        }
        let items = agent_items(
            &self.endpoints,
            &self.active_endpoint_id,
            &self.config,
            self.collapsed_group_sets(),
        );
        let Some(target) = reveal_index(&items, endpoint_id, pane_id, |row| {
            &row.endpoint_id == endpoint_id && row.agent.pane_id == pane_id
        }) else {
            return;
        };
        let (heights, gaps) = item_layout(
            &items,
            |row| row.agent.rows.len(),
            self.config.agents.row_gap,
        );
        self.agent_scroll = super::scroll::list_scroll_start_to_reveal(
            &heights,
            &gaps,
            body_height,
            self.agent_scroll,
            target,
        );
    }
}

struct EndpointAgentRow {
    endpoint_id: ClientEndpointId,
    machine_label: String,
    stale: bool,
    agent: super::agent_sidebar::AgentRow,
}

/// Every online-cached agent in list order with the group it belongs to.
fn agent_entries(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
) -> Vec<(AgentGroupSeed, EndpointAgentRow)> {
    let mut rendered_rows = endpoints
        .iter()
        .filter_map(|endpoint| {
            endpoint.snapshot.as_deref().map(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .filter_map(|agent| {
                        super::agent_sidebar::agent_row(
                            snapshot,
                            &agent.pane_id,
                            config,
                            Some(&endpoint.label),
                        )
                    })
                    .map(|agent| ((endpoint.endpoint_id.clone(), agent.pane_id.clone()), agent))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<HashMap<_, _>>();

    super::aggregate_navigation::aggregate_agent_rows(
        endpoints,
        active_endpoint_id,
        config.agent_panel_sort,
    )
    .into_iter()
    .filter_map(|row| {
        let key = (row.endpoint.endpoint_id.clone(), row.agent.pane_id.clone());
        let mut agent = rendered_rows.remove(&key)?;
        agent.focused &= row.endpoint.endpoint_id == active_endpoint_id;
        let workspace_label = row
            .endpoint
            .snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == row.agent.workspace_id)
            .map(|workspace| workspace.label.as_str())
            .unwrap_or_default();
        // Groups are per machine and workspace: ids and labels are only unique
        // within one endpoint. With several machines the header names both.
        let seed = AgentGroupSeed {
            endpoint_id: row.endpoint.endpoint_id.clone(),
            workspace_id: row.agent.workspace_id.clone(),
            label: if endpoints.len() > 1 {
                format!("{} · {workspace_label}", row.endpoint.label)
            } else {
                workspace_label.to_owned()
            },
            stale: row.endpoint.stale(),
        };
        Some((
            seed,
            EndpointAgentRow {
                endpoint_id: row.endpoint.endpoint_id.clone(),
                machine_label: row.endpoint.label.to_owned(),
                stale: row.endpoint.stale(),
                agent,
            },
        ))
    })
    .collect()
}

/// Flat agent rows in list order, without group headers.
fn agent_rows(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
) -> Vec<EndpointAgentRow> {
    agent_entries(endpoints, active_endpoint_id, config)
        .into_iter()
        .map(|(_, row)| row)
        .collect()
}

/// The combined agent list: rows under per-machine workspace headers when
/// grouping applies (collapsed groups keep only their header).
fn agent_items(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    collapsed: CollapsedGroups<'_>,
) -> Vec<AgentListItem<EndpointAgentRow>> {
    let entries = agent_entries(endpoints, active_endpoint_id, config);
    let agent_view_active = endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == active_endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .is_some_and(|snapshot| snapshot.agent_view_label.is_some());
    if !group_headers_enabled(config, agent_view_active) {
        return ungrouped_items(entries.into_iter().map(|(_, row)| row).collect());
    }
    group_agent_items(entries, collapsed, |row| AgentGroupMember {
        status: row.agent.status,
        focused: row.agent.focused,
        pane_id: &row.agent.pane_id,
    })
}
