// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin manager (ADR-0060): the headless state behind the Plugins side view and the permission dialog, and, behind
//! the `gui` feature, the view that draws the list.
//!
//! The state never touches a file or a lock: every call that does goes out as a [`PluginEffect`], runs on the
//! background executor and comes back as a [`PluginDone`]. Nothing runs before the first frame.

#[cfg(any(test, feature = "screenshots"))]
pub(crate) mod fixture;
mod state;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use state::{
    GrantDialog, PluginDone, PluginEffect, PluginsState, RowGrant, parse_setting, row_grant,
};
