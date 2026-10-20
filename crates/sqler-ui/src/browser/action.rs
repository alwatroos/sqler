// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `BrowserAction`: object browser actions, each mapped to a command ID.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

use super::menu::MenuEntry;

/// A user action on the object browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrowserAction {
    /// A context menu entry on the selected node.
    Entry(MenuEntry),
    /// Show the children of the selected node.
    Expand,
    /// Hide the children of the selected node.
    Collapse,
    /// Expand or collapse the selected node.
    Toggle,
    /// Select the next row.
    SelectNext,
    /// Select the previous row.
    SelectPrevious,
    /// Open the context menu of the selected node.
    ShowMenu,
    /// Cancel the running refresh.
    CancelRefresh,
    /// Go on in the DROP dialog (the second click on production).
    ConfirmDrop,
    /// Close the DROP dialog without dropping.
    CancelDrop,
    /// Copy the text of the DDL or properties tab.
    CopyTabText,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("object:open-data", "Object: Open Data"),
    ("object:generate-create", "Object: Generate CREATE"),
    ("object:generate-select", "Object: Generate SELECT"),
    ("object:generate-insert", "Object: Generate INSERT"),
    ("object:generate-drop", "Object: Generate DROP"),
    ("object:drop", "Object: Drop"),
    ("object:copy-name", "Object: Copy Name"),
    ("object:refresh", "Object: Refresh"),
    ("object:properties", "Object: Properties"),
    ("object:expand", "Object: Expand"),
    ("object:collapse", "Object: Collapse"),
    ("object:toggle", "Object: Toggle"),
    ("object:select-next", "Object: Select Next"),
    ("object:select-previous", "Object: Select Previous"),
    ("object:show-menu", "Object: Show Context Menu"),
    ("object:cancel-refresh", "Object: Cancel Refresh"),
    ("object:confirm-drop", "Object: Confirm Drop"),
    ("object:cancel-drop", "Object: Cancel Drop"),
    ("object:copy-tab-text", "Object: Copy Tab Text"),
];

impl BrowserAction {
    /// The ID of the command this action runs.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        CommandId::new(self.id_str()).expect("constant command ID is valid")
    }

    fn id_str(self) -> &'static str {
        match self {
            Self::Entry(MenuEntry::OpenData) => "object:open-data",
            Self::Entry(MenuEntry::GenerateCreate) => "object:generate-create",
            Self::Entry(MenuEntry::GenerateSelect) => "object:generate-select",
            Self::Entry(MenuEntry::GenerateInsert) => "object:generate-insert",
            Self::Entry(MenuEntry::GenerateDrop) => "object:generate-drop",
            Self::Entry(MenuEntry::Drop) => "object:drop",
            Self::Entry(MenuEntry::CopyName) => "object:copy-name",
            Self::Entry(MenuEntry::Refresh) => "object:refresh",
            Self::Entry(MenuEntry::Properties) => "object:properties",
            Self::Expand => "object:expand",
            Self::Collapse => "object:collapse",
            Self::Toggle => "object:toggle",
            Self::SelectNext => "object:select-next",
            Self::SelectPrevious => "object:select-previous",
            Self::ShowMenu => "object:show-menu",
            Self::CancelRefresh => "object:cancel-refresh",
            Self::ConfirmDrop => "object:confirm-drop",
            Self::CancelDrop => "object:cancel-drop",
            Self::CopyTabText => "object:copy-tab-text",
        }
    }
}

/// The action a command ID runs, if it is a browser command.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<BrowserAction> {
    Some(match id.as_str() {
        "object:open-data" => BrowserAction::Entry(MenuEntry::OpenData),
        "object:generate-create" => BrowserAction::Entry(MenuEntry::GenerateCreate),
        "object:generate-select" => BrowserAction::Entry(MenuEntry::GenerateSelect),
        "object:generate-insert" => BrowserAction::Entry(MenuEntry::GenerateInsert),
        "object:generate-drop" => BrowserAction::Entry(MenuEntry::GenerateDrop),
        "object:drop" => BrowserAction::Entry(MenuEntry::Drop),
        "object:copy-name" => BrowserAction::Entry(MenuEntry::CopyName),
        "object:refresh" => BrowserAction::Entry(MenuEntry::Refresh),
        "object:properties" => BrowserAction::Entry(MenuEntry::Properties),
        "object:expand" => BrowserAction::Expand,
        "object:collapse" => BrowserAction::Collapse,
        "object:toggle" => BrowserAction::Toggle,
        "object:select-next" => BrowserAction::SelectNext,
        "object:select-previous" => BrowserAction::SelectPrevious,
        "object:show-menu" => BrowserAction::ShowMenu,
        "object:cancel-refresh" => BrowserAction::CancelRefresh,
        "object:confirm-drop" => BrowserAction::ConfirmDrop,
        "object:cancel-drop" => BrowserAction::CancelDrop,
        "object:copy-tab-text" => BrowserAction::CopyTabText,
        _ => return None,
    })
}

/// Registers the object browser commands.
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
    use std::collections::HashSet;

    use super::*;

    const ALL: [BrowserAction; 19] = [
        BrowserAction::Entry(MenuEntry::OpenData),
        BrowserAction::Entry(MenuEntry::GenerateCreate),
        BrowserAction::Entry(MenuEntry::GenerateSelect),
        BrowserAction::Entry(MenuEntry::GenerateInsert),
        BrowserAction::Entry(MenuEntry::GenerateDrop),
        BrowserAction::Entry(MenuEntry::Drop),
        BrowserAction::Entry(MenuEntry::CopyName),
        BrowserAction::Entry(MenuEntry::Refresh),
        BrowserAction::Entry(MenuEntry::Properties),
        BrowserAction::Expand,
        BrowserAction::Collapse,
        BrowserAction::Toggle,
        BrowserAction::SelectNext,
        BrowserAction::SelectPrevious,
        BrowserAction::ShowMenu,
        BrowserAction::CancelRefresh,
        BrowserAction::ConfirmDrop,
        BrowserAction::CancelDrop,
        BrowserAction::CopyTabText,
    ];

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS {
            CommandId::new(id).unwrap();
            assert_ne!(title.len(), 0);
        }
    }

    #[test]
    fn there_are_no_duplicates() {
        let ids: HashSet<_> = COMMANDS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), COMMANDS.len());
        let titles: HashSet<_> = COMMANDS.iter().map(|(_, t)| *t).collect();
        assert_eq!(titles.len(), COMMANDS.len());
    }

    #[test]
    fn every_action_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        assert_eq!(ALL.len(), COMMANDS.len());
        for a in ALL {
            assert!(r.get(&a.command_id()).is_some(), "{a:?}");
        }
    }

    #[test]
    fn every_command_has_an_action_that_maps_back() {
        for (id, _) in COMMANDS {
            let cid = CommandId::new(id).unwrap();
            let action = action_for(&cid).unwrap_or_else(|| panic!("no action for {id}"));
            assert_eq!(action.command_id(), cid);
            assert!(ALL.contains(&action));
        }
        assert_eq!(
            action_for(&CommandId::new("grid:scroll-to-top").unwrap()),
            None
        );
        assert_eq!(action_for(&CommandId::new("object:nope").unwrap()), None);
    }

    #[test]
    fn spec_ids() {
        assert_eq!(
            BrowserAction::Entry(MenuEntry::OpenData)
                .command_id()
                .to_string(),
            "object:open-data"
        );
        assert_eq!(
            BrowserAction::CopyTabText.command_id().to_string(),
            "object:copy-tab-text"
        );
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
