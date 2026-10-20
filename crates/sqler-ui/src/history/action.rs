// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The history and saved-query commands.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// What a history command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HistoryCommand {
    /// Open the history tab of the active connection.
    OpenHistory,
    /// Open the selected history entry in a new editor tab.
    OpenEntry,
    /// Open the selected history entry in a new editor tab and run it.
    RunEntry,
    /// Switch the history tab between its connection and all connections.
    ToggleAllConnections,
    /// Delete the whole history (after a confirmation).
    ClearHistory,
    /// Save the buffer of the active editor as a named query.
    SaveQuery,
    /// Pick a saved query and open it in a new editor tab.
    OpenSaved,
    /// Rename the selected saved query.
    RenameSaved,
    /// Delete the selected saved query (after a confirmation).
    DeleteSaved,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("history:open", "History: Open Query History"),
    ("history:open-entry", "History: Open Entry in Editor"),
    ("history:run-entry", "History: Run Entry Again"),
    (
        "history:toggle-all-connections",
        "History: Toggle All Connections",
    ),
    ("history:clear", "History: Clear History"),
    ("query:save", "Query: Save Query"),
    ("query:open-saved", "Query: Open Saved Query"),
    ("query:rename-saved", "Query: Rename Saved Query"),
    ("query:delete-saved", "Query: Delete Saved Query"),
];

impl HistoryCommand {
    /// The ID of the command that asks for this.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        CommandId::new(self.id_str()).expect("constant command ID is valid")
    }

    fn id_str(self) -> &'static str {
        match self {
            Self::OpenHistory => "history:open",
            Self::OpenEntry => "history:open-entry",
            Self::RunEntry => "history:run-entry",
            Self::ToggleAllConnections => "history:toggle-all-connections",
            Self::ClearHistory => "history:clear",
            Self::SaveQuery => "query:save",
            Self::OpenSaved => "query:open-saved",
            Self::RenameSaved => "query:rename-saved",
            Self::DeleteSaved => "query:delete-saved",
        }
    }
}

/// The history command an ID stands for, if any.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<HistoryCommand> {
    Some(match id.as_str() {
        "history:open" => HistoryCommand::OpenHistory,
        "history:open-entry" => HistoryCommand::OpenEntry,
        "history:run-entry" => HistoryCommand::RunEntry,
        "history:toggle-all-connections" => HistoryCommand::ToggleAllConnections,
        "history:clear" => HistoryCommand::ClearHistory,
        "query:save" => HistoryCommand::SaveQuery,
        "query:open-saved" => HistoryCommand::OpenSaved,
        "query:rename-saved" => HistoryCommand::RenameSaved,
        "query:delete-saved" => HistoryCommand::DeleteSaved,
        _ => return None,
    })
}

/// Registers the history and saved-query commands. They have no default keybinding.
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

    const ALL: [HistoryCommand; 9] = [
        HistoryCommand::OpenHistory,
        HistoryCommand::OpenEntry,
        HistoryCommand::RunEntry,
        HistoryCommand::ToggleAllConnections,
        HistoryCommand::ClearHistory,
        HistoryCommand::SaveQuery,
        HistoryCommand::OpenSaved,
        HistoryCommand::RenameSaved,
        HistoryCommand::DeleteSaved,
    ];

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS {
            CommandId::new(id).unwrap();
            assert_ne!(title.len(), 0);
        }
    }

    #[test]
    fn every_command_maps_back_in_order() {
        assert_eq!(ALL.len(), COMMANDS.len());
        for (command, (id, _)) in ALL.iter().zip(COMMANDS) {
            let cid = CommandId::new(id).unwrap();
            assert_eq!(action_for(&cid), Some(*command));
            assert_eq!(command.command_id(), cid);
        }
        assert_eq!(action_for(&CommandId::new("history:nope").unwrap()), None);
        assert_eq!(action_for(&CommandId::new("query:run-all").unwrap()), None);
    }

    #[test]
    fn registered_with_titles() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), 9);
        for a in ALL {
            assert!(r.get(&a.command_id()).is_some());
        }
        assert!(matches!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(_))
        ));
    }

    #[test]
    fn titles_follow_the_spec() {
        assert_eq!(COMMANDS[0], ("history:open", "History: Open Query History"));
        assert_eq!(COMMANDS[5], ("query:save", "Query: Save Query"));
    }

    #[test]
    fn query_commands_do_not_clash_with_the_run_commands() {
        let mut r = CommandRegistry::new();
        crate::query::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
    }
}
