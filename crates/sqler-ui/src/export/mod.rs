// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Exporting a result to a file (ADR-0032).
//!
//! The pipeline is plain Rust and unit-tested headless: `plan` holds the dialog, its validation
//! and the rule for running a statement again, `file` the partial-file protocol and `job` the
//! producer and writer joined by a bounded channel. `Workspace` keeps the state of the dialog and
//! of the running export (`workspace/export.rs`); the `GPUI` views live behind the `gui` feature.
//!
//! Rows come from the rows the grid holds, or from a fresh run of the statement on a short-lived
//! session. A statement that `sqler-sql` does not classify `Read` is never run again.

mod file;
mod job;
mod plan;
#[cfg(feature = "gui")]
pub(crate) mod view;

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

pub use file::PartialFile;
pub use job::{
    ExportFailure, ExportOutcome, ExportStop, ExportSummary, FEED_CAPACITY, Feed, LoadedCursor,
    WriteSpec, connect_text, driver_text, fresh_rows, pump_stream, write_feed,
};
pub use plan::{
    Delimiter, ExportDialog, ExportForm, ExportJob, ExportOrigin, ExportScope, ExportSource,
    ExportStatus, ExportTarget, FormatEntry, FreshRun, GridFacts, LoadedRows, MAX_ROWS_PER_INSERT,
    available_formats, display_name, file_stem, fresh_gate, rows_text, start_directory, validate,
};

/// A user command of the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportCommand {
    /// Open the export dialog for the result on screen.
    Open,
    /// Cancel the running export of the active tab.
    Cancel,
}

/// The commands: ID and title.
const COMMANDS: &[(&str, &str)] = &[
    ("grid:export", "Export Results…"),
    ("export:cancel", "Cancel Export"),
];

impl ExportCommand {
    /// The ID of the command.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        let id = match self {
            Self::Open => COMMANDS[0].0,
            Self::Cancel => COMMANDS[1].0,
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// The export command behind a command ID, if it is one.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<ExportCommand> {
    [ExportCommand::Open, ExportCommand::Cancel]
        .into_iter()
        .find(|c| c.command_id() == *id)
}

/// Registers the export commands.
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

    #[test]
    fn ids_are_valid() {
        for (id, _) in COMMANDS {
            CommandId::new(id).unwrap();
        }
    }

    #[test]
    fn every_command_is_registered_and_maps_back() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        for c in [ExportCommand::Open, ExportCommand::Cancel] {
            assert!(r.get(&c.command_id()).is_some(), "{c:?}");
            assert_eq!(action_for(&c.command_id()), Some(c));
        }
        assert_eq!(ExportCommand::Open.command_id().to_string(), "grid:export");
        assert_eq!(
            ExportCommand::Cancel.command_id().to_string(),
            "export:cancel"
        );
        assert_eq!(
            action_for(&CommandId::new("grid:fetch-more").unwrap()),
            None
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

    #[test]
    fn the_grid_commands_do_not_collide() {
        let mut r = CommandRegistry::new();
        crate::grid::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
    }
}
