// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tab strip: state (always built) and the `GPUI` view (feature `gui`).

mod bar;
mod header;
mod model;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use bar::{MENU, MenuEntry, Trailing, trailing};
pub use header::{Rgb, TabDecor, TabHeader};
pub use model::{
    Group, GroupId, MAX_GROUPS, Tab, TabConnection, TabId, TabKind, TabMode, TabStrip,
};
