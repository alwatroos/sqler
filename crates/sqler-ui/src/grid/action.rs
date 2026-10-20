// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridAction`: grid-local user actions, each mapped to a command ID.

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// A user action on a results grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GridAction {
    /// Scroll to the first row.
    ScrollToTop,
    /// Scroll to the last row.
    ScrollToEnd,
}

/// The commands: ID and title.
const COMMANDS: &[(&str, &str)] = &[
    ("grid:scroll-to-top", "Scroll Results to Top"),
    ("grid:scroll-to-end", "Scroll Results to End"),
];

impl GridAction {
    /// The ID of the command this action runs.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        let id = match self {
            Self::ScrollToTop => "grid:scroll-to-top",
            Self::ScrollToEnd => "grid:scroll-to-end",
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// Registers the grid commands.
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

    const ALL: [GridAction; 2] = [GridAction::ScrollToTop, GridAction::ScrollToEnd];

    #[test]
    fn ids_are_valid() {
        for (id, _) in COMMANDS {
            CommandId::new(id).unwrap();
        }
    }

    #[test]
    fn every_action_is_registered() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len());
        for a in ALL {
            assert!(r.get(&a.command_id()).is_some(), "{a:?}");
        }
        assert_eq!(ALL[0].command_id().to_string(), "grid:scroll-to-top");
        assert_eq!(ALL[1].command_id().to_string(), "grid:scroll-to-end");
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
