// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The connection manager: the [`Connector`] contract, the connection form, dialogs and the
//! `connection:*` commands.
//!
//! Everything here is plain Rust and unit-tested headless. The connect pipeline (secret, tunnel,
//! driver) is not in this crate: `sqler-app` implements [`Connector`] and the UI only calls it
//! through `SessionBridge`, so `sqler-ui` never depends on a driver or on `sqler-tunnel`.

mod action;
mod connector;
mod dialog;
mod form;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use action::{COMMANDS, ConnectionAction, action_for, register_commands};
pub(crate) use connector::from_profile_secret;
pub use connector::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};
pub use dialog::{Dialog, GroupPurpose};
pub use form::{
    COLOR_PRESETS, ConnectionForm, FieldError, FormField, FormMode, SecretField, SecretSlot,
    SshAuthKind, TestStatus, ValidForm,
};
