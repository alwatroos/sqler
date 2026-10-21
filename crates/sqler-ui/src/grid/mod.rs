// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A framework-independent results model for the grid.
//!
//! [`grid_channel_with`] returns a [`GridModel`] (owned by the UI thread) and a [`GridSink`] (owned
//! by ingestion). [`ingest()`] reads any `QueryStream` into the sink on any thread or runtime;
//! [`spawn_ingest()`] runs it on a dedicated thread. The UI calls [`GridModel::poll`], which never
//! waits, then reads only the visible rows with [`GridModel::viewport`].
//!
//! The model is bounded: [`GridLimits`] sets a page of rows and bytes, ingestion sends rows only
//! against that credit and, when it runs out, pauses without reading the stream further until
//! [`GridModel::fetch_more`] grants the next page. Rows are stored compactly per batch (16 bytes
//! per cell, text and binary data in per-batch arenas) and read as [`RowRef`] and [`CellRef`], so
//! a 1M-row result is never copied while it grows. [`SyntheticSource`] generates rows for tests and
//! benchmarks.

mod action;
mod cell;
mod control;
mod copy;
mod detail;
#[cfg(feature = "gui")]
pub(crate) mod detail_view;
mod filter;
mod ingest;
mod layout;
mod model;
mod order;
mod selection;
mod sort;
mod stats;
mod store;
mod synthetic;
mod tab;
#[cfg(feature = "gui")]
pub(crate) mod view;
mod viewport;

pub use action::{GridAction, grid_key_action, register_commands};
pub use cell::{ColumnHeader, MAX_CELL_CHARS, cell_text, group_thousands, type_label};
pub use control::{Applied, GridControls};
pub use copy::{
    CopyError, CopyFormat, CopyJob, CopyLimits, CopyOutput, GATHER_SLICE_CELLS, InsertTarget,
    NOTIFY_CELLS, PLACEHOLDER_TABLE, SYNC_CELLS, copy_key_format, copy_now, register_copy_commands,
    success_notice,
};
pub use detail::{
    CellDetail, DETAIL_BINARY_CAP, DETAIL_LINE_CHARS, DETAIL_TEXT_CAP, DetailCap, DetailKind,
    DetailLine, HEX_BYTES_PER_LINE, JSON_PRETTY_CAP, copy_text, human_bytes,
};
pub use filter::{CompiledFilter, Condition, FilterOp, FilterSpec, QuickFilter};
pub use ingest::{IngestSummary, ingest, spawn_ingest};
pub use layout::{ColumnLayout, MAX_COLUMN_PX, MIN_COLUMN_PX, default_width, widest_text};
pub use model::{
    GridCancel, GridLimits, GridModel, GridSink, IngestState, Viewport, grid_channel,
    grid_channel_with,
};
pub use order::{DisplayRows, GridOrder, OrderProgress};
pub use selection::{
    CellPos, DisplayMap, DisplayRect, IdentityRows, Motion, RowOrder, Selection, SelectionKind,
    key_command, scroll_row,
};
pub use sort::{Collation, SortDirection, SortKey, SortSpec, compare_cells};
pub use stats::{ProbeReport, RenderStats, nearest_rank};
pub use store::{CellRef, RowRef};
pub use synthetic::SyntheticSource;
pub use tab::{
    GridRows, GridTab, POLL_BUDGET, PollOutcome, SYNTHETIC_BATCH, SYNTHETIC_COLUMNS, SYNTHETIC_ROWS,
};
pub use viewport::{ROW_HEIGHT_PX, ScrollViewport};
pub mod edit;
