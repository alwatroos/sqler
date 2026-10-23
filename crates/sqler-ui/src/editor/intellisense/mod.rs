// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Intellisense for the SQL editor, without the GUI: the adapter between the editor and the engine
//! ([`sqler_completion`]) over the connection's catalog ([`sqler_catalog`]).
//!
//! - [`CatalogSchema`] is a [`sqler_completion::SchemaSource`] over one [`CatalogSnapshot`](sqler_catalog::CatalogSnapshot).
//!   Take a fresh one for every request (ADR-0022), so a refresh shows up on the next keystroke.
//! - [`keys_for`], [`prefetch_keys`] and [`load`] turn what the engine reports as missing into catalog loads.
//! - [`complete_popup`], [`hover_markdown`] and [`code_name`] convert engine answers into plain data that the
//!   GPUI providers map onto the editor's LSP types.
//! - [`TabIntel`] is the per-tab bookkeeping (which loads were asked, which snapshot generation was seen).
//! - [`synthetic`] is a 5 000-table source for the latency bench and tests.
//!
//! Nothing here touches GPUI, so all of it is unit-tested headless.

mod hover;
mod keys;
mod plugin;
mod popup;
mod schema;
pub mod synthetic;
mod tab;

use std::time::Duration;

pub use hover::{code_name, hover_markdown};
pub use keys::{bootstrap_keys, keys_for, load, prefetch_keys, prefetch_keys_skipping};
pub use plugin::{
    PLUGIN_POPUP_LIMIT, PluginAnswers, PluginPopupItem, merge_plugin_items, plugin_request,
};
pub use popup::{
    Popup, PopupItem, complete_popup, flatten_snippet, highlight_ranges, is_completion_trigger,
};
pub use schema::CatalogSchema;
pub use tab::TabIntel;

/// The most items a popup shows.
pub const COMPLETION_LIMIT: usize = 200;
/// The most tables whose columns one prefetch pass loads.
pub const PREFETCH_LIMIT: usize = 64;
/// How long after the last edit the diagnostics are recomputed.
pub const DIAGNOSE_DEBOUNCE: Duration = Duration::from_millis(300);
