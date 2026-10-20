// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `WorkspaceAction`: every workspace user action, each mapped to a command ID.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::tabs::TabId;
use sqler_config::ThemePreference;

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
    /// Open a results-grid tab with a synthetic 1M-row result.
    OpenSyntheticGrid,
    /// Quit the application.
    Quit,
    /// Open the settings tab (or switch to it).
    OpenSettings,
    /// Close the settings tab.
    CloseSettings,
    /// Switch between the light and the dark theme.
    ToggleThemeMode,
    /// Choose the theme preference.
    SetTheme(ThemePreference),
    /// Set the UI font size in points.
    SetUiFontSize(u16),
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
    ("settings:open", "Open Settings"),
    ("theme:toggle-mode", "Toggle Light/Dark Theme"),
    ("settings:set-theme", "Set Theme"),
    ("settings:set-ui-font-size", "Set UI Font Size"),
    ("dev:open-synthetic-grid", "Open Synthetic 1M-Row Grid"),
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
            Self::CloseTab(_) | Self::CloseActiveTab | Self::CloseSettings => "tab:close",
            Self::NextTab => "tab:next",
            Self::PreviousTab => "tab:previous",
            Self::Quit => "app:quit",
            Self::OpenSettings => "settings:open",
            Self::ToggleThemeMode => "theme:toggle-mode",
            Self::SetTheme(_) => "settings:set-theme",
            Self::SetUiFontSize(_) => "settings:set-ui-font-size",
            Self::OpenSyntheticGrid => "dev:open-synthetic-grid",
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
            WorkspaceAction::OpenSettings,
            WorkspaceAction::CloseSettings,
            WorkspaceAction::ToggleThemeMode,
            WorkspaceAction::SetTheme(ThemePreference::Dark),
            WorkspaceAction::SetUiFontSize(14),
            WorkspaceAction::OpenSyntheticGrid,
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
        // CloseTab, CloseActiveTab and CloseSettings intentionally share `tab:close`.
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), COMMANDS.len());
        assert_eq!(ids.len(), COMMANDS.len() + 2);
    }

    #[test]
    fn keymap_defaults_are_present() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        for id in [
            "tab:close",
            "tab:next",
            "tab:previous",
            "app:quit",
            "settings:open",
        ] {
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
