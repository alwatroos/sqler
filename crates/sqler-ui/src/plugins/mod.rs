// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugins in the UI: the permission dialog and the book of what the user approved (ADR-0059).

pub mod action;
pub mod approval;
pub mod backend;
pub mod contrib;
pub mod grant;
pub mod manager;
pub mod menu;
pub mod panel;
pub mod status;
pub mod widget;

pub use action::{PluginCommand, register_commands};
pub use approval::{
    PluginWriteAsk, PluginWriteRequest, WriteInbox, WriteQueue, WriteSender, WriteStage,
    write_channel,
};

pub use backend::{
    NO_UI, PanelRender, PluginProblem, PluginRow, PluginSnapshot, PluginUiEvent, PluginsBackend,
    PluginsStartup, RowState, UiInbox, UiSender, ui_channel,
};
