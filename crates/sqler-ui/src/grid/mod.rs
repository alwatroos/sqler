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

mod ingest;
mod model;
mod synthetic;

pub use ingest::{IngestSummary, ingest, spawn_ingest};
pub use model::{GridModel, GridSink, IngestState, Viewport, grid_channel};
pub use synthetic::SyntheticSource;
