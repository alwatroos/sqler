// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The query commands: run selection, run statement, run all and cancel.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

use super::buffer::RunMode;

/// What a query command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryCommand {
    /// Run part of the buffer.
    Run(RunMode),
    /// Cancel the running query.
    Cancel,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("query:run-selection", "Query: Run Selection"),
    ("query:run-statement", "Query: Run Statement at Cursor"),
    ("query:run-all", "Query: Run All"),
    ("query:cancel", "Query: Cancel"),
];

impl QueryCommand {
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
            Self::Run(RunMode::Selection) => "query:run-selection",
            Self::Run(RunMode::Statement) => "query:run-statement",
            Self::Run(_) => "query:run-all",
            Self::Cancel => "query:cancel",
        }
    }
}

/// The query command an ID stands for, if any.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<QueryCommand> {
    Some(match id.as_str() {
        "query:run-selection" => QueryCommand::Run(RunMode::Selection),
        "query:run-statement" => QueryCommand::Run(RunMode::Statement),
        "query:run-all" => QueryCommand::Run(RunMode::All),
        "query:cancel" => QueryCommand::Cancel,
        _ => return None,
    })
}

/// Registers the query commands.
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

    const ALL: [QueryCommand; 4] = [
        QueryCommand::Run(RunMode::Selection),
        QueryCommand::Run(RunMode::Statement),
        QueryCommand::Run(RunMode::All),
        QueryCommand::Cancel,
    ];

    #[test]
    fn ids_are_valid() {
        for (id, title) in COMMANDS {
            CommandId::new(id).unwrap();
            assert_ne!(title.len(), 0);
        }
    }

    #[test]
    fn every_command_maps_back() {
        assert_eq!(ALL.len(), COMMANDS.len());
        for (id, _) in COMMANDS {
            let cid = CommandId::new(id).unwrap();
            let action = action_for(&cid).unwrap();
            assert_eq!(action.command_id(), cid);
            assert!(ALL.contains(&action));
        }
        assert_eq!(action_for(&CommandId::new("query:nope").unwrap()), None);
        assert_eq!(action_for(&CommandId::new("object:drop").unwrap()), None);
    }

    #[test]
    fn registered_with_titles() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), 4);
        for a in ALL {
            assert!(r.get(&a.command_id()).is_some());
        }
        assert!(matches!(
            register_commands(&mut r),
            Err(CommandError::Duplicate(_))
        ));
    }
}
