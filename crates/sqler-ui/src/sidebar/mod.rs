// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The connection sidebar: state (always built) and the `GPUI` view (feature `gui`).

mod model;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use model::{
    ConnectionStatus, GroupOption, RowKind, Selection, SidebarEntry, SidebarItem, SidebarModel,
    SidebarRow, changed_range, key_command,
};
