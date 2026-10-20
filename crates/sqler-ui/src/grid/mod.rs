// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A framework-independent results model for the grid.
//!
//! [`grid_channel`] returns a [`GridModel`] (owned by the UI thread) and a [`GridSink`] (owned by
//! ingestion). [`ingest()`] reads any `QueryStream` into the sink on any thread or runtime;
//! [`spawn_ingest()`] runs it on a dedicated thread. The UI calls [`GridModel::poll`], which never
//! waits, then reads only the visible rows with [`GridModel::viewport`]. Rows are kept in batches,
//! so a 1M-row result is never copied while it grows. [`SyntheticSource`] generates rows for tests
//! and benchmarks.

mod action;
mod cell;
mod control;
mod ingest;
mod layout;
mod model;
mod selection;
mod stats;
mod synthetic;
mod tab;
#[cfg(feature = "gui")]
pub(crate) mod view;
mod viewport;

pub use action::{GridAction, register_commands};
pub use cell::{ColumnHeader, MAX_CELL_CHARS, cell_text, group_thousands, type_label};
pub use control::{Applied, GridControls};
pub use ingest::{IngestSummary, ingest, spawn_ingest};
pub use layout::{ColumnLayout, MAX_COLUMN_PX, MIN_COLUMN_PX, default_width, widest_text};
pub use model::{GridModel, GridSink, IngestState, Viewport, grid_channel};
pub use selection::{
    CellRef, DisplayMap, DisplayRect, IdentityRows, Motion, RowOrder, Selection, SelectionKind,
    key_command, scroll_row,
};
pub use stats::{ProbeReport, RenderStats, nearest_rank};
pub use synthetic::SyntheticSource;
pub use tab::{
    GridTab, POLL_BUDGET, PollOutcome, SYNTHETIC_BATCH, SYNTHETIC_COLUMNS, SYNTHETIC_ROWS,
};
pub use viewport::{ROW_HEIGHT_PX, ScrollViewport};
