// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Result writers for `CSV`, `JSON`, `SQL` `INSERT`, Markdown tables, `Parquet` (feature
//! `parquet`) and `XLSX` (feature `xlsx`).
//!
//! A [`RowWriter`] is a push-style renderer: `begin` with the columns, `write_batch` with rows
//! any number of times, `finish`. A [`Format`] makes a writer on any `io::Write` sink, and a
//! [`Registry`] finds formats by id. The [`Exporter`] drives a writer and does the shared
//! work: call order, row width, a 64 KiB buffer, byte counting, cancel ([`CancelToken`]) and
//! progress ([`Progress`]). Writers keep no rows, so memory does not depend on the row count.
//!
//! ```
//! use sqler_core::{Column, DataType, Row, Value};
//! use sqler_export::{CsvFormat, ExportOptions, export_to_vec};
//!
//! let columns = [Column::new("id", DataType::Integer), Column::new("name", DataType::Text)];
//! let rows = [Row(vec![Value::Int(1), Value::Text("a,b".into())])];
//! let csv = export_to_vec(&CsvFormat, &ExportOptions::default(), &columns, &rows)?;
//! assert_eq!(String::from_utf8(csv)?, "id,name\r\n1,\"a,b\"\r\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Each [`Value`](sqler_core::Value) variant has a rendering in every format; see
//! `docs/src/architecture/export.md`. `SQL` literals come from `sqler_sql::sql_literal` and
//! never leave a string open.

#![forbid(unsafe_code)]

mod csv;
mod error;
mod json;
mod markdown;
mod options;
#[cfg(feature = "parquet")]
mod parquet;
mod registry;
mod render;
mod sql;
#[cfg(any(feature = "parquet", feature = "xlsx"))]
mod temporal;
mod writer;
#[cfg(feature = "xlsx")]
mod xlsx;

pub use csv::CsvFormat;
pub use error::ExportError;
pub use json::JsonFormat;
pub use markdown::MarkdownFormat;
pub use options::{
    CsvOptions, ExportOptions, LineEnding, MarkdownOptions, ParquetCompression, ParquetOptions,
    SqlOptions,
};
#[cfg(feature = "parquet")]
pub use parquet::ParquetFormat;
pub use registry::{CSV, JSON, MARKDOWN, PARQUET, Registry, SQL, XLSX};
pub use sql::SqlFormat;
pub use writer::{
    CancelToken, ExportControl, Exporter, Format, Progress, RowWriter, Sink, Summary, export_to_vec,
};
#[cfg(feature = "xlsx")]
pub use xlsx::XlsxFormat;
