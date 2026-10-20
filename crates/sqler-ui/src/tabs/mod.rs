// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tab strip: state (always built) and the `GPUI` view (feature `gui`).

mod header;
mod model;
#[cfg(feature = "gui")]
pub(crate) mod view;

pub use header::{Rgb, TabHeader};
#[cfg(feature = "gui")]
pub(crate) use model::env_badge;
pub use model::{Tab, TabConnection, TabId, TabKind, TabStrip};
