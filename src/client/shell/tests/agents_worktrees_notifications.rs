// Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE.
use super::*;

#[test]
fn mouse_hits_use_stable_workspace_tab_and_pane_ids() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");

    let workspace_down =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace_down.actions.is_empty());
    let workspace =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(workspace.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &workspace.actions[..] else {
        panic!("workspace click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_1"
    ));

    let pane = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 27,
        row: 1,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(pane.requests.is_empty());
    let [ClientShellAction::Endpoint { request, .. }] = &pane.actions[..] else {
        panic!("pane click should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));
}

#[test]
fn collapsed_workspace_jitter_remains_a_click() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("collapsed sidebar");
    let workspace = state.hits.workspaces[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: workspace.x,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: workspace.x + 1,
        row: workspace.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());
    assert!(state.workspace_press.is_some());
    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: workspace.x + 1,
            row: workspace.y,
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn grouped_worktrees_render_parent_branch_and_indented_child() {
    let config = ClientShellConfig::from_config(&Config::default());
    let mut state = ClientShellState::new(config);
    let mut snapshot = snapshot();
    snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    snapshot.workspaces.push(ClientShellWorkspace {
        workspace_id: "ws_2".into(),
        active_tab_id: "tab_ws2".into(),
        new_workspace_cwd: "/repo/feature".into(),
        number: 2,
        label: "repo-feature".into(),
        custom_label: false,
        branch: Some("worktree/feature".into()),
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        }),
        focused: false,
        agent_status: AgentStatus::Idle,
    });
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 20).expect("composed frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("main"));
    assert!(text.contains("└─"));
    assert!(text.contains("feature"));

    let mut replacement = (**state.snapshot.as_ref().expect("snapshot")).clone();
    replacement.revision = 2;
    replacement.focused_workspace_id = Some("ws_2".into());
    replacement.workspaces[0].focused = false;
    replacement.workspaces[1].focused = true;
    replacement.workspaces[1].agent_status = AgentStatus::Blocked;
    let mut replacement_surface = surface();
    replacement_surface.projection_revision = 2;
    state.collapsed_groups.insert("repo".into());
    state.set_snapshot(Box::new(replacement));
    state.set_pane_surface(replacement_surface);
    let collapsed = state.compose(106, 20).expect("collapsed worktree group");
    let parent = state.hits.workspaces[0].rect;
    let status_cell = usize::from(parent.y) * usize::from(collapsed.width)
        + usize::from(parent.x.saturating_add(1));
    assert_eq!(
        collapsed.cells[status_cell].fg,
        crate::protocol::color_to_u32(state.config.palette.red)
    );

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextWorkspace),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_1"
            )
    ));
}

#[test]
fn duplicate_repo_parents_remain_visible_and_focusable_when_collapsed() {
    for linked_child in [false, true] {
        let mut projected = snapshot();
        projected.workspaces[0].agent_status = AgentStatus::Idle;
        projected.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo".into(),
            label: "repo".into(),
            is_linked_worktree: false,
        });
        let mut duplicate = projected.workspaces[0].clone();
        duplicate.workspace_id = "ws_duplicate".into();
        duplicate.label = "duplicate-repo".into();
        duplicate.focused = false;
        duplicate.agent_status = AgentStatus::Blocked;
        projected.workspaces.push(duplicate);
        if linked_child {
            let mut child = projected.workspaces[0].clone();
            child.workspace_id = "ws_child".into();
            child.focused = false;
            child.worktree.as_mut().unwrap().is_linked_worktree = true;
            projected.workspaces.push(child);
        }

        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(projected));
        state.set_pane_surface(surface());
        for collapsed in [false, true] {
            if collapsed {
                state.collapsed_groups.insert("repo".into());
            }
            let frame = state
                .compose(106, 24)
                .expect("duplicate repository parents");
            for workspace_id in ["ws_1", "ws_duplicate"] {
                let hit = state
                    .hits
                    .workspaces
                    .iter()
                    .find(|hit| hit.workspace_id == workspace_id)
                    .expect("repository parent remains visible");
                assert!(!hit.indented);
                assert_eq!(hit.group_toggle.is_some(), linked_child);
            }
            let parent = state.hits.workspaces[0].rect;
            let status_cell =
                usize::from(parent.y) * usize::from(frame.width) + usize::from(parent.x + 1);
            assert_ne!(
                frame.cells[status_cell].fg,
                crate::protocol::color_to_u32(state.config.palette.red)
            );
            assert_eq!(
                state
                    .hits
                    .workspaces
                    .iter()
                    .any(|hit| hit.workspace_id == "ws_child" && hit.indented),
                linked_child && !collapsed
            );
            let duplicate = state.hits.workspaces[1].rect;
            state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: duplicate.x + 2,
                row: duplicate.y,
                modifiers: KeyModifiers::empty(),
            })]);
            let click = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: duplicate.x + 2,
                row: duplicate.y,
                modifiers: KeyModifiers::empty(),
            })]);
            assert!(matches!(click.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(target)
                        if target.workspace_id == "ws_duplicate")));
        }
        let original = state.snapshot.as_deref().unwrap().clone();
        for confirm_close in [false, true] {
            let mut restored = original.clone();
            restored.revision = state.snapshot.as_deref().unwrap().revision + 1;
            state.set_snapshot(Box::new(restored));
            state.config.confirm_close = confirm_close;
            state.open_workspace_context_menu("ws_duplicate".into(), 0, 0);
            let Some(ClientShellOverlay::ContextMenu(menu)) = state.overlay.as_ref() else {
                panic!("repository workspace context menu");
            };
            assert_eq!(menu.items()[1].label, "Close");
            assert_eq!(
                menu.items()
                    .iter()
                    .any(|item| matches!(item.action, ClientContextMenuAction::ToggleGroup)),
                linked_child
            );
            if linked_child {
                let mut replacement = original.clone();
                replacement.revision = state.snapshot.as_deref().unwrap().revision + 1;
                replacement
                    .workspaces
                    .retain(|workspace| workspace.workspace_id != "ws_1");
                state.set_snapshot(Box::new(replacement));
            }
            let mut close = ClientShellInput::default();
            state.activate_context_menu_item(1, &mut close);
            if confirm_close {
                assert!(close.actions.is_empty());
                assert!(matches!(state.overlay.as_ref(),
                    Some(ClientShellOverlay::ConfirmClose(confirm))
                        if confirm.title == "Close workspace?" && !confirm.close_group));
                close = state.handle_input_bytes(b"\r");
            }
            assert!(matches!(close.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, crate::api::schema::Method::WorkspaceClose(params)
                        if params.workspace_id == "ws_duplicate" && !params.close_group)));
        }
    }
}

#[test]
fn workspace_click_waits_for_release_and_drag_reorders_by_stable_id() {
    let mut projected = snapshot();
    for index in 2..=3 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("three workspaces");
    let first = state.hits.workspaces[0].rect;
    let third = state.hits.workspaces[2].rect;

    let down = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x + 2,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(down.actions.is_empty());
    let drag = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: third.x + 2,
        row: third.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drag.repaint);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::Workspace {
            ref source_workspace_id,
            target: Some((None, _)),
        }) if source_workspace_id == "ws_1"
    ));
    let frame = state.compose(106, 24).expect("workspace drop indicator");
    assert!(frame
        .cells
        .chunks(frame.width as usize)
        .any(|row| row.iter().take(20).any(|cell| cell.symbol == "─")));

    let release =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: third.x + 2,
            row: third.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(matches!(
        &release.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_1" && params.insert_index == 3
            )
    ));

    state.compose(106, 24).expect("workspaces after drag");
    let second = state.hits.workspaces[1].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: second.x + 2,
        row: second.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &click.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceFocus(target)
                    if target.workspace_id == "ws_2"
            )
    ));
}

#[test]
fn duplicate_repo_parent_drag_does_not_target_its_own_move_block() {
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut other = projected.workspaces[0].clone();
    other.workspace_id = "ws_other".into();
    other.worktree = None;
    other.focused = false;
    let mut duplicate = projected.workspaces[0].clone();
    duplicate.workspace_id = "ws_duplicate".into();
    duplicate.focused = false;
    projected.workspaces.extend([other, duplicate]);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
        .compose(106, 24)
        .expect("separated repository parents");
    let source = state.hits.workspaces[0].rect;
    let target = state.hits.workspaces[2].rect;
    for (kind, rect, row) in [
        (MouseEventKind::Down(MouseButton::Left), source, source.y),
        (
            MouseEventKind::Drag(MouseButton::Left),
            target,
            target.y.saturating_sub(1),
        ),
    ] {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind,
            column: rect.x + 2,
            row,
            modifiers: KeyModifiers::empty(),
        })]);
    }
    assert!(
        matches!(state.chrome_drag.as_ref(), Some(ClientChromeDrag::Workspace {
        target: Some((Some(workspace_id), _)), ..
    }) if workspace_id == "ws_duplicate")
    );
    let drop = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: target.x + 2,
        row: target.y.saturating_sub(1),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(drop.actions.is_empty());
}

#[test]
fn workspace_drag_moves_parent_worktree_as_one_block_and_rejects_child() {
    let mut projected = snapshot();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    let mut child = projected.workspaces[0].clone();
    child.workspace_id = "ws_child".into();
    child.number = 2;
    child.label = "feature".into();
    child.focused = false;
    child.worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut other = projected.workspaces[0].clone();
    other.workspace_id = "ws_other".into();
    other.number = 3;
    other.label = "other".into();
    other.focused = false;
    other.worktree = None;
    projected.workspaces.extend([child, other]);

    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("worktree workspaces");
    assert!(state.hits.workspaces[1].indented);
    let parent = state.hits.workspaces[0].rect;
    let child = state.hits.workspaces[1].rect;
    let other = state.hits.workspaces[2].rect;

    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: parent.x + 2,
        row: parent.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: other.x + 2,
        row: other.bottom(),
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        &moved.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMoveBlock(params)
                    if params.workspace_ids == ["ws_1", "ws_child"]
                        && params.before_workspace_id.is_none()
            )
    ));

    state.compose(106, 24).expect("worktree child");
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: child.x + 2,
        row: child.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let dragging_child =
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: other.x + 2,
            row: other.bottom(),
            modifiers: KeyModifiers::empty(),
        })]);
    assert!(dragging_child.actions.is_empty());
    assert!(state.chrome_drag.is_none());

    let mut projected = state.snapshot.as_deref().unwrap().clone();
    let mut duplicate = projected.workspaces[0].clone();
    duplicate.workspace_id = "ws_duplicate".into();
    duplicate.focused = false;
    projected.workspaces.push(duplicate);
    state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state
        .compose(106, 24)
        .expect("repository block with duplicate root");
    let duplicate = state.hits.workspaces[1].rect;
    let other = state.hits.workspaces[3].rect;
    for (source, target_row, moving_duplicate) in [
        (other, duplicate.y.saturating_sub(1), false),
        (duplicate, other.bottom(), true),
    ] {
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: source.x + 2,
            row: source.y,
            modifiers: KeyModifiers::empty(),
        })]);
        state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: source.x + 2,
            row: target_row,
            modifiers: KeyModifiers::empty(),
        })]);
        let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: source.x + 2,
            row: target_row,
            modifiers: KeyModifiers::empty(),
        })]);
        if moving_duplicate {
            assert!(matches!(moved.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, crate::api::schema::Method::WorkspaceMoveBlock(params)
                        if params.workspace_ids == ["ws_duplicate", "ws_1", "ws_child"] && params.before_workspace_id.is_none())));
        } else {
            assert!(matches!(moved.actions.as_slice(),
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, crate::api::schema::Method::WorkspaceMove(params)
                        if params.workspace_id == "ws_other" && params.insert_index == 0)));
        }
    }
}

#[test]
fn pane_cycle_last_and_agent_actions_resolve_to_stable_pane_ids() {
    let mut initial = snapshot();
    let mut second = initial.panes[0].clone();
    second.pane_id = "pane_2".into();
    second.focused = false;
    initial.panes.push(second);
    initial.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(initial.clone()));

    let mut cycle = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePaneNext),
        &mut cycle,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &cycle.actions[..] else {
        panic!("pane cycle should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    let mut replacement = initial;
    replacement.revision = 2;
    replacement.focused_pane_id = Some("pane_2".into());
    replacement.panes[0].focused = false;
    replacement.panes[1].focused = true;
    state.set_snapshot(Box::new(replacement));
    let mut last = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::LastPane),
        &mut last,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &last.actions[..] else {
        panic!("last pane should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_1"
    ));

    let mut agent = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(1)),
        &mut agent,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &agent.actions[..] else {
        panic!("agent focus should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));
}

#[test]
fn agent_sidebar_honors_priority_symbols_tokens_and_stable_hits() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi one".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("first title".into()),
            terminal_title_stripped: Some("first".into()),
            agent_status: AgentStatus::Done,
            state_change_seq: 10,
            state_labels: Vec::new(),
            tokens: vec![("summary".into(), "review complete".into())],
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("pi two".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: Some("second title".into()),
            terminal_title_stripped: Some("second".into()),
            agent_status: AgentStatus::Blocked,
            state_change_seq: 20,
            state_labels: vec![("blocked".into(), "needs input".into())],
            tokens: vec![("summary".into(), "waiting for Can".into())],
            focused: false,
        },
    ];
    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    config.ui.status_indicators = crate::config::StatusIndicatorStyle::Symbols;
    config.ui.sidebar.agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];
    config.ui.sidebar.agents.rows_by_agent.insert(
        "pi".into(),
        vec![
            vec![
                crate::config::AgentSidebarToken::StateIcon,
                crate::config::AgentSidebarToken::StateText,
            ],
            vec![
                crate::config::AgentSidebarToken::Agent,
                crate::config::AgentSidebarToken::Custom("summary".into()),
            ],
        ],
    );
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());

    let frame = state.compose(106, 30).expect("agent sidebar frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("× needs input"), "frame: {text}");
    assert!(text.contains("pi two"), "frame: {text}");
    assert!(text.contains("waiting for"), "frame: {text}");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );

    let first = state.hits.agents[0].0;
    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: first.x,
        row: first.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &click.actions[..] else {
        panic!("agent row should focus through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    state.compose(106, 10).expect("short agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_2")
    );
    let body = state.hits.agent_body;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: body.x,
        row: body.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state
        .compose(106, 10)
        .expect("scrolled agent sidebar frame");
    assert_eq!(
        state
            .hits
            .agents
            .first()
            .map(|(_, pane_id)| pane_id.as_str()),
        Some("pane_1")
    );

    state.sidebar_collapsed = true;
    let compact = state.compose(106, 30).expect("compact agent sidebar frame");
    let blocked = state
        .hits
        .agents
        .iter()
        .find(|(_, pane_id)| pane_id == "pane_2")
        .expect("blocked compact agent")
        .0;
    let row_start = blocked.y as usize * compact.width as usize + blocked.x as usize;
    assert_ne!(compact.cells[row_start].fg, compact.cells[row_start + 2].fg);
    assert_eq!(compact.cells[row_start].bg, compact.cells[row_start + 2].bg);
}

fn ungrouped_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar.group_agents_by_workspace = false;
    config
}

#[test]
fn muted_agent_sidebar_rows_do_not_stack_terminal_faint() {
    let mut projected = snapshot();
    projected.tabs[0].label = "second".into();
    projected.tabs[0].custom_label = true;
    projected.agents = vec![ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("reviewer".into()),
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    }];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&ungrouped_config()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("agent sidebar frame");
    let row = state.hits.agents.first().expect("agent row hit").0;
    let buffer = frame.to_ratatui_buffer().expect("agent sidebar buffer");

    for (label, needle) in [
        ("label", "second"),
        ("agent kind", "reviewer"),
        ("separator", "·"),
    ] {
        let (x, y) = cell_symbol_position(&frame, row, needle);
        let cell = buffer.cell((x, y)).expect("muted sidebar cell");
        assert!(
            !cell.modifier.contains(Modifier::DIM),
            "{label} cell at ({x},{y}) should not stack terminal faint: {cell:?}"
        );
    }
}

#[test]
fn workspace_state_text_does_not_stack_terminal_faint() {
    use crate::config::SpaceSidebarToken;

    let mut config = Config::default();
    config.ui.sidebar.spaces.rows = vec![
        vec![SpaceSidebarToken::StateIcon, SpaceSidebarToken::Workspace],
        vec![SpaceSidebarToken::StateText],
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("workspace sidebar frame");
    let rect = state.hits.workspaces.first().expect("workspace hit").rect;
    let buffer = frame.to_ratatui_buffer().expect("workspace sidebar buffer");
    let (x, y) = cell_symbol_position(&frame, rect, "idle");
    let cell = buffer.cell((x, y)).expect("workspace state text cell");
    assert!(
        !cell.modifier.contains(Modifier::DIM),
        "workspace state text at ({x},{y}) should not stack terminal faint: {cell:?}"
    );
}

#[test]
fn active_agent_view_controls_sidebar_order_and_focus_indices() {
    let mut projected = snapshot();
    let mut second_pane = projected.panes[0].clone();
    second_pane.pane_id = "pane_2".into();
    second_pane.focused = false;
    projected.panes.push(second_pane.clone());
    let mut third_pane = second_pane;
    third_pane.pane_id = "pane_3".into();
    projected.panes.push(third_pane);
    projected.agents = vec![
        ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("first".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        },
        ClientShellAgent {
            pane_id: "pane_2".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("second".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Blocked,
            state_change_seq: 2,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
        ClientShellAgent {
            pane_id: "pane_3".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("third".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 3,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        },
    ];
    projected.agent_view_label = Some("review".into());
    projected.agent_order = vec!["pane_2".into(), "pane_3".into()];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("filtered agent sidebar");
    assert_eq!(
        state
            .hits
            .agents
            .iter()
            .map(|(_, pane_id)| pane_id.as_str())
            .collect::<Vec<_>>(),
        vec!["pane_2", "pane_3"]
    );
    assert_eq!(state.hits.agent_sort_toggle, Rect::default());

    let mut focus = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::FocusAgent(0)),
        &mut focus,
    );
    assert!(matches!(
        &focus.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NextAgent),
        &mut next,
    );
    assert!(matches!(
        &next.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
            )
    ));
}

#[test]
fn agent_sort_toggle_is_client_local_and_persists_per_endpoint() {
    let path = std::env::temp_dir().join(format!(
        "flockr-shell-agent-sort-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("pi".into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Working,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("agent sidebar frame");
    let toggle = state.hits.agent_sort_toggle;

    let click = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: toggle.x,
        row: toggle.y,
        modifiers: KeyModifiers::empty(),
    })]);

    assert_eq!(
        state.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(click.actions.is_empty());
    let reloaded_config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let reloaded = ClientShellState::new(reloaded_config);
    assert_eq!(
        reloaded.config.agent_panel_sort,
        crate::config::AgentPanelSortConfig::Priority
    );
    assert!(reloaded.agent_panel_sort_manual);
    std::fs::remove_file(path).expect("remove agent sort preferences");
}

#[test]
fn workspace_actions_preserve_selected_target_and_client_confirmation() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_2");

    let rename = state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        KeyCode::Char('w'),
        KeyModifiers::SHIFT,
    ))]);
    assert!(rename.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_2"
    ));
    assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
    assert!(state.handle_input_bytes(b"renamed").actions.is_empty());
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("workspace rename should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceRename(params)
            if params.workspace_id == "ws_2" && params.label == "renamed"
    ));

    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_2");
    let mut close = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CloseWorkspace),
        &mut close,
    );
    assert!(close.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::ConfirmClose(ClientConfirmCloseOverlay {
            workspace_id,
            ..
        })) if workspace_id == "ws_2"
    ));
    let confirm = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &confirm.actions[..] else {
        panic!("workspace confirmation should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceClose(params)
            if params.workspace_id == "ws_2" && !params.close_group
    ));
}

#[test]
fn desktop_workspace_navigation_reveals_overflowing_selection() {
    let mut projected = snapshot();
    for index in 2..=8 {
        let mut workspace = projected.workspaces[0].clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index;
        workspace.label = format!("workspace-{index}");
        workspace.focused = false;
        projected.workspaces.push(workspace);
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.mode = ClientShellMode::Navigate;
    state.navigate_workspace_id = state.navigation_target(&ClientEndpointId::Local, "ws_1");
    state.compose(106, 12).expect("overflowing sidebar");

    for _ in 0..6 {
        state.handle_input_bytes(b"\x1b[B");
        state.compose(106, 12).expect("revealed workspace");
        let selected = state
            .navigate_workspace_id
            .as_ref()
            .expect("selection")
            .workspace_id
            .as_str();
        assert!(
            state
                .hits
                .workspaces
                .iter()
                .any(|hit| hit.workspace_id == selected),
            "{selected} should remain visible"
        );
    }
}

#[test]
fn named_workspace_overlay_targets_projected_source_workspace() {
    let mut config = Config::default();
    config.ui.prompt_new_workspace_name = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot()));
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorkspace),
        &mut open,
    );
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            input: value,
            target: ClientRenameTarget::NewWorkspace {
                source_workspace_id,
                ..
            },
            ..
        })) if value.as_str() == "repo" && source_workspace_id.as_deref() == Some("ws_1")
    ));
    let create = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &create.actions[..] else {
        panic!("named workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceCreate(params)
            if params.source_workspace_id.as_deref() == Some("ws_1")
                && params.cwd.as_deref() == Some("/repo")
                && params.label.is_none()
    ));
}

#[test]
fn navigate_mode_selects_workspace_locally_then_focuses_by_stable_id() {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());

    assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
    let enter_navigate = state.handle_input_bytes(b"w");
    assert!(enter_navigate.repaint);
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_1")
    );

    let invalid = state.handle_input_bytes(b"9");
    assert!(invalid.actions.is_empty());
    assert_eq!(state.mode, ClientShellMode::Navigate);
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_1")
    );

    let move_selection = state.handle_input_bytes(b"\x1b[B");
    assert!(move_selection.actions.is_empty());
    assert_eq!(
        state.navigate_workspace_id,
        state.navigation_target(&ClientEndpointId::Local, "ws_2")
    );
    let frame = state.compose(106, 20).expect("navigate frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("second"));
    assert!(text.contains("NAVIGATE"));

    let focus = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &focus.actions[..] else {
        panic!("selected workspace should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceFocus(target)
            if target.workspace_id == "ws_2"
    ));
    assert_eq!(state.mode, ClientShellMode::Terminal);
}

#[test]
fn worktree_create_previews_the_endpoint_owned_checkout_path() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeList(params)
            if params.workspace_id.as_deref() == Some("ws_1")
    ));
    let request_id = request.id.clone();
    assert!(
        state
            .handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)))
            .0
    );
    let frame = state.compose(106, 30).expect("new worktree modal");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("new worktree"));
    assert!(text.contains("create and open"));
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));

    assert!(state
        .handle_input_bytes(b"feature/client-shell")
        .actions
        .is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create))
            if create.checkout_path
                == "/tmp/flockr-worktrees/repo/feature-client-shell"
    ));
    let submit = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &submit.actions[..] else {
        panic!("worktree create should use endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeCreate(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.branch.as_deref() == Some("feature/client-shell")
                && params.path.is_none()
                && !params.focus
    ));
}

#[test]
fn unavailable_worktree_create_does_not_wedge_the_overlay() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    state.handle_endpoint_result("boot-1", &request.id, Ok(worktree_list_result(None)));
    state.set_endpoint_methods(Some(vec!["worktree.list".into()]));
    state.handle_input_bytes(b"feature/unavailable");

    let submit = state.handle_input_bytes(b"\r");

    assert!(submit.actions.is_empty());
    assert!(matches!(
        &state.overlay,
        Some(ClientShellOverlay::WorktreeCreate(create)) if !create.creating
    ));
    assert!(state
        .visible_endpoint_notice
        .as_ref()
        .is_some_and(|notice| notice.key.code == "worktree.create"));
}

#[test]
fn worktree_action_errors_expire_without_more_input() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());

    let mut guard = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
        &mut guard,
    );
    let message = "This workspace is not a Flockr-managed worktree checkout.";
    assert_eq!(state.endpoint_error.as_deref(), Some(message));

    let deadline = state.endpoint_error_deadline.expect("deadline");
    assert!(!state.tick_endpoint_error(deadline - std::time::Duration::from_secs(1)));
    assert_eq!(state.endpoint_error.as_deref(), Some(message));

    assert!(state.tick_endpoint_error(deadline + std::time::Duration::from_millis(1)));
    assert!(state.endpoint_error.is_none());

    // A repeated identical message must start a fresh lifetime instead of
    // inheriting the earlier deadline.
    let before_repeat = std::time::Instant::now();
    state.set_endpoint_error(message);
    assert!(
        state.endpoint_error_deadline.expect("deadline")
            >= before_repeat + std::time::Duration::from_secs(5)
    );
}

#[test]
fn worktree_prepare_rejection_notice_expires() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::NewWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("new worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Err(ClientShellEndpointError {
            code: Some("not_git_worktree".into()),
            message: "Flockr worktree actions require a workspace inside a Git work tree".into(),
        }),
    );
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("rejection notice");
    assert!(notice.key.code.contains("not_git_worktree"));
    let deadline = notice.deadline;

    let (_, repaint) = state.tick_notifications(deadline + std::time::Duration::from_millis(1));
    assert!(repaint);
    assert!(state.visible_endpoint_notice.is_none());
}

#[test]
fn worktree_open_filters_and_clicks_a_stable_public_entry() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    let mut prepare = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenWorktree),
        &mut prepare,
    );
    let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
        panic!("open worktree should prepare through worktree.list");
    };
    let request_id = request.id.clone();
    state.handle_endpoint_result("boot-1", &request_id, Ok(worktree_list_result(None)));
    let frame = state.compose(106, 30).expect("open worktree modal");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("feature"));
    let row = state.hits.worktree_rows[0].0;
    let open = state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let [ClientShellAction::Endpoint { request, .. }] = &open.actions[..] else {
        panic!("worktree row should open through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorktreeOpen(params)
            if params.workspace_id.as_deref() == Some("ws_1")
                && params.path.as_deref() == Some("/repo-feature")
                && params.focus
    ));
}

#[test]
fn worktree_remove_escalates_recoverable_failure_to_force_confirmation() {
    for (code, message, expect_force) in [
        ("dirty_worktree_requires_force", "dirty worktree", true),
        (
            "worktree_remove_failed",
            "fatal: '/repo-feature' is not a working tree",
            true,
        ),
        ("worktree_remove_failed", "Permission denied", false),
        ("server_unavailable", "is not a working tree", false),
    ] {
        let mut snapshot = snapshot();
        snapshot.workspaces[0].worktree = Some(ClientShellWorktree {
            key: "repo-key".into(),
            label: "repo".into(),
            is_linked_worktree: true,
        });
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(snapshot));
        state.set_pane_surface(surface());
        let mut prepare = ClientShellInput::default();
        state.record_binding(
            crate::input::KeybindMatch::Action(crate::input::KeybindAction::RemoveWorktree),
            &mut prepare,
        );
        let [ClientShellAction::Endpoint { request, .. }] = &prepare.actions[..] else {
            panic!("remove worktree should prepare through worktree.list");
        };
        let request_id = request.id.clone();
        state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Ok(worktree_list_result(Some("ws_1"))),
        );
        let remove = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &remove.actions[..] else {
            panic!("worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && !params.force
        ));
        let request_id = request.id.clone();
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some(code.into()),
                message: message.into(),
            }),
        );
        assert!(actions.is_empty(), "failure must not retry automatically");
        let Some(ClientShellOverlay::WorktreeRemove(remove)) = &state.overlay else {
            panic!("failed remove should keep its confirmation");
        };
        assert!(!remove.removing);
        assert_eq!(remove.force_confirmation, expect_force);
        if !expect_force {
            assert_eq!(remove.error.as_deref(), Some(message));
            continue;
        }
        assert!(remove.error.is_none());
        let frame = state.compose(106, 30).expect("force remove modal");
        let text = frame
            .cells
            .chunks(frame.width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("delete anyway"));
        assert!(text.contains("permanently deleted"));
        let force = state.handle_input_bytes(b"\r");
        let [ClientShellAction::Endpoint { request, .. }] = &force.actions[..] else {
            panic!("forced worktree remove should use endpoint API");
        };
        assert!(matches!(
            &request.method,
            crate::api::schema::Method::WorktreeRemove(params)
                if params.workspace_id == "ws_1" && params.force
        ));
        let request_id = request.id.clone();
        let (_, actions) = state.handle_endpoint_result(
            "boot-1",
            &request_id,
            Err(ClientShellEndpointError {
                code: Some("worktree_remove_failed".into()),
                message: "fatal: '/repo-feature' is not a working tree".into(),
            }),
        );
        assert!(actions.is_empty());
        let Some(ClientShellOverlay::WorktreeRemove(remove)) = &state.overlay else {
            panic!("forced failure should keep its confirmation");
        };
        assert!(!remove.removing);
        assert_eq!(
            remove.error.as_deref(),
            Some("fatal: '/repo-feature' is not a working tree")
        );
        state.handle_input_bytes(b"\x1b");
        assert!(state.overlay.is_none());
    }
}

#[test]
fn semantic_notifications_use_client_policy_and_stable_navigation_targets() {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.toast_delivery = crate::config::ToastDelivery::Flockr;
    config.toast_delay_seconds = 0;
    let mut state = ClientShellState::new(config);
    let mut projected = snapshot();
    projected.agents.push(ClientShellAgent {
        pane_id: "pane_2".into(),
        workspace_id: "ws_2".into(),
        tab_id: "tab_2".into(),
        name: None,
        display_agent: Some("codex".into()),
        agent: Some("codex".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Blocked,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    });
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let now = std::time::Instant::now();
    let (effects, repaint) = state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: Some("other · 2".into()),
            sound: Some(SemanticNotificationSound::Request),
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(matches!(
        effects.as_slice(),
        [ClientShellNotificationEffect::Sound {
            sound: crate::sound::Sound::Request,
            ..
        }]
    ));
    let frame = state.compose(100, 28).expect("notification frame");
    let rendered = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("codex needs attention"));
    let hit = state.hits.notification_toast;
    let click = || {
        RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    state.mode = ClientShellMode::Navigate;
    let ignored = state.handle_raw_events(vec![click()]);
    assert!(ignored.actions.is_empty());
    assert!(state.visible_notification.is_some());

    state.mode = ClientShellMode::Terminal;
    let outcome = state.handle_raw_events(vec![click()]);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "codex needs attention".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    let mut keybind = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::OpenNotificationTarget),
        &mut keybind,
    );
    assert!(keybind.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::PaneFocus(params)
                    if params.pane_id == "pane_2"
            )
    )));
    assert!(state.visible_notification.is_none());

    state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "first".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(state.visible_notification.is_some());
    state.config.toast_delay_seconds = 1;
    let (_, repaint) = state.receive_notification(
        &ClientEndpointId::Local,
        SemanticNotification {
            kind: SemanticNotificationKind::NeedsAttention,
            title: "replacement".into(),
            body: None,
            sound: None,
            agent: Some("codex".into()),
            workspace_id: Some("ws_2".into()),
            tab_id: Some("tab_2".into()),
            pane_id: Some("pane_2".into()),
            position: None,
        },
        now,
    );
    assert!(repaint);
    assert!(state.visible_notification.is_none());
    assert_eq!(state.pending_notifications.len(), 1);
}

pub(super) fn two_workspace_agent_snapshot() -> ClientShellSnapshot {
    let mut projected = snapshot();
    let mut second_workspace = projected.workspaces[0].clone();
    second_workspace.workspace_id = "ws_2".into();
    second_workspace.number = 2;
    second_workspace.label = "api-server".into();
    second_workspace.focused = false;
    projected.workspaces.push(second_workspace);
    let agent = |pane_id: &str, workspace_id: &str, status, focused| ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: "tab_1".into(),
        name: Some(pane_id.into()),
        display_agent: None,
        agent: Some("pi".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused,
    };
    projected.agents = vec![
        agent("pane_1", "ws_1", AgentStatus::Working, true),
        agent("pane_2", "ws_1", AgentStatus::Blocked, false),
        agent("pane_3", "ws_2", AgentStatus::Idle, false),
    ];
    projected
}

fn sidebar_agent_state(config: &Config) -> (ClientShellState, FrameData) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("agent sidebar frame").frame;
    (state, frame)
}

#[test]
fn agent_sidebar_groups_agents_under_workspace_headers() {
    let (state, frame) = sidebar_agent_state(&Config::default());
    let rows = frame_rows(&frame);
    let body = state.hits.agent_body;
    let header_rows = |label: &str| {
        (body.y..body.bottom())
            .filter(|y| {
                rows[*y as usize]
                    .chars()
                    .skip(body.x as usize)
                    .take(body.width as usize)
                    .collect::<String>()
                    .starts_with(&format!(" ▾ {label}"))
            })
            .collect::<Vec<_>>()
    };
    let first = header_rows("client-shell");
    let second = header_rows("api-server");
    assert_eq!(first, vec![body.y], "first group header opens the list");
    assert_eq!(second.len(), 1, "one header per workspace");

    // Counts summarise the group's states: one working, one blocked.
    let header_text = rows[first[0] as usize].clone();
    assert!(header_text.contains("1●"), "header: {header_text}");

    // Every agent keeps its own hit rect, none of them on a header row.
    let hits = state
        .hits
        .agents
        .iter()
        .map(|(rect, pane_id)| (pane_id.as_str(), rect.y))
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0], ("pane_1", body.y + 1));
    // Default agent rows are two lines tall, so the second group opens at +5.
    assert_eq!(hits[1], ("pane_2", body.y + 3));
    assert_eq!(second, vec![body.y + 5]);
    assert_eq!(hits[2], ("pane_3", body.y + 6));
    assert!(hits.iter().all(|(_, y)| *y != first[0] && *y != second[0]));
}

/// Sidebar body rows (trimmed of the right-hand border) for the agent list.
fn agent_body_lines(state: &ClientShellState, frame: &FrameData) -> Vec<String> {
    let body = state.hits.agent_body;
    let rows = frame_rows(frame);
    (body.y..body.bottom())
        .map(|y| {
            rows[y as usize]
                .chars()
                .skip(body.x as usize)
                .take(body.width as usize)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn agent_state_with_tab_label(label: &str, collapsed: &[&str]) -> (ClientShellState, FrameData) {
    let mut projected = two_workspace_agent_snapshot();
    projected.tabs[0].label = label.into();
    projected.tabs[0].custom_label = true;
    for agent in &mut projected.agents {
        agent.name = None;
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    for key in collapsed {
        state.collapsed_groups.insert((*key).into());
    }
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("agent sidebar frame").frame;
    (state, frame)
}

#[test]
fn agent_tree_guides_mark_first_middle_last_and_single_children() {
    let (state, frame) = agent_state_with_tab_label("Fidelity", &[]);
    let lines = agent_body_lines(&state, &frame);
    // Group one: a middle child then the last child. Group two: a lone child.
    assert!(lines[0].starts_with(" ▾ client-shell"), "{lines:?}");
    assert_eq!(lines[1], " ├─ ● Fidelity", "{lines:?}");
    assert_eq!(lines[2], " │    pi", "{lines:?}");
    assert_eq!(lines[3], " └─ ● Fidelity", "{lines:?}");
    assert_eq!(lines[4], "      pi", "{lines:?}");
    assert!(lines[5].starts_with(" ▾ api-server"), "{lines:?}");
    assert_eq!(lines[6], " └─ ○ Fidelity", "{lines:?}");
    assert_eq!(lines[7], "      pi", "{lines:?}");
}

#[test]
fn grouped_agent_rows_drop_the_workspace_prefix_but_ungrouped_rows_keep_it() {
    let (state, frame) = agent_state_with_tab_label("Fidelity", &[]);
    let lines = agent_body_lines(&state, &frame);
    assert!(
        lines[1..5]
            .iter()
            .all(|line| !line.contains("client-shell")),
        "{lines:?}"
    );

    let mut config = Config::default();
    config.ui.sidebar.group_agents_by_workspace = false;
    let (state, frame) = sidebar_agent_state(&config);
    let lines = agent_body_lines(&state, &frame);
    assert!(
        lines.iter().any(|line| line.contains("client-shell")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains('├')), "{lines:?}");
    assert!(lines.iter().all(|line| !line.contains('└')), "{lines:?}");
}

#[test]
fn grouped_row_without_a_tab_label_keeps_the_workspace_as_its_label() {
    let mut snapshot = two_workspace_agent_snapshot();
    for agent in &mut snapshot.agents {
        agent.name = None;
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("agent sidebar frame").frame;
    let lines = agent_body_lines(&state, &frame);
    assert_eq!(lines[1], " ├─ ● client-shell", "{lines:?}");
    assert_eq!(lines[2], " │    pi", "{lines:?}");

    // With an alias, the default `label` token names the row instead.
    let (state, frame) = sidebar_agent_state(&Config::default());
    let lines = agent_body_lines(&state, &frame);
    assert_eq!(lines[1], " ├─ ● pane_1", "{lines:?}");
    assert_eq!(lines[2], " │    pi", "{lines:?}");
}

#[test]
fn collapsed_agent_group_shows_only_its_header_without_guides() {
    let (state, frame) = agent_state_with_tab_label("Fidelity", &["agents:ws_1"]);
    let lines = agent_body_lines(&state, &frame);
    assert!(lines[0].starts_with(" ▸ client-shell"), "{lines:?}");
    assert!(lines[1].starts_with(" ▾ api-server"), "{lines:?}");
    assert_eq!(lines[2], " └─ ○ Fidelity", "{lines:?}");
    assert_eq!(state.hits.agent_headers.len(), 2);
}

#[test]
fn group_header_uses_the_blue_palette_token_and_stays_bold() {
    let (state, frame) = sidebar_agent_state(&Config::default());
    let header = state.hits.agent_headers[1].rect;
    let (x, y) = cell_symbol_position(&frame, header, "a");
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
    let cell = buffer.cell((x, y)).expect("header cell");
    assert_eq!(cell.fg, state.config.palette.blue);
    assert!(cell.modifier.contains(Modifier::BOLD));
}

#[test]
fn tree_guides_use_the_muted_color() {
    let (state, frame) = agent_state_with_tab_label("Fidelity", &[]);
    let row = state.hits.agents[0].0;
    let (x, y) = cell_symbol_position(&frame, row, "├");
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
    assert_eq!(
        buffer.cell((x, y)).unwrap().fg,
        state.config.palette.overlay0
    );
}

#[test]
fn tree_guides_shrink_then_vanish_on_narrow_rows() {
    use crate::client::shell::agent_groups::TreeGuide;
    use crate::client::shell::agent_sidebar::{render_agent_row, AgentRow};
    use crate::ui::{ResolvedToken, ResolvedTokenKind};

    let config = ClientShellConfig::from_config(&Config::default());
    let render = |tree: TreeGuide, width: u16| {
        let row = AgentRow {
            pane_id: "pane_1".into(),
            status: AgentStatus::Idle,
            focused: false,
            rows: vec![
                vec![
                    ResolvedToken {
                        kind: ResolvedTokenKind::StateIcon,
                        style: Default::default(),
                    },
                    ResolvedToken {
                        kind: ResolvedTokenKind::Tab("webapp".into()),
                        style: Default::default(),
                    },
                ],
                vec![ResolvedToken {
                    kind: ResolvedTokenKind::Agent("claude".into()),
                    style: Default::default(),
                }],
            ],
            tree,
        };
        let area = Rect::new(0, 0, width, 2);
        let mut buffer = Buffer::empty(area);
        render_agent_row(&mut buffer, area, &row, &config);
        (0..2)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        render(TreeGuide::Middle, 20),
        [" ├─ ○ webapp", " │    claude"]
    );
    assert_eq!(
        render(TreeGuide::Last, 20),
        [" └─ ○ webapp", "      claude"]
    );
    assert_eq!(render(TreeGuide::Middle, 10), [" ├ ○ weba…", " │   clau…"]);
    // Too narrow for guides: the original one-column indent, icon intact.
    assert_eq!(render(TreeGuide::Middle, 6), [" ○ we…", "   cl…"]);
    assert_eq!(render(TreeGuide::None, 20), [" ○ webapp", "   claude"]);
    // The state icon is never pushed off the row.
    for width in 1..=20 {
        let lines = render(TreeGuide::Middle, width);
        if width >= 4 {
            assert!(lines[0].contains('○'), "width {width}: {lines:?}");
        }
    }
}

#[test]
fn agent_sidebar_headers_are_not_selectable_but_rows_still_focus_their_pane() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let body = state.hits.agent_body;
    let click = |state: &mut ClientShellState, row: u16| {
        state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: body.x + 1,
            row,
            modifiers: KeyModifiers::empty(),
        })])
    };

    let on_header = click(&mut state, body.y);
    assert!(
        on_header.actions.is_empty(),
        "header click must not target an agent: {:?}",
        on_header.actions
    );

    let third = state.hits.agents[2].0;
    let on_agent = click(&mut state, third.y);
    let [ClientShellAction::Endpoint { request, .. }] = &on_agent.actions[..] else {
        panic!("agent row click should focus through endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_3"
    ));
}

#[test]
fn agent_sidebar_grouping_can_be_disabled_or_is_skipped_for_priority_sort() {
    let mut config = Config::default();
    config.ui.sidebar.group_agents_by_workspace = false;
    let (state, frame) = sidebar_agent_state(&config);
    let body = state.hits.agent_body;
    let rows = frame_rows(&frame);
    assert!(!(body.y..body.bottom()).any(|y| {
        let row = &rows[y as usize];
        row.chars()
            .skip(body.x as usize)
            .collect::<String>()
            .starts_with(" api-server")
    }));
    assert_eq!(state.hits.agents[0].0.y, body.y);
    assert_eq!(state.hits.agents[1].0.y, body.y + 2);

    let mut config = Config::default();
    config.ui.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    let (state, _) = sidebar_agent_state(&config);
    let body = state.hits.agent_body;
    assert_eq!(state.hits.agents[0].0.y, body.y);
    assert_eq!(
        state.hits.agents[0].1, "pane_2",
        "blocked agent sorts first"
    );
}

fn mouse_at(
    state: &mut ClientShellState,
    kind: MouseEventKind,
    column: u16,
    row: u16,
) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn assert_agent_rename_request(
    input: &ClientShellInput,
    expected_pane: &str,
    expected_name: Option<&str>,
) {
    let [ClientShellAction::Endpoint { request, .. }] = &input.actions[..] else {
        panic!(
            "agent rename should use the endpoint API: {:?}",
            input.actions
        );
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::AgentRename(params)
            if params.target == expected_pane && params.name.as_deref() == expected_name
    ));
}

#[test]
fn rename_agent_binding_renames_the_focused_agent_through_agent_rename() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenameAgent),
        &mut open,
    );
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename agent",
            target: ClientRenameTarget::Agent { pane_id },
            ..
        })) if pane_id == "pane_1"
    ));
    assert!(state.handle_input_bytes(&[0x15]).actions.is_empty());
    state.handle_input_bytes(b"reviewer");
    assert_agent_rename_request(&state.handle_input_bytes(b"\r"), "pane_1", Some("reviewer"));
    assert!(state.overlay.is_none());
}

#[test]
fn rename_agent_binding_is_a_default_prefix_binding() {
    let config = Config::default();
    let keybinds = config.keybinds();
    let key = crate::input::TerminalKey::new(KeyCode::Char('a'), KeyModifiers::SHIFT);
    assert_eq!(
        crate::input::resolve_non_indexed_action(
            &keybinds,
            &key,
            crate::input::KeybindDispatch::Prefix
        ),
        Some(crate::input::KeybindAction::RenameAgent)
    );
}

#[test]
fn rename_agent_empty_name_clears_and_invalid_name_keeps_the_prompt_open() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    assert!(state.open_rename_agent_overlay("pane_2"));
    state.handle_input_bytes(&[0x15]);
    state.handle_input_bytes("x".repeat(65).as_bytes());
    let rejected = state.handle_input_bytes(b"\r");
    assert!(rejected.actions.is_empty(), "invalid names are not sent");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Agent { .. },
            ..
        }))
    ));
    state.handle_input_bytes(&[0x15]);
    assert_agent_rename_request(&state.handle_input_bytes(b"\r"), "pane_2", None);
}

#[test]
fn rename_agent_accepts_ticket_keys_and_spaced_names() {
    for name in ["NH-123", "Fix login bug"] {
        let (mut state, _) = sidebar_agent_state(&Config::default());
        assert!(state.open_rename_agent_overlay("pane_2"));
        state.handle_input_bytes(&[0x15]);
        state.handle_input_bytes(name.as_bytes());
        assert_agent_rename_request(&state.handle_input_bytes(b"\r"), "pane_2", Some(name));
    }
}

#[test]
fn rename_agent_without_an_agent_reports_instead_of_opening_a_prompt() {
    let mut snapshot = two_workspace_agent_snapshot();
    snapshot.agents.clear();
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    let mut open = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::RenameAgent),
        &mut open,
    );
    assert!(state.overlay.is_none());
    assert!(open.repaint);
    assert!(state.visible_endpoint_notice.is_some());
}

#[test]
fn double_click_on_an_agent_row_opens_the_rename_prompt() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let row = state.hits.agents[2].0;
    let down = MouseEventKind::Down(MouseButton::Left);
    let first = mouse_at(&mut state, down, row.x + 1, row.y);
    assert!(matches!(
        &first.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(&request.method, crate::api::schema::Method::PaneFocus(_))
    ));
    assert!(state.overlay.is_none());
    mouse_at(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        row.x + 1,
        row.y,
    );
    let second = mouse_at(&mut state, down, row.x + 1, row.y);
    assert!(second.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Agent { pane_id },
            ..
        })) if pane_id == "pane_3"
    ));
}

#[test]
fn two_single_clicks_on_different_agents_do_not_rename() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let down = MouseEventKind::Down(MouseButton::Left);
    let (first, second) = (state.hits.agents[0].0, state.hits.agents[1].0);
    mouse_at(&mut state, down, first.x + 1, first.y);
    mouse_at(&mut state, down, second.x + 1, second.y);
    assert!(state.overlay.is_none());
}

#[test]
fn right_click_on_an_agent_row_offers_rename_and_clear() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let row = state.hits.agents[1].0;
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 2,
        row.y,
    );
    let items = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("agent context menu"),
    };
    let actions = items.iter().map(|item| item.action).collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            ClientContextMenuAction::RenameAgent,
            ClientContextMenuAction::ClearAgentName
        ]
    );

    state.compose(106, 30).expect("context menu frame");
    let clear = state.hits.context_menu_rows[1].0;
    let clicked = mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        clear.x + 1,
        clear.y,
    );
    assert_agent_rename_request(&clicked, "pane_2", None);

    // The rename entry opens the same prompt as the keybinding.
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 2,
        row.y,
    );
    state.compose(106, 30).expect("context menu frame");
    let rename = state.hits.context_menu_rows[0].0;
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        rename.x + 1,
        rename.y,
    );
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Agent { pane_id },
            ..
        })) if pane_id == "pane_2"
    ));
}

#[test]
fn sidebar_agent_label_prefers_the_renamed_alias_over_reported_display_name() {
    let mut snapshot = two_workspace_agent_snapshot();
    snapshot.agents[0].name = Some("reviewer".into());
    snapshot.agents[0].display_agent = Some("Codex CLI".into());
    let mut config = Config::default();
    config.ui.sidebar.agents.rows = vec![vec![
        crate::config::AgentSidebarToken::StateIcon,
        crate::config::AgentSidebarToken::Agent,
    ]];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot.clone()));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("frame").frame;
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("reviewer"), "alias shown: {text}");
    assert!(!text.contains("Codex CLI"));

    // The default rows show the alias as the label and keep the reported kind.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let frame = state.compose(106, 30).expect("frame").frame;
    let text = frame_rows(&frame).join("\n");
    assert!(text.contains("├─ ● reviewer"), "alias as label: {text}");
    assert!(text.contains("Codex CLI"), "kind kept: {text}");
}

fn click_agent_header(state: &mut ClientShellState, index: usize) -> ClientShellInput {
    let rect = state.hits.agent_headers[index].rect;
    mouse_at(
        state,
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 1,
        rect.y,
    )
}

fn recompose(state: &mut ClientShellState) -> FrameData {
    state.compose(106, 30).expect("agent sidebar frame").frame
}

fn agent_hit_panes(state: &ClientShellState) -> Vec<&str> {
    state
        .hits
        .agents
        .iter()
        .map(|(_, pane_id)| pane_id.as_str())
        .collect()
}

fn focus_target(input: &ClientShellInput) -> Option<&str> {
    match &input.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => match &request.method {
            crate::api::schema::Method::PaneFocus(target) => Some(target.pane_id.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn agent_group_headers_are_list_items_with_their_own_hit_rects() {
    let (state, _) = sidebar_agent_state(&Config::default());
    let body = state.hits.agent_body;
    let headers = state
        .hits
        .agent_headers
        .iter()
        .map(|hit| {
            (
                hit.workspace_id.as_str(),
                hit.rect.y,
                hit.rect.height,
                hit.collapsed,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        headers,
        [("ws_1", body.y, 1, false), ("ws_2", body.y + 5, 1, false)]
    );
    // Agent rows keep their own rects between the headers.
    let agents = state
        .hits
        .agents
        .iter()
        .map(|(rect, pane_id)| (pane_id.as_str(), rect.y))
        .collect::<Vec<_>>();
    assert_eq!(
        agents,
        [
            ("pane_1", body.y + 1),
            ("pane_2", body.y + 3),
            ("pane_3", body.y + 6)
        ]
    );
}

#[test]
fn clicking_a_group_header_collapses_and_expands_its_agents() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let body = state.hits.agent_body;

    let collapse = click_agent_header(&mut state, 0);
    assert!(collapse.actions.is_empty() && collapse.repaint);
    let frame = recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_3"]);
    assert_eq!(
        state.hits.agents[0].0.y,
        body.y + 2,
        "second group moves up"
    );
    let rows = frame_rows(&frame);
    let header = rows[body.y as usize]
        .chars()
        .skip(body.x as usize)
        .take(body.width as usize)
        .collect::<String>();
    assert!(header.starts_with(" ▸ client-shell"), "header: {header}");
    // Counts stay visible so a hidden blocked agent is still accounted for.
    assert!(header.contains("1●"), "header: {header}");
    assert!(state.hits.agent_headers[0].collapsed);

    // A stale hit rect from the expanded layout no longer focuses anything.
    let hidden_row = mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y + 4,
    );
    assert!(focus_target(&hidden_row).is_none_or(|pane| pane != "pane_2"));

    click_agent_header(&mut state, 0);
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_1", "pane_2", "pane_3"]);
    assert!(!state.hits.agent_headers[0].collapsed);
}

#[test]
fn group_toggle_ignores_other_groups_and_uses_a_dedicated_state_key() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_agent_header(&mut state, 1);
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_1", "pane_2"]);
    assert!(state.collapsed_groups.contains("agents:ws_2"));
    assert_eq!(state.collapsed_groups.len(), 1);
}

#[test]
fn collapsed_group_with_a_blocked_agent_highlights_its_header() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let blocked = status_color(AgentStatus::Blocked, &state.config.palette);
    let label_fg = |state: &mut ClientShellState, workspace: &str| {
        let frame = recompose(state);
        let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
        let header = state
            .hits
            .agent_headers
            .iter()
            .find(|hit| hit.workspace_id == workspace)
            .expect("header")
            .rect;
        let label = if workspace == "ws_1" {
            "client-shell"
        } else {
            "api-server"
        };
        let (x, y) = cell_symbol_position(&frame, header, label);
        buffer.cell((x, y)).expect("label cell").fg
    };
    // Expanded: the row shows the state, the header stays muted.
    assert_ne!(label_fg(&mut state, "ws_1"), blocked);
    click_agent_header(&mut state, 0);
    assert_eq!(label_fg(&mut state, "ws_1"), blocked);
    // A collapsed group without a blocked agent is not highlighted.
    click_agent_header(&mut state, 1);
    assert_ne!(label_fg(&mut state, "ws_2"), blocked);
}

#[test]
fn group_header_shows_focus_even_when_the_focused_agent_is_hidden() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_agent_header(&mut state, 0);
    let frame = recompose(&mut state);
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
    let header = state.hits.agent_headers[0].rect;
    let (x, y) = cell_symbol_position(&frame, header, "client-shell");
    assert_eq!(
        buffer.cell((x, y)).expect("label cell").bg,
        state.config.palette.active_row_bg
    );
    let other = state.hits.agent_headers[1].rect;
    let (x, y) = cell_symbol_position(&frame, other, "api-server");
    assert_ne!(
        buffer.cell((x, y)).expect("label cell").bg,
        state.config.palette.active_row_bg
    );
}

#[test]
fn double_click_on_a_group_header_renames_the_workspace_without_changing_collapse() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_agent_header(&mut state, 1);
    assert!(state.overlay.is_none());
    let second = click_agent_header(&mut state, 1);
    assert!(second.actions.is_empty());
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename workspace",
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_2"
    ));
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_1", "pane_2", "pane_3"]);
    assert!(state.collapsed_groups.is_empty());

    state.handle_input_bytes(&[0x15]);
    state.handle_input_bytes(b"backend");
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("workspace rename should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceRename(params)
            if params.workspace_id == "ws_2" && params.label == "backend"
    ));
}

fn click_space_row(state: &mut ClientShellState, index: usize, line: u16) -> ClientShellInput {
    let rect = state.hits.workspaces[index].rect;
    let row = rect.y + line.min(rect.height.saturating_sub(1));
    mouse_at(
        state,
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 3,
        row,
    );
    mouse_at(
        state,
        MouseEventKind::Up(MouseButton::Left),
        rect.x + 3,
        row,
    )
}

fn rename_overlay_workspace(state: &ClientShellState) -> Option<&str> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "rename workspace",
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) => Some(workspace_id.as_str()),
        _ => None,
    }
}

#[test]
fn double_click_on_a_space_label_or_secondary_line_opens_the_rename_prompt() {
    for line in [0u16, 1] {
        let (mut state, _) = sidebar_agent_state(&Config::default());
        assert!(
            state.hits.workspaces[1].rect.height > line,
            "space row has line {line}"
        );
        let first = click_space_row(&mut state, 1, line);
        assert!(
            matches!(
                &first.actions[..],
                [ClientShellAction::Endpoint { request, .. }]
                    if matches!(&request.method, crate::api::schema::Method::WorkspaceFocus(_))
            ),
            "single click still focuses: {:?}",
            first.actions
        );
        assert!(state.overlay.is_none());
        click_space_row(&mut state, 1, line);
        assert_eq!(rename_overlay_workspace(&state), Some("ws_2"));
    }
}

#[test]
fn double_click_on_a_space_row_renames_via_workspace_rename() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_space_row(&mut state, 1, 0);
    click_space_row(&mut state, 1, 1);
    assert_eq!(rename_overlay_workspace(&state), Some("ws_2"));
    state.handle_input_bytes(&[0x15]);
    state.handle_input_bytes(b"backend");
    let save = state.handle_input_bytes(b"\r");
    let [ClientShellAction::Endpoint { request, .. }] = &save.actions[..] else {
        panic!("workspace rename should use the endpoint API");
    };
    assert!(matches!(
        &request.method,
        crate::api::schema::Method::WorkspaceRename(params)
            if params.workspace_id == "ws_2" && params.label == "backend"
    ));
}

#[test]
fn single_clicks_on_different_space_rows_do_not_rename() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_space_row(&mut state, 0, 0);
    click_space_row(&mut state, 1, 0);
    assert!(state.overlay.is_none());
    click_space_row(&mut state, 0, 0);
    assert!(state.overlay.is_none());
}

#[test]
fn space_double_click_is_not_completed_by_a_slow_second_click() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_space_row(&mut state, 1, 0);
    let last = state
        .last_agent_list_click
        .as_mut()
        .expect("click recorded");
    last.at -= ClientAgentListClick::DOUBLE_CLICK + std::time::Duration::from_millis(50);
    click_space_row(&mut state, 1, 0);
    assert!(state.overlay.is_none());
}

#[test]
fn right_click_on_a_group_header_offers_rename_and_collapse_controls() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    let header = state.hits.agent_headers[0].rect;
    let open = |state: &mut ClientShellState| {
        mouse_at(
            state,
            MouseEventKind::Down(MouseButton::Right),
            header.x + 2,
            header.y,
        );
        state.compose(106, 30).expect("context menu frame");
    };
    open(&mut state);
    let actions = match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .map(|item| item.action)
            .collect::<Vec<_>>(),
        _ => panic!("group header context menu"),
    };
    assert_eq!(
        actions,
        [
            ClientContextMenuAction::Rename,
            ClientContextMenuAction::ToggleAgentGroup,
            ClientContextMenuAction::CollapseAgentGroups,
            ClientContextMenuAction::ExpandAgentGroups,
        ]
    );
    let click_item = |state: &mut ClientShellState, index: usize| {
        let row = state.hits.context_menu_rows[index].0;
        mouse_at(
            state,
            MouseEventKind::Down(MouseButton::Left),
            row.x + 1,
            row.y,
        );
    };

    click_item(&mut state, 2);
    recompose(&mut state);
    assert!(
        agent_hit_panes(&state).is_empty(),
        "collapse all hides every row"
    );
    assert_eq!(state.hits.agent_headers.len(), 2);

    open(&mut state);
    click_item(&mut state, 3);
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_1", "pane_2", "pane_3"]);

    open(&mut state);
    click_item(&mut state, 0);
    assert!(matches!(
        state.overlay.as_ref(),
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Workspace { workspace_id },
            ..
        })) if workspace_id == "ws_1"
    ));
}

#[test]
fn group_bindings_toggle_the_focused_group_and_collapse_or_expand_all() {
    use crate::input::{KeybindAction, KeybindMatch};

    let (mut state, _) = sidebar_agent_state(&Config::default());
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleAgentGroup),
        &mut outcome,
    );
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_3"], "focused group is ws_1");
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ToggleAgentGroup),
        &mut outcome,
    );
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state), ["pane_1", "pane_2", "pane_3"]);

    state.record_binding(
        KeybindMatch::Action(KeybindAction::CollapseAgentGroups),
        &mut outcome,
    );
    recompose(&mut state);
    assert!(agent_hit_panes(&state).is_empty());
    state.record_binding(
        KeybindMatch::Action(KeybindAction::ExpandAgentGroups),
        &mut outcome,
    );
    recompose(&mut state);
    assert_eq!(agent_hit_panes(&state).len(), 3);
    assert!(state.collapsed_groups.is_empty());
}

#[test]
fn group_bindings_have_expected_defaults_and_report_when_grouping_is_off() {
    let config = Config::default();
    let keybinds = config.keybinds();
    let resolve = |code, modifiers| {
        crate::input::resolve_non_indexed_action(
            &keybinds,
            &crate::input::TerminalKey::new(code, modifiers),
            crate::input::KeybindDispatch::Prefix,
        )
    };
    assert_eq!(
        resolve(KeyCode::Char('e'), KeyModifiers::SHIFT),
        Some(crate::input::KeybindAction::ToggleAgentGroup)
    );
    assert!(!config.keys.collapse_agent_groups.has_values());
    assert!(!config.keys.expand_agent_groups.has_values());

    let mut config = Config::default();
    config.ui.sidebar.group_agents_by_workspace = false;
    let (mut state, _) = sidebar_agent_state(&config);
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CollapseAgentGroups),
        &mut outcome,
    );
    assert!(state.collapsed_groups.is_empty());
    assert!(state.visible_endpoint_notice.is_some());
}

#[test]
fn next_and_previous_agent_skip_collapsed_groups() {
    use crate::input::{KeybindAction, KeybindMatch};

    let (mut state, _) = sidebar_agent_state(&Config::default());
    click_agent_header(&mut state, 1);
    recompose(&mut state);
    let step = |state: &mut ClientShellState, action| {
        let mut outcome = ClientShellInput::default();
        state.record_binding(KeybindMatch::Action(action), &mut outcome);
        focus_target(&outcome).map(str::to_owned)
    };
    // pane_1 (focused) -> pane_2 -> wraps over the hidden pane_3 to pane_1.
    assert_eq!(
        step(&mut state, KeybindAction::NextAgent).as_deref(),
        Some("pane_2")
    );
    assert_eq!(
        step(&mut state, KeybindAction::PreviousAgent).as_deref(),
        Some("pane_2"),
        "previous from pane_1 wraps to the last visible agent"
    );

    // Focus inside the collapsed group: stepping leaves it for visible agents.
    let mut snapshot = two_workspace_agent_snapshot();
    snapshot.agents[0].focused = false;
    snapshot.agents[2].focused = true;
    snapshot.focused_pane_id = Some("pane_3".into());
    state.set_snapshot(Box::new(snapshot));
    assert_eq!(
        step(&mut state, KeybindAction::NextAgent),
        Some("pane_1".into())
    );
}

#[test]
fn stepping_falls_back_to_all_agents_when_every_group_is_collapsed() {
    use crate::input::{KeybindAction, KeybindMatch};

    let (mut state, _) = sidebar_agent_state(&Config::default());
    let mut outcome = ClientShellInput::default();
    state.record_binding(
        KeybindMatch::Action(KeybindAction::CollapseAgentGroups),
        &mut outcome,
    );
    recompose(&mut state);
    let mut step = ClientShellInput::default();
    state.record_binding(KeybindMatch::Action(KeybindAction::NextAgent), &mut step);
    assert_eq!(focus_target(&step), Some("pane_2"));
}

fn many_workspace_agent_snapshot(count: usize) -> ClientShellSnapshot {
    let mut projected = two_workspace_agent_snapshot();
    let template = projected.workspaces[0].clone();
    projected.workspaces.clear();
    projected.agents.clear();
    for index in 0..count {
        let mut workspace = template.clone();
        workspace.workspace_id = format!("ws_{index}");
        workspace.number = index + 1;
        workspace.label = format!("space-{index}");
        workspace.focused = index == 0;
        projected.workspaces.push(workspace);
        projected.agents.push(ClientShellAgent {
            pane_id: format!("pane_{index}"),
            workspace_id: format!("ws_{index}"),
            tab_id: "tab_1".into(),
            name: Some(format!("agent-{index}")),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: index == 0,
        });
    }
    projected
}

#[test]
fn revealing_an_agent_in_a_collapsed_group_scrolls_to_its_header() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(many_workspace_agent_snapshot(12)));
    state.set_pane_surface(surface());
    state.compose(106, 24).expect("frame");
    assert!(state.hits.agent_max_scroll > 0, "list must overflow");
    assert!(!state
        .hits
        .agent_headers
        .iter()
        .any(|hit| hit.workspace_id == "ws_11"));

    state.collapsed_groups.insert("agents:ws_11".into());
    state.reveal_snapshot_agent("pane_11");
    state.compose(106, 24).expect("frame");
    let header = state
        .hits
        .agent_headers
        .iter()
        .find(|hit| hit.workspace_id == "ws_11")
        .expect("collapsed group header is revealed");
    assert!(header.collapsed);
    assert!(!state.hits.agents.iter().any(|(_, pane)| pane == "pane_11"));
}

#[test]
fn collapsed_group_state_persists_across_client_restarts() {
    let path = std::env::temp_dir().join(format!(
        "flockr-shell-agent-groups-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    state.set_pane_surface(surface());
    recompose(&mut state);
    click_agent_header(&mut state, 0);

    let restored_config =
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone());
    let mut restored = ClientShellState::new(restored_config);
    restored.set_snapshot(Box::new(two_workspace_agent_snapshot()));
    restored.set_pane_surface(surface());
    recompose(&mut restored);
    assert_eq!(agent_hit_panes(&restored), ["pane_3"]);
    assert!(restored.hits.agent_headers[0].collapsed);
    assert!(!restored.hits.agent_headers[1].collapsed);

    // Expanding again is persisted too.
    click_agent_header(&mut restored, 0);
    let reloaded = ClientShellState::new(
        ClientShellConfig::from_config(&Config::default()).with_preferences_path(path.clone()),
    );
    assert!(reloaded.collapsed_groups.is_empty());
    std::fs::remove_file(path).expect("remove agent group preferences");
}

#[test]
fn collapsed_agent_groups_do_not_collide_with_worktree_group_keys() {
    let (mut state, _) = sidebar_agent_state(&Config::default());
    state.collapsed_groups.insert("/repo".into());
    click_agent_header(&mut state, 0);
    click_agent_header(&mut state, 0);
    assert!(state.collapsed_groups.contains("/repo"));
    let mut outcome = ClientShellInput::default();
    state.set_all_agent_groups(true, &mut outcome);
    state.set_all_agent_groups(false, &mut outcome);
    assert_eq!(
        state.collapsed_groups.len(),
        1,
        "expand all keeps worktree groups"
    );
    assert!(state.collapsed_groups.contains("/repo"));
}

#[test]
fn step_visible_agent_wraps_skips_hidden_and_falls_back() {
    use super::super::agent_groups::step_visible_agent;

    let targets = [1, 2, 3, 4];
    let visible = |value: &i32| value % 2 == 0;
    assert_eq!(
        step_visible_agent(&targets, visible, Some(1), false),
        Some(3)
    );
    assert_eq!(
        step_visible_agent(&targets, visible, Some(3), false),
        Some(1)
    );
    assert_eq!(
        step_visible_agent(&targets, visible, Some(1), true),
        Some(3)
    );
    assert_eq!(step_visible_agent(&targets, visible, None, false), Some(1));
    assert_eq!(step_visible_agent(&targets, visible, None, true), Some(3));
    // Current agent hidden (index 0 holds 1): stepping still moves on.
    assert_eq!(
        step_visible_agent(&targets, visible, Some(0), false),
        Some(1)
    );
    // Everything hidden: walk the unfiltered list.
    assert_eq!(
        step_visible_agent(&targets, |_: &i32| false, Some(0), false),
        Some(1)
    );
    assert_eq!(step_visible_agent::<i32>(&[], visible, None, false), None);
}

/// Grouped tree sidebar like the reported setup: workspace "vitalengine-new-house"
/// with tabs "today" and "2", workspace "sandbox" with tabs "1" and "2", one
/// `claude` agent per tab and no aliases. The active endpoint advertises exactly
/// the methods the real server offers on the client-shell lane.
fn reported_tree_state(rows: &str) -> ClientShellState {
    let config: Config = toml::from_str(&format!("[ui.sidebar.agents]\nrows = {rows}\n"))
        .expect("sidebar rows config");
    let mut snapshot = two_workspace_agent_snapshot();
    snapshot.workspaces[0].label = "vitalengine-new-house".into();
    snapshot.workspaces[1].label = "sandbox".into();
    let tab = |tab_id: &str, workspace_id: &str, number: usize, label: &str, custom: bool| {
        let mut tab = snapshot.tabs[0].clone();
        tab.tab_id = tab_id.into();
        tab.workspace_id = workspace_id.into();
        tab.number = number;
        tab.label = label.into();
        tab.custom_label = custom;
        tab.focused = tab_id == "tab_1";
        tab
    };
    snapshot.tabs = vec![
        tab("tab_1", "ws_1", 1, "today", true),
        tab("tab_2", "ws_1", 2, "2", false),
        tab("tab_3", "ws_2", 1, "1", false),
        tab("tab_4", "ws_2", 2, "2", false),
    ];
    for (agent, tab_id) in snapshot.agents.iter_mut().zip(["tab_1", "tab_2", "tab_3"]) {
        agent.name = None;
        agent.agent = Some("claude".into());
        agent.agent_status = AgentStatus::Idle;
        agent.tab_id = tab_id.into();
    }
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    state.set_snapshot(Box::new(snapshot));
    state.endpoints[0].methods = Some(
        crate::server::client_commands::supported_client_shell_method_names()
            .iter()
            .map(|method| (*method).to_owned())
            .collect(),
    );
    state.set_pane_surface(surface());
    state.compose(106, 30).expect("agent sidebar frame");
    state
}

const REPORTED_ROWS: &str = r#"[["state_icon", "machine", "workspace", "tab", "agent"]]"#;
const LABEL_ROWS: &str = r#"[["state_icon", "machine", "workspace", "label", "agent_kind"]]"#;

fn agent_row_rect(state: &ClientShellState, pane_id: &str) -> Rect {
    state
        .hits
        .agents
        .iter()
        .find(|(_, pane)| pane == pane_id)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("agent row for {pane_id}"))
}

fn sidebar_text(state: &mut ClientShellState) -> String {
    frame_rows(&recompose(state)).join("\n")
}

/// Type a name into the open agent prompt and press Enter.
fn submit_agent_name(state: &mut ClientShellState, name: &str) -> ClientShellInput {
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Agent { .. },
            ..
        }))
    ));
    state.handle_input_bytes(&[0x15]);
    state.handle_input_bytes(name.as_bytes());
    let saved = state.handle_input_bytes(b"\r");
    assert!(
        state.visible_endpoint_notice.is_none(),
        "rename must reach the server, not stop at a client notice"
    );
    saved
}

#[test]
fn server_advertises_agent_rename_on_the_client_shell_lane() {
    // The sidebar prompt only sends methods the server advertises; without this
    // entry every UI rename ended in "Action unavailable" and never left the client.
    assert!(
        crate::server::client_commands::supported_client_shell_method_names()
            .contains(&"agent.rename")
    );
}

#[test]
fn double_click_on_a_tree_agent_row_renames_that_pane() {
    let mut state = reported_tree_state(REPORTED_ROWS);
    let text = sidebar_text(&mut state);
    assert!(text.contains("├─ ○ today · claude"), "{text}");
    assert!(text.contains("└─ ○ 2 · claude"), "{text}");

    let row = agent_row_rect(&state, "pane_2");
    let down = MouseEventKind::Down(MouseButton::Left);
    mouse_at(&mut state, down, row.x + 6, row.y);
    mouse_at(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        row.x + 6,
        row.y,
    );
    assert!(mouse_at(&mut state, down, row.x + 6, row.y)
        .actions
        .is_empty());
    let saved = submit_agent_name(&mut state, "NH-1234");
    assert_agent_rename_request(&saved, "pane_2", Some("NH-1234"));
}

#[test]
fn context_menu_rename_on_a_tree_agent_row_renames_that_pane() {
    let mut state = reported_tree_state(REPORTED_ROWS);
    let row = agent_row_rect(&state, "pane_3");
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 6,
        row.y,
    );
    state.compose(106, 30).expect("context menu frame");
    let rename = state.hits.context_menu_rows[0].0;
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        rename.x + 1,
        rename.y,
    );
    let saved = submit_agent_name(&mut state, "NH-1234");
    assert_agent_rename_request(&saved, "pane_3", Some("NH-1234"));

    // "Clear agent name" (offered once an alias exists) uses the same method.
    let mut snapshot = state.snapshot.as_deref().cloned().expect("snapshot");
    snapshot.agents[2].name = Some("NH-1234".into());
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 30).expect("agent sidebar frame");
    mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 6,
        row.y,
    );
    state.compose(106, 30).expect("context menu frame");
    let clear = state.hits.context_menu_rows[1].0;
    let cleared = mouse_at(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        clear.x + 1,
        clear.y,
    );
    assert_agent_rename_request(&cleared, "pane_3", None);
}

#[test]
fn prefix_shift_a_renames_the_focused_agent_through_the_server() {
    let mut state = reported_tree_state(REPORTED_ROWS);
    assert!(state.handle_input_bytes(&[0x02]).actions.is_empty());
    assert!(state.handle_input_bytes(b"A").actions.is_empty());
    let saved = submit_agent_name(&mut state, "NH-1234");
    assert_agent_rename_request(&saved, "pane_1", Some("NH-1234"));
}

#[test]
fn label_rows_show_the_alias_in_place_of_the_tab_and_keep_the_agent_kind() {
    let mut state = reported_tree_state(LABEL_ROWS);
    let text = sidebar_text(&mut state);
    assert!(text.contains("├─ ○ today · claude"), "{text}");
    assert!(text.contains("└─ ○ 1 · claude"), "{text}");
    assert!(!text.contains("vitalengine-new-house · "), "{text}");

    let mut snapshot = state.snapshot.as_deref().cloned().expect("snapshot");
    snapshot.agents[0].name = Some("NH-1234".into());
    snapshot.agents[2].name = Some("NH-123".into());
    state.set_snapshot(Box::new(snapshot));
    let text = sidebar_text(&mut state);
    assert!(text.contains("├─ ○ NH-1234 · claude"), "{text}");
    assert!(text.contains("└─ ○ NH-123 · claude"), "{text}");
    assert!(
        !text.contains("○ today"),
        "the alias replaces the tab label: {text}"
    );

    // The reported layout keeps its documented meaning: `agent` shows the alias.
    let mut state = reported_tree_state(REPORTED_ROWS);
    let mut snapshot = state.snapshot.as_deref().cloned().expect("snapshot");
    snapshot.agents[0].name = Some("NH-1234".into());
    state.set_snapshot(Box::new(snapshot));
    let text = sidebar_text(&mut state);
    assert!(text.contains("├─ ○ today · NH-1234"), "{text}");
}
