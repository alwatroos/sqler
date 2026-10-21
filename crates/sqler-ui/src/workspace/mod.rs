// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workspace: sidebar, tab strip and the sessions behind the tabs.
//!
//! `Workspace` (state), `WorkspaceAction` and `SessionBridge` are plain Rust. The `GPUI` root view
//! and `run` exist with the `gui` feature.

mod action;
mod bridge;
mod browser;
#[cfg(feature = "gui")]
mod drop_dialog;
mod edit;
mod editor;
mod export;
mod history;
mod query;
mod state;
#[cfg(test)]
pub(crate) mod testing;
mod tx;
#[cfg(feature = "gui")]
mod view;

pub use action::{WorkspaceAction, register_commands};
pub use bridge::{PendingConnect, PendingTrust, SessionBridge};
pub use browser::{BrowserJob, BrowserJobResult, BrowserNote, CatalogResult, OpenDataJob};
pub use edit::{
    ApplyFinished, ClosePrompt, CloseTarget, EditConfirm, EditPreview, EditPushState,
    lost_apply_outcome,
};
pub use state::{Effect, Purpose, RequestId, SecretOp, Workspace};
#[cfg(feature = "gui")]
pub(crate) use view::WorkspaceView;
#[cfg(feature = "gui")]
pub use view::{Startup, init, run};
