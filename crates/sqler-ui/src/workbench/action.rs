// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `workbench:*` and `pane:*` command IDs and their mapping to [`WorkbenchAction`].

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

use super::layout::{PanelView, SideView, WorkbenchAction};

/// The `workbench:*` commands: ID and title.
const COMMANDS: &[(&str, &str)] = &[
    ("workbench:toggle-side-panel", "View: Toggle Side Panel"),
    ("workbench:show-connections", "View: Show Connections"),
    ("workbench:show-saved-queries", "View: Show Saved Queries"),
    ("workbench:show-ai", "View: Show AI"),
    ("workbench:toggle-bottom-panel", "View: Toggle Panel"),
    ("workbench:show-results", "View: Show Results"),
    ("workbench:show-messages", "View: Show Messages"),
    ("workbench:show-audit", "View: Show Audit Log"),
    ("workbench:focus-next-region", "View: Focus Next Region"),
    (
        "workbench:focus-previous-region",
        "View: Focus Previous Region",
    ),
    ("workbench:reset-layout", "View: Reset Layout"),
    ("workbench:set-side-width", "Set Side Panel Width"),
    ("workbench:set-panel-height", "Set Panel Height"),
];

/// The `pane:*` commands (editor groups): ID and title. Registered by the shell, not by
/// [`register_commands`].
const PANE_COMMANDS: &[(&str, &str)] = &[
    ("pane:split-right", "View: Split Editor Right"),
    ("pane:focus-next", "View: Focus Next Editor Group"),
    ("pane:focus-previous", "View: Focus Previous Editor Group"),
    ("pane:move-tab-to-next", "View: Move Tab to Next Group"),
    (
        "pane:move-tab-to-previous",
        "View: Move Tab to Previous Group",
    ),
    ("pane:focus", "Focus Editor Group"),
];

fn register_all(r: &mut CommandRegistry, rows: &[(&str, &str)]) -> Result<(), CommandError> {
    for (id, title) in rows {
        r.register(Command::new(id, *title)?)?;
    }
    Ok(())
}

/// Registers the `workbench:*` commands.
///
/// # Errors
/// [`CommandError::Duplicate`] when one is already registered.
pub fn register_commands(r: &mut CommandRegistry) -> Result<(), CommandError> {
    register_all(r, COMMANDS)
}

/// Registers the `pane:*` commands.
///
/// # Errors
/// [`CommandError::Duplicate`] when one is already registered.
pub fn register_pane_commands(r: &mut CommandRegistry) -> Result<(), CommandError> {
    register_all(r, PANE_COMMANDS)
}

/// The layout action a `workbench:*` command runs; `None` for other IDs and for the two
/// commands that take an argument.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<WorkbenchAction> {
    Some(match id.as_str() {
        "workbench:toggle-side-panel" => WorkbenchAction::ToggleSidePanel,
        "workbench:show-connections" => WorkbenchAction::ShowSideView(SideView::Connections),
        "workbench:show-saved-queries" => WorkbenchAction::ShowSideView(SideView::SavedQueries),
        "workbench:show-ai" => WorkbenchAction::ShowSideView(SideView::Ai),
        "workbench:toggle-bottom-panel" => WorkbenchAction::ToggleBottomPanel,
        "workbench:show-results" => WorkbenchAction::ShowPanelView(PanelView::Results),
        "workbench:show-messages" => WorkbenchAction::ShowPanelView(PanelView::Messages),
        "workbench:show-audit" => WorkbenchAction::ShowPanelView(PanelView::Audit),
        "workbench:focus-next-region" => WorkbenchAction::FocusNextRegion,
        "workbench:focus-previous-region" => WorkbenchAction::FocusPreviousRegion,
        "workbench:reset-layout" => WorkbenchAction::ResetLayout,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS.iter().chain(PANE_COMMANDS) {
            assert!(CommandId::new(id).is_ok(), "{id}");
            assert_ne!(*title, "");
        }
        assert!(COMMANDS.iter().all(|(id, _)| id.starts_with("workbench:")));
        assert!(PANE_COMMANDS.iter().all(|(id, _)| id.starts_with("pane:")));
    }

    #[test]
    fn registry_round_trip() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        register_pane_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len() + PANE_COMMANDS.len());
        for (id, title) in COMMANDS.iter().chain(PANE_COMMANDS) {
            assert_eq!(r.get(&CommandId::new(id).unwrap()).unwrap().title(), *title);
        }
        assert_eq!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(
                "workbench:toggle-side-panel".into()
            ))
        );
        assert!(register_pane_commands(&mut r).is_err());
    }

    #[test]
    fn action_for_every_no_arg_id() {
        let args = ["workbench:set-side-width", "workbench:set-panel-height"];
        for (id, _) in COMMANDS {
            let got = action_for(&CommandId::new(id).unwrap());
            assert_eq!(got.is_none(), args.contains(id), "{id}");
        }
        for (id, _) in PANE_COMMANDS {
            assert_eq!(action_for(&CommandId::new(id).unwrap()), None);
        }
        assert_eq!(
            action_for(&CommandId::new("workbench:show-ai").unwrap()),
            Some(WorkbenchAction::ShowSideView(SideView::Ai))
        );
        assert_eq!(
            action_for(&CommandId::new("workbench:show-messages").unwrap()),
            Some(WorkbenchAction::ShowPanelView(PanelView::Messages))
        );
    }
}
