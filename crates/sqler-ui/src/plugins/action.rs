// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginCommand`: the commands of the plugin manager, each mapped to a command ID (ADR-0050 module pattern).
//! The permission dialog's own commands live in `grant` (`plugins:deny-grant` is registered there;
//! `plugins:approve-grant` is deliberately not).

use crate::command::{Command, CommandError, CommandId, CommandRegistry};

/// A command of the plugin manager. The row-bound ones act on the selected row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PluginCommand {
    /// Discover the plugins again.
    Refresh,
    /// Switch the selected plugin on.
    Enable,
    /// Switch the selected plugin off.
    Disable,
    /// Show the permission dialog for the selected plugin.
    ReviewPermissions,
    /// Take back the approval of the selected plugin.
    Revoke,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("plugins:refresh", "Plugins: Rescan"),
    ("plugins:enable", "Plugins: Enable Selected Plugin"),
    ("plugins:disable", "Plugins: Disable Selected Plugin"),
    (
        "plugins:review-permissions",
        "Plugins: Review Permissions of Selected Plugin",
    ),
    (
        "plugins:revoke",
        "Plugins: Revoke Permissions of Selected Plugin",
    ),
];

impl PluginCommand {
    /// Every command.
    pub const ALL: [PluginCommand; 5] = [
        Self::Refresh,
        Self::Enable,
        Self::Disable,
        Self::ReviewPermissions,
        Self::Revoke,
    ];

    fn index(self) -> usize {
        match self {
            Self::Refresh => 0,
            Self::Enable => 1,
            Self::Disable => 2,
            Self::ReviewPermissions => 3,
            Self::Revoke => 4,
        }
    }

    /// The ID of the command.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        CommandId::new(COMMANDS[self.index()].0).expect("constant command ID is valid")
    }
}

/// The manager command behind a command ID, if it is one.
#[must_use]
pub fn action_for(id: &CommandId) -> Option<PluginCommand> {
    PluginCommand::ALL
        .into_iter()
        .find(|c| c.command_id() == *id)
}

/// Registers the manager commands and the permission dialog's Deny command.
///
/// # Errors
///
/// [`CommandError::Duplicate`] when one of the IDs is already registered.
pub fn register_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for (id, title) in COMMANDS {
        registry.register(Command::new(id, *title)?)?;
    }
    super::grant::register_commands(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::grant::GrantCommand;

    #[test]
    fn ids_are_valid_and_round_trip() {
        for (id, title) in COMMANDS {
            assert!(CommandId::new(id).is_ok(), "{id}");
            assert!(id.starts_with("plugins:"));
            assert_ne!(*title, "");
        }
        for c in PluginCommand::ALL {
            assert_eq!(action_for(&c.command_id()), Some(c));
        }
        assert_eq!(PluginCommand::ALL.len(), COMMANDS.len());
        assert_eq!(action_for(&CommandId::new("grid:export").unwrap()), None);
        assert_eq!(action_for(&GrantCommand::Deny.command_id()), None);
    }

    #[test]
    fn the_approve_id_is_not_registered_and_not_in_the_palette() {
        let mut r = CommandRegistry::new();
        register_commands(&mut r).unwrap();
        assert_eq!(r.len(), COMMANDS.len() + 1);
        assert!(r.get(&GrantCommand::Deny.command_id()).is_some());
        let approve = GrantCommand::Approve.command_id();
        assert!(r.get(&approve).is_none());
        assert!(r.iter().all(|c| c.id() != &approve));
        // The palette lists only registered commands.
        assert!(r.iter().all(|c| !c.id().as_str().contains("approve")));
        // `run_command` never maps it to a manager command either.
        assert_eq!(action_for(&approve), None);
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
    fn the_workbench_show_command_is_registered_by_the_workbench() {
        let mut r = CommandRegistry::new();
        crate::workbench::register_commands(&mut r).unwrap();
        register_commands(&mut r).unwrap();
        let show = CommandId::new("workbench:show-plugins").unwrap();
        assert_eq!(r.get(&show).unwrap().title(), "View: Show Plugins");
    }
}
