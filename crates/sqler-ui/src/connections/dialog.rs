// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Dialog`: the modal state the workspace shows above the sidebar and tabs.

use sqler_config::{ConnectionId, GroupId};

use super::connector::HostKeyPrompt;
use super::form::ConnectionForm;
use crate::workspace::Purpose;

/// What a group-name dialog does with the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupPurpose {
    /// Create a group inside `parent` (`None` = root).
    New {
        /// The enclosing group.
        parent: Option<GroupId>,
    },
    /// Rename this group.
    Rename(GroupId),
}

/// The dialog on screen, if any. At most one is open.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // one dialog at a time; boxing the form buys nothing
pub enum Dialog {
    /// The new/edit connection form.
    Form(ConnectionForm),
    /// "Delete this connection?"
    ConfirmDelete {
        /// The connection to delete.
        id: ConnectionId,
        /// Its name, for the question.
        name: String,
    },
    /// Ask for a group name.
    GroupName {
        /// Create or rename.
        purpose: GroupPurpose,
        /// The name typed so far.
        text: String,
        /// Why the last attempt was rejected.
        error: Option<String>,
    },
    /// Pick the group to move a connection to.
    MoveTo {
        /// The connection to move.
        id: ConnectionId,
    },
    /// "Trust this SSH host key?"
    TrustHostKey {
        /// The key the server presented.
        key: HostKeyPrompt,
        /// What to run again once the key is trusted.
        retry: Purpose,
    },
}
