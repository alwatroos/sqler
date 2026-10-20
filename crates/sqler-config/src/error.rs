// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Errors raised while locating, reading, validating or writing configuration files.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The platform configuration directory could not be determined.
    #[error("could not determine the platform configuration directory")]
    NoConfigDir,
    /// A file exists but could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A file or its directory could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The file that failed to write.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// A file is not valid TOML or has the wrong shape or types.
    #[error("{}: {message}", path.display())]
    Parse {
        /// The malformed file.
        path: PathBuf,
        /// The parser message, including line and column when known.
        message: String,
    },
    /// A file parsed but a value is not acceptable.
    #[error("{}: {message}", path.display())]
    Invalid {
        /// The file holding the bad value.
        path: PathBuf,
        /// What is wrong and where.
        message: String,
    },
    /// A command id is not of the form `category:verb-object`.
    #[error("invalid command id {0:?}: expected `category:verb-object` in lowercase")]
    InvalidCommandId(String),
    /// A key chord could not be parsed.
    #[error("invalid key chord {chord:?}: {reason}")]
    InvalidKeyChord {
        /// The offending chord text.
        chord: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A value could not be serialised to TOML.
    #[error("cannot serialize {}: {message}", path.display())]
    Serialize {
        /// The file that was being produced.
        path: PathBuf,
        /// The serialiser message.
        message: String,
    },
}
