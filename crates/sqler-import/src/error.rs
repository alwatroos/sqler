// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The typed error of the importers. No text quotes file content.

use std::io::ErrorKind;

use thiserror::Error;

/// Why a foreign file could not be read or recognised.
///
/// The messages name a position or a limit, never a piece of the file: a foreign file may hold a
/// password next to the line that failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ImportError {
    /// The file could not be opened or read.
    #[error("the file could not be read: {kind}")]
    Io {
        /// What the operating system reported.
        kind: ErrorKind,
    },
    /// The path is not a regular file (a directory, a pipe, a device).
    #[error("the path is not a regular file")]
    NotAFile,
    /// The file is larger than the read limit.
    #[error("the file is larger than {limit} bytes")]
    TooLarge {
        /// The limit in bytes ([`crate::MAX_FILE_BYTES`]).
        limit: u64,
    },
    /// The file is not valid UTF-8.
    #[error("the file is not valid UTF-8")]
    NotUtf8,
    /// The file is not well-formed `JSON`.
    #[error("malformed file at line {line}, column {column}: {message}")]
    Malformed {
        /// 1-based line of the problem.
        line: usize,
        /// 1-based column of the problem.
        column: usize,
        /// A fixed description of the kind of problem.
        message: String,
    },
    /// The file parses but is not what the source expects.
    #[error("{0}")]
    UnrecognisedFormat(&'static str),
    /// The file holds more entries than the importer accepts.
    #[error("the file holds more than {limit} entries")]
    TooManyEntries {
        /// The limit ([`crate::MAX_ENTRIES`]).
        limit: usize,
    },
}
