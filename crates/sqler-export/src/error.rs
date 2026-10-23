// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the writers.

use std::io;

use sqler_sql::DdlError;

/// Why an export failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The sink failed.
    #[error("write failed: {0}")]
    Io(#[from] io::Error),
    /// The [`CancelToken`](crate::CancelToken) was set.
    #[error("export cancelled")]
    Cancelled,
    /// The options cannot be used with the format.
    #[error("invalid export options: {0}")]
    InvalidOptions(String),
    /// A format with this id is already registered.
    #[error("format {0:?} is already registered")]
    DuplicateFormat(String),
    /// No format has this id.
    #[error("unknown format {0:?}")]
    UnknownFormat(String),
    /// A row has another number of values than there are columns.
    #[error("row has {got} values, expected {expected}")]
    RowWidth {
        /// The number of columns.
        expected: usize,
        /// The number of values in the row.
        got: usize,
    },
    /// The calls came in the wrong order (for example a batch before `begin`).
    #[error("{0}")]
    State(&'static str),
    /// A limit of the format was passed (for example the row count of a worksheet).
    #[error("{0}")]
    Limit(String),
    /// A value cannot be stored in its column.
    #[error("column {column:?}: {reason}")]
    Unsupported {
        /// The column name.
        column: String,
        /// Why the value does not fit.
        reason: String,
    },
    /// The encoder failed (`Parquet`).
    #[error("encoding failed: {0}")]
    Encode(String),
    /// A plugin format failed. The text is already safe to show to the user.
    #[error("{0}")]
    Plugin(String),
    /// An identifier could not be quoted.
    #[error(transparent)]
    Sql(#[from] DdlError),
}
