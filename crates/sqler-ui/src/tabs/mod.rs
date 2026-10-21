// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tab strip: state (always built) and the `GPUI` view (feature `gui`).

mod bar;
mod crumbs;
mod header;
mod model;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use bar::{MENU, MenuEntry, Trailing, trailing};
pub use crumbs::{Breadcrumbs, object_segments};
pub use header::{Rgb, TabDecor, TabHeader};
#[cfg(feature = "gui")]
pub(crate) use model::env_badge;
pub use model::{
    Group, GroupId, MAX_GROUPS, Tab, TabConnection, TabId, TabKind, TabMode, TabStrip,
};
