// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `ConnectionAction`: every action of the connection manager, each mapped to a command ID.

use sqler_config::{ConnectionId, GroupId, TlsMode};
use sqler_driver_api::sqler_core::{DriverId, Environment};

use super::dialog::Dialog;
use super::form::{FormField, SecretSlot, SshAuthKind};
use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::workspace::Workspace;

/// A user action on the connection manager (sidebar tree, dialogs and the form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionAction {
    /// Open the form for a new connection (in the selected group).
    New,
    /// Open the form for a saved connection.
    Edit(ConnectionId),
    /// Ask whether to delete a connection.
    Delete(ConnectionId),
    /// Delete the connection the open dialog asks about.
    ConfirmDelete,
    /// Test the connection in the form (or open the selected connection's form and test it).
    Test,
    /// Cancel the running test.
    CancelTest,
    /// Validate and save the form.
    Save,
    /// Close the open dialog without applying it.
    Cancel,
    /// Set a text field of the form.
    SetText {
        /// The field.
        field: FormField,
        /// The new text.
        text: String,
    },
    /// Choose the driver in the form.
    SetDriver(DriverId),
    /// Choose the environment in the form.
    SetEnvironment(Option<Environment>),
    /// Choose the group in the form.
    SetGroup(Option<GroupId>),
    /// Choose the TLS mode in the form (`None` = as the target says).
    SetTlsMode(Option<TlsMode>),
    /// Turn the SSH tunnel on or off in the form.
    SetSshEnabled(bool),
    /// Choose how the tunnel authenticates.
    SetSshAuth(SshAuthKind),
    /// Remove a stored secret on save.
    ClearSecret(SecretSlot),
    /// Open the "move to group" dialog for a connection.
    OpenMoveTo(ConnectionId),
    /// Move a connection into a group (`None` = root) as sibling number `index`.
    MoveConnection {
        /// The connection.
        id: ConnectionId,
        /// The destination group.
        to: Option<GroupId>,
        /// The position among the destination's connections (clamped to the end).
        index: usize,
    },
    /// Open the group-name dialog for a new group.
    NewGroup {
        /// The enclosing group.
        parent: Option<GroupId>,
    },
    /// Open the group-name dialog to rename a group.
    RenameGroup(GroupId),
    /// Set the name typed in the group-name dialog.
    SetGroupName(String),
    /// Create or rename the group with the typed name.
    SubmitGroupName,
    /// Delete an empty group.
    DeleteGroup(GroupId),
    /// Expand or collapse a group.
    ToggleGroup(GroupId),
    /// Select a group.
    SelectGroup(GroupId),
    /// Trust the SSH host key the dialog shows and try again.
    TrustHostKey,
    /// Do not trust the SSH host key.
    RejectHostKey,
}

/// The commands: ID and title.
pub const COMMANDS: &[(&str, &str)] = &[
    ("connection:new", "New Connection"),
    ("connection:edit", "Edit Connection"),
    ("connection:delete", "Delete Connection"),
    ("connection:test", "Test Connection"),
    ("connection:cancel-test", "Cancel Connection Test"),
    ("connection:save", "Save Connection"),
    ("connection:cancel", "Close Connection Dialog"),
    ("connection:move-to-group", "Move Connection to Group"),
    ("connection:trust-host-key", "Trust SSH Host Key"),
    ("connection:reject-host-key", "Reject SSH Host Key"),
    ("connection-group:new", "New Connection Group"),
    ("connection-group:rename", "Rename Connection Group"),
    ("connection-group:delete", "Delete Connection Group"),
    (
        "connection-group:toggle",
        "Expand or Collapse Connection Group",
    ),
];

impl ConnectionAction {
    /// The ID of the command this action runs. Field edits belong to `connection:edit`.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(&self) -> CommandId {
        let id = match self {
            Self::New => "connection:new",
            Self::Edit(_)
            | Self::SetText { .. }
            | Self::SetDriver(_)
            | Self::SetEnvironment(_)
            | Self::SetGroup(_)
            | Self::SetTlsMode(_)
            | Self::SetSshEnabled(_)
            | Self::SetSshAuth(_)
            | Self::ClearSecret(_) => "connection:edit",
            Self::Delete(_) | Self::ConfirmDelete => "connection:delete",
            Self::Test => "connection:test",
            Self::CancelTest => "connection:cancel-test",
            Self::Save => "connection:save",
            Self::Cancel => "connection:cancel",
            Self::OpenMoveTo(_) | Self::MoveConnection { .. } => "connection:move-to-group",
            Self::TrustHostKey => "connection:trust-host-key",
            Self::RejectHostKey => "connection:reject-host-key",
            Self::NewGroup { .. } | Self::SetGroupName(_) | Self::SubmitGroupName => {
                "connection-group:new"
            }
            Self::RenameGroup(_) => "connection-group:rename",
            Self::DeleteGroup(_) => "connection-group:delete",
            Self::ToggleGroup(_) | Self::SelectGroup(_) => "connection-group:toggle",
        };
        CommandId::new(id).expect("constant command ID is valid")
    }
}

/// Registers the connection commands.
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

/// The action a command stands for, or `None` when it is not a connection command or does not
/// apply now (`connection:edit` with nothing selected, `connection:save` with no form open).
#[must_use]
pub fn action_for(id: &CommandId, ws: &Workspace) -> Option<ConnectionAction> {
    let selected = ws.sidebar().selected_id().cloned();
    let group = ws.sidebar().selected_group().cloned();
    let form_open = matches!(ws.dialog(), Some(Dialog::Form(_)));
    let host_key_open = matches!(ws.dialog(), Some(Dialog::TrustHostKey { .. }));
    match id.as_str() {
        "connection:new" => Some(ConnectionAction::New),
        "connection:edit" => selected.map(ConnectionAction::Edit),
        "connection:delete" => selected.map(ConnectionAction::Delete),
        "connection:test" => (form_open || selected.is_some()).then_some(ConnectionAction::Test),
        "connection:cancel-test" => form_open.then_some(ConnectionAction::CancelTest),
        "connection:save" => form_open.then_some(ConnectionAction::Save),
        "connection:cancel" => ws.dialog().is_some().then_some(ConnectionAction::Cancel),
        "connection:move-to-group" => selected.map(ConnectionAction::OpenMoveTo),
        "connection:trust-host-key" => host_key_open.then_some(ConnectionAction::TrustHostKey),
        "connection:reject-host-key" => host_key_open.then_some(ConnectionAction::RejectHostKey),
        "connection-group:new" => Some(ConnectionAction::NewGroup { parent: group }),
        "connection-group:rename" => group.map(ConnectionAction::RenameGroup),
        "connection-group:delete" => group.map(ConnectionAction::DeleteGroup),
        "connection-group:toggle" => group.map(ConnectionAction::ToggleGroup),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::SidebarModel;
    use sqler_config::{ConnectionSpec, ConnectionStore};
    use sqler_driver_api::sqler_core::DriverId;

    fn ids() -> (ConnectionId, GroupId) {
        let mut s = ConnectionStore::new();
        let g = s.add_group("g", None).unwrap();
        let c = s
            .add_connection(
                ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x"),
                None,
            )
            .unwrap();
        (c, g)
    }

    fn all() -> Vec<ConnectionAction> {
        let (c, g) = ids();
        vec![
            ConnectionAction::New,
            ConnectionAction::Edit(c.clone()),
            ConnectionAction::Delete(c.clone()),
            ConnectionAction::ConfirmDelete,
            ConnectionAction::Test,
            ConnectionAction::CancelTest,
            ConnectionAction::Save,
            ConnectionAction::Cancel,
            ConnectionAction::SetText {
                field: FormField::Name,
                text: "x".into(),
            },
            ConnectionAction::SetDriver(DriverId::new("sqlite").unwrap()),
            ConnectionAction::SetEnvironment(None),
            ConnectionAction::SetGroup(None),
            ConnectionAction::SetTlsMode(None),
            ConnectionAction::SetSshEnabled(true),
            ConnectionAction::SetSshAuth(SshAuthKind::Agent),
            ConnectionAction::ClearSecret(SecretSlot::Db),
            ConnectionAction::OpenMoveTo(c.clone()),
            ConnectionAction::MoveConnection {
                id: c,
                to: None,
                index: 0,
            },
            ConnectionAction::NewGroup { parent: None },
            ConnectionAction::RenameGroup(g.clone()),
            ConnectionAction::SetGroupName("n".into()),
            ConnectionAction::SubmitGroupName,
            ConnectionAction::DeleteGroup(g.clone()),
            ConnectionAction::ToggleGroup(g.clone()),
            ConnectionAction::SelectGroup(g),
            ConnectionAction::TrustHostKey,
            ConnectionAction::RejectHostKey,
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
    fn every_command_has_an_action() {
        let used: std::collections::HashSet<String> =
            all().iter().map(|a| a.command_id().to_string()).collect();
        for (id, _) in COMMANDS {
            assert!(used.contains(*id), "{id} has no action");
        }
    }

    #[test]
    fn the_required_commands_exist() {
        for id in [
            "connection:new",
            "connection:edit",
            "connection:delete",
            "connection:test",
            "connection:cancel-test",
            "connection:save",
            "connection:cancel",
            "connection:move-to-group",
            "connection-group:new",
            "connection-group:rename",
            "connection-group:delete",
        ] {
            assert!(COMMANDS.iter().any(|(c, _)| *c == id), "{id}");
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

    fn ws() -> Workspace {
        let d = DriverId::new("sqlite").unwrap();
        let mut sidebar = SidebarModel::default();
        let g = sidebar.add_group("g", None).unwrap();
        sidebar
            .add_connection(ConnectionSpec::new("c", d, "x"), Some(&g))
            .unwrap();
        sidebar.clear_selection();
        Workspace::new(sidebar)
    }

    fn act(id: &str, w: &Workspace) -> Option<ConnectionAction> {
        action_for(&CommandId::new(id).unwrap(), w)
    }

    #[test]
    fn commands_that_need_a_selection() {
        let mut w = ws();
        for id in [
            "connection:edit",
            "connection:delete",
            "connection:test",
            "connection:move-to-group",
            "connection-group:rename",
            "connection-group:delete",
            "connection-group:toggle",
        ] {
            assert_eq!(act(id, &w), None, "{id}");
        }
        assert_eq!(act("connection:new", &w), Some(ConnectionAction::New));
        assert_eq!(
            act("connection-group:new", &w),
            Some(ConnectionAction::NewGroup { parent: None })
        );
        w.dispatch(crate::workspace::WorkspaceAction::SelectConnection(0));
        let id = w.sidebar().selected_id().unwrap().clone();
        assert_eq!(
            act("connection:edit", &w),
            Some(ConnectionAction::Edit(id.clone()))
        );
        assert_eq!(
            act("connection:delete", &w),
            Some(ConnectionAction::Delete(id.clone()))
        );
        assert_eq!(act("connection:test", &w), Some(ConnectionAction::Test));
        assert_eq!(
            act("connection:move-to-group", &w),
            Some(ConnectionAction::OpenMoveTo(id))
        );
        assert_eq!(act("connection-group:rename", &w), None);
    }

    #[test]
    fn group_commands_use_the_selected_group() {
        let mut w = ws();
        let g = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(ConnectionAction::SelectGroup(g.clone()));
        assert_eq!(
            act("connection-group:rename", &w),
            Some(ConnectionAction::RenameGroup(g.clone()))
        );
        assert_eq!(
            act("connection-group:delete", &w),
            Some(ConnectionAction::DeleteGroup(g.clone()))
        );
        assert_eq!(
            act("connection-group:toggle", &w),
            Some(ConnectionAction::ToggleGroup(g.clone()))
        );
        assert_eq!(
            act("connection-group:new", &w),
            Some(ConnectionAction::NewGroup { parent: Some(g) })
        );
    }

    #[test]
    fn dialog_commands_need_a_dialog() {
        let mut w = ws();
        for id in [
            "connection:save",
            "connection:cancel",
            "connection:cancel-test",
            "connection:trust-host-key",
            "connection:reject-host-key",
        ] {
            assert_eq!(act(id, &w), None, "{id}");
        }
        w.dispatch_connection(ConnectionAction::New);
        assert_eq!(act("connection:save", &w), Some(ConnectionAction::Save));
        assert_eq!(act("connection:cancel", &w), Some(ConnectionAction::Cancel));
        assert_eq!(act("connection:test", &w), Some(ConnectionAction::Test));
        assert_eq!(
            act("connection:cancel-test", &w),
            Some(ConnectionAction::CancelTest)
        );
        assert_eq!(act("connection:trust-host-key", &w), None);
    }

    #[test]
    fn other_commands_are_not_ours() {
        let w = ws();
        for id in [
            "connection:open",
            "connection:select",
            "tab:close",
            "app:quit",
        ] {
            assert_eq!(act(id, &w), None, "{id}");
        }
    }
}
