// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use thiserror::Error;

/// Errors raised by the domain types in this crate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// A driver id was empty or contained characters outside `[a-z0-9-]`.
    #[error("invalid driver id: {0}")]
    InvalidDriverId(String),
    /// A row did not have as many values as the result has columns.
    #[error("row has {actual} values but the result has {expected} columns")]
    RowWidth {
        /// Number of columns in the result.
        expected: usize,
        /// Number of values in the rejected row.
        actual: usize,
    },
    /// A text could not be parsed or a part was out of range for a typed value.
    #[error("invalid {kind} value: {input}")]
    InvalidValue {
        /// One of `date`, `time`, `timestamp`, `timestamptz` or `decimal`.
        kind: &'static str,
        /// The rejected input.
        input: String,
    },
}
