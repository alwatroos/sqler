// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `WorkspaceAction`: every workspace user action, each mapped to a command ID.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::tabs::TabId;

/// A user action on the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// Select a sidebar entry.
    SelectConnection(usize),
    /// Open a session on a sidebar entry and a tab bound to it.
    OpenConnection(usize),
    /// Make a tab the active one.
    ActivateTab(TabId),
    /// Close a tab.
    CloseTab(TabId),
    /// Close the active tab.
    CloseActiveTab,
    /// Activate the next tab.
    NextTab,
    /// Activate the previous tab.
    PreviousTab,
    /// Quit the application.
    Quit,
}

/// The commands: ID and title. IDs of `tab:close`, `tab:next`, `tab:previous` and `app:quit`
/// match the default keymap in `sqler-config`.
const COMMANDS: &[(&str, &str)] = &[
    ("connection:select", "Select Connection"),
    ("connection:open", "Open Connection in New Tab"),
    ("tab:activate", "Go to Tab"),
    ("tab:close", "Close Tab"),
    ("tab:next", "Next Tab"),
    ("tab:previous", "Previous Tab"),
    ("app:quit", "Quit"),
];

impl WorkspaceAction {
    /// The ID of the command this action runs.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        let id = match self {
            Self::SelectConnection(_) => "connection:select",
            Self::OpenConnection(_) => "connection:open",
            Self::ActivateTab(_) => "tab:activate",
            Self::CloseTab(_) | Self::CloseActiveTab => "tab:close",
            Self::NextTab => "tab:next",
            Self::PreviousTab => "tab:previous",
            Self::Quit => "app:quit",
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// Registers the workspace commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        registry.register(Command::new(id, *title)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::{TabConnection, TabKind, TabStrip};
    use sqler_driver_api::sqler_core::DriverId;

    fn tab_id() -> TabId {
        let mut s = TabStrip::default();
        s.open(
            TabConnection {
                name: "n".into(),
                driver: DriverId::new("sqlite").unwrap(),
                color: None,
                environment: None,
            },
            TabKind::Query,
        )
    }

    fn all() -> Vec<WorkspaceAction> {
        vec![
            WorkspaceAction::SelectConnection(0),
            WorkspaceAction::OpenConnection(0),
            WorkspaceAction::ActivateTab(tab_id()),
            WorkspaceAction::CloseTab(tab_id()),
            WorkspaceAction::CloseActiveTab,
            WorkspaceAction::NextTab,
            WorkspaceAction::PreviousTab,
            WorkspaceAction::Quit,
        ]
    }

    #[test]
    fn ids_are_valid() {
        for (id, _) in COMMANDS {
            CommandId::new(id).unwrap();
        }
    }

    #[test]
    fn every_action_command_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        for action in all() {
            assert!(r.get(&action.command_id()).is_some(), "{action:?}");
        }
    }

    #[test]
    fn command_ids_are_unique_per_kind() {
        let ids: Vec<String> = all().iter().map(|a| a.command_id().to_string()).collect();
        // CloseTab and CloseActiveTab intentionally share `tab:close`.
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), COMMANDS.len());
        assert_eq!(ids.len(), COMMANDS.len() + 1);
    }

    #[test]
    fn keymap_defaults_are_present() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        for id in ["tab:close", "tab:next", "tab:previous", "app:quit"] {
            assert!(r.get(&CommandId::new(id).unwrap()).is_some(), "{id}");
        }
    }

    #[test]
    fn registering_twice_is_a_duplicate_error() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert!(matches!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(_))
        ));
    }
}
