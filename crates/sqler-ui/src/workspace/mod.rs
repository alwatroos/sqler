// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workspace: sidebar, tab strip and the sessions behind the tabs.
//!
//! `Workspace` (state), `WorkspaceAction` and `SessionBridge` are plain Rust. The `GPUI` root view
//! and `run` exist with the `gui` feature.

mod action;
mod bridge;
mod state;
#[cfg(test)]
mod testing;
#[cfg(feature = "gui")]
mod view;

pub use action::{WorkspaceAction, register_commands};
pub use bridge::{PendingConnect, PendingTrust, SessionBridge};
pub use state::{Effect, Purpose, RequestId, SecretOp, Workspace};
#[cfg(feature = "gui")]
pub(crate) use view::WorkspaceView;
#[cfg(feature = "gui")]
pub use view::{Startup, init, run};
