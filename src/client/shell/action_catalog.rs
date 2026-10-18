//! Keybinding actions with display labels and categories, read from the live
//! keybinding table so user overrides and endpoint profiles are reflected.
//! Shared by the which-key popup and the command palette.

use crate::config::{ActionKeybinds, IndexedKeybind, Keybinds};
use crate::input::KeybindAction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActionCategory {
    Session,
    Workspaces,
    Agents,
    Tabs,
    Panes,
    Custom,
}

impl ActionCategory {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspaces => "workspaces",
            Self::Agents => "agents",
            Self::Tabs => "tabs",
            Self::Panes => "panes",
            Self::Custom => "custom",
        }
    }
}

pub(super) struct ActionSpec<'a> {
    pub(super) action: KeybindAction,
    pub(super) label: &'static str,
    pub(super) category: ActionCategory,
    pub(super) bindings: &'a ActionKeybinds,
}

pub(super) struct IndexedSpec<'a> {
    pub(super) label: &'static str,
    pub(super) category: ActionCategory,
    pub(super) bindings: &'a [IndexedKeybind],
}

/// Every single-key action in display order. Indexed actions (switch tab,
/// switch workspace, focus agent) are listed by `indexed_specs`.
pub(super) fn action_specs(keybinds: &Keybinds) -> Vec<ActionSpec<'_>> {
    use ActionCategory::{Agents, Panes, Session, Tabs, Workspaces};
    use KeybindAction as A;

    let k = keybinds;
    [
        (A::Help, "keybinds", Session, &k.help),
        (A::Settings, "settings", Session, &k.settings),
        (
            A::AgentDashboard,
            "agent dashboard",
            Agents,
            &k.agent_dashboard,
        ),
        (A::Jobs, "jobs", Session, &k.jobs),
        (A::PreviewAgent, "preview agent", Agents, &k.preview_agent),
        (
            A::CommandPalette,
            "command palette",
            Session,
            &k.command_palette,
        ),
        (A::ReloadConfig, "reload config", Session, &k.reload_config),
        (
            A::OpenNotificationTarget,
            "open notification target",
            Session,
            &k.open_notification_target,
        ),
        (
            A::ToggleSidebar,
            "toggle sidebar",
            Session,
            &k.toggle_sidebar,
        ),
        (A::Detach, "detach", Session, &k.detach),
        (
            A::WorkspacePicker,
            "workspace navigation",
            Workspaces,
            &k.workspace_picker,
        ),
        (A::OpenNavigator, "session navigator", Workspaces, &k.goto),
        (
            A::NewWorkspace,
            "new workspace",
            Workspaces,
            &k.new_workspace,
        ),
        (A::NewWorktree, "new worktree", Workspaces, &k.new_worktree),
        (
            A::OpenWorktree,
            "open worktree",
            Workspaces,
            &k.open_worktree,
        ),
        (
            A::RemoveWorktree,
            "delete worktree checkout",
            Workspaces,
            &k.remove_worktree,
        ),
        (
            A::RenameWorkspace,
            "rename workspace",
            Workspaces,
            &k.rename_workspace,
        ),
        (
            A::CloseWorkspace,
            "close workspace",
            Workspaces,
            &k.close_workspace,
        ),
        (
            A::PreviousWorkspace,
            "previous workspace",
            Workspaces,
            &k.previous_workspace,
        ),
        (
            A::NextWorkspace,
            "next workspace",
            Workspaces,
            &k.next_workspace,
        ),
        (
            A::PreviousAgent,
            "previous agent",
            Agents,
            &k.previous_agent,
        ),
        (A::NextAgent, "next agent", Agents, &k.next_agent),
        (A::RenameAgent, "rename agent", Agents, &k.rename_agent),
        (
            A::ToggleAgentGroup,
            "collapse / expand agent group",
            Agents,
            &k.toggle_agent_group,
        ),
        (
            A::CollapseAgentGroups,
            "collapse all agent groups",
            Agents,
            &k.collapse_agent_groups,
        ),
        (
            A::ExpandAgentGroups,
            "expand all agent groups",
            Agents,
            &k.expand_agent_groups,
        ),
        (A::NewTab, "new tab", Tabs, &k.new_tab),
        (A::RenameTab, "rename tab", Tabs, &k.rename_tab),
        (A::PreviousTab, "previous tab", Tabs, &k.previous_tab),
        (A::NextTab, "next tab", Tabs, &k.next_tab),
        (
            A::MoveTabPrevious,
            "move tab left",
            Tabs,
            &k.move_tab_previous,
        ),
        (A::MoveTabNext, "move tab right", Tabs, &k.move_tab_next),
        (A::CloseTab, "close tab", Tabs, &k.close_tab),
        (A::SplitVertical, "split vertical", Panes, &k.split_vertical),
        (
            A::SplitHorizontal,
            "split horizontal",
            Panes,
            &k.split_horizontal,
        ),
        (A::ClosePane, "close pane", Panes, &k.close_pane),
        (A::RenamePane, "rename pane", Panes, &k.rename_pane),
        (
            A::EditScrollback,
            "edit scrollback",
            Panes,
            &k.edit_scrollback,
        ),
        (A::ClearPane, "clear pane", Panes, &k.clear_pane),
        (A::CopyMode, "copy mode", Panes, &k.copy_mode),
        (A::Zoom, "zoom pane", Panes, &k.zoom),
        (A::EnterResizeMode, "resize mode", Panes, &k.resize_mode),
        (
            A::ResizePaneLeft,
            "resize pane left",
            Panes,
            &k.resize_pane_left,
        ),
        (
            A::ResizePaneDown,
            "resize pane down",
            Panes,
            &k.resize_pane_down,
        ),
        (A::ResizePaneUp, "resize pane up", Panes, &k.resize_pane_up),
        (
            A::ResizePaneRight,
            "resize pane right",
            Panes,
            &k.resize_pane_right,
        ),
        (
            A::FocusPaneLeft,
            "focus pane left",
            Panes,
            &k.focus_pane_left,
        ),
        (
            A::FocusPaneDown,
            "focus pane down",
            Panes,
            &k.focus_pane_down,
        ),
        (A::FocusPaneUp, "focus pane up", Panes, &k.focus_pane_up),
        (
            A::FocusPaneRight,
            "focus pane right",
            Panes,
            &k.focus_pane_right,
        ),
        (A::SwapPaneLeft, "swap pane left", Panes, &k.swap_pane_left),
        (A::SwapPaneDown, "swap pane down", Panes, &k.swap_pane_down),
        (A::SwapPaneUp, "swap pane up", Panes, &k.swap_pane_up),
        (
            A::SwapPaneRight,
            "swap pane right",
            Panes,
            &k.swap_pane_right,
        ),
        (
            A::CyclePaneNext,
            "cycle pane next",
            Panes,
            &k.cycle_pane_next,
        ),
        (
            A::CyclePanePrevious,
            "cycle pane previous",
            Panes,
            &k.cycle_pane_previous,
        ),
        (A::LastPane, "last pane", Panes, &k.last_pane),
    ]
    .into_iter()
    .map(|(action, label, category, bindings)| ActionSpec {
        action,
        label,
        category,
        bindings,
    })
    .collect()
}

pub(super) fn indexed_specs(keybinds: &Keybinds) -> [IndexedSpec<'_>; 3] {
    [
        IndexedSpec {
            label: "switch workspace",
            category: ActionCategory::Workspaces,
            bindings: &keybinds.switch_workspace,
        },
        IndexedSpec {
            label: "focus agent",
            category: ActionCategory::Agents,
            bindings: &keybinds.focus_agent,
        },
        IndexedSpec {
            label: "switch tab",
            category: ActionCategory::Tabs,
            bindings: &keybinds.switch_tab,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_unique_and_bound_actions_are_listed() {
        let keybinds = Keybinds::default();
        let specs = action_specs(&keybinds);
        for (index, spec) in specs.iter().enumerate() {
            assert!(
                specs[index + 1..]
                    .iter()
                    .all(|other| other.label != spec.label),
                "{:?} listed twice",
                spec.label
            );
        }
        let bound = |label: &str| {
            specs
                .iter()
                .find(|spec| spec.label == label)
                .is_some_and(|spec| spec.bindings.prefix_rhs_label().is_some())
        };
        assert!(bound("new tab") && bound("rename agent") && bound("zoom pane"));
        assert!(!bound("move tab left"));
    }
}
