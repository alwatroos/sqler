// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin permission grants (ADR-0059): the typed book over `plugin-grants.toml`, the model of
//! the dialog (`GrantPrompt`) and, behind the `gui` feature, the overlay that draws it.
//!
//! Approval is all-or-nothing: Approve grants exactly the capabilities the prompt lists, Deny
//! grants nothing and is remembered. Escape is Deny. No key is ever bound to Approve.

mod book;
mod prompt;
#[cfg(feature = "gui")]
pub(crate) mod view;

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

pub use book::{GrantBook, GrantBookError, GrantStatus};
#[cfg(test)]
pub(crate) use prompt::fixture_manifest;
#[cfg(test)]
pub(crate) use prompt::sample_prompt;
pub use prompt::{CapabilityLine, GrantPrompt};

/// The answer to the permission dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GrantCommand {
    /// Grant what the dialog lists.
    Approve,
    /// Grant nothing, and remember the answer.
    Deny,
}

/// The commands: ID and title.
const COMMANDS: &[(&str, &str)] = &[
    ("plugins:approve-grant", "Approve Plugin Permissions"),
    ("plugins:deny-grant", "Deny Plugin Permissions"),
];

impl GrantCommand {
    /// The ID of the command.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        let id = match self {
            Self::Approve => COMMANDS[0].0,
            Self::Deny => COMMANDS[1].0,
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// The grant command behind a command ID, if it is one.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<GrantCommand> {
    [GrantCommand::Approve, GrantCommand::Deny]
        .into_iter()
        .find(|c| c.command_id() == *id)
}

/// Registers the grant commands that may be listed and rebound: Deny only.
///
/// `plugins:approve-grant` stays an ID (the dialog's pointer Approve and the tests use it) but is deliberately not
/// registered, so the palette never lists it and a key override for it is rejected as an unknown command. The
/// keyboard route to Approve is the typed plugin id ([`GrantPrompt::confirms`]).
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        if *id == COMMANDS[0].0 {
            continue;
        }
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
        assert_eq!(r.len(), COMMANDS.len() - 1);
        assert!(r.get(&GrantCommand::Deny.command_id()).is_some());
        // Approve is never registered: not in the palette, not rebindable.
        assert!(r.get(&GrantCommand::Approve.command_id()).is_none());
        for c in [GrantCommand::Approve, GrantCommand::Deny] {
            assert_eq!(action_for(&c.command_id()), Some(c));
        }
        assert_eq!(
            GrantCommand::Approve.command_id().to_string(),
            "plugins:approve-grant"
        );
        assert_eq!(
            GrantCommand::Deny.command_id().to_string(),
            "plugins:deny-grant"
        );
        assert_eq!(action_for(&CommandId::new("grid:export").unwrap()), None);
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
    fn the_export_commands_do_not_collide() {
        let mut r = CommandRegistry::new();
        crate::export::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
    }
}
