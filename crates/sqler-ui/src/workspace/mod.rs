// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workspace: sidebar, tab strip and the sessions behind the tabs.
//!
//! `Workspace` (state), `WorkspaceAction` and `SessionBridge` are plain Rust. The `GPUI` root view
//! and `run` exist with the `gui` feature.

mod action;
mod agent;
mod ai;
mod ai_session;
mod assist;
mod audit;
mod bridge;
mod browser;
mod chat_history;
#[cfg(feature = "gui")]
pub(crate) mod chrome;
#[cfg(feature = "gui")]
mod drop_dialog;
mod edit;
mod editor;
mod export;
mod history;
mod import;
mod plugins;
mod query;
mod state;
#[cfg(test)]
pub(crate) mod testing;
mod tx;
mod update;
#[cfg(feature = "gui")]
mod view;

pub use action::{WorkspaceAction, register_commands};
#[cfg(any(feature = "gui", test))]
pub(crate) use ai::NO_PROVIDER;
pub use bridge::{PendingConnect, PendingTrust, SessionBridge};
pub use browser::{BrowserJob, BrowserJobResult, BrowserNote, CatalogResult, OpenDataJob};
pub use chat_history::ChatList;
pub use edit::{
    ApplyFinished, ClosePrompt, CloseTarget, EditConfirm, EditPreview, EditPushState,
    lost_apply_outcome,
};
pub use state::{Effect, Purpose, RequestId, SecretOp, Workspace};
#[cfg(feature = "gui")]
pub(crate) use view::WorkspaceView;
#[cfg(feature = "screenshots")]
pub use view::scene;
#[cfg(feature = "gui")]
pub use view::{Startup, init, run};
