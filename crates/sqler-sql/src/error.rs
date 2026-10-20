// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of DDL generation.

use sqler_core::ObjectKind;

use crate::{Dialect, Script};

/// Why a script could not be generated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DdlError {
    /// An identifier is empty or contains a NUL character.
    #[error("invalid identifier {name:?}: must be non-empty and free of NUL")]
    InvalidIdentifier {
        /// The rejected name.
        name: String,
    },
    /// The dialect has no such script for this kind of object.
    #[error(
        "{} scripts for {} objects are not supported on {}",
        script.as_str(),
        kind.as_str(),
        dialect.as_str()
    )]
    Unsupported {
        /// The dialect.
        dialect: Dialect,
        /// The kind of object.
        kind: ObjectKind,
        /// The requested script.
        script: Script,
    },
    /// A trigger or index needs the table it belongs to (`parent.table`).
    #[error("{} needs the table it belongs to", kind.as_str())]
    MissingTable {
        /// The kind of object.
        kind: ObjectKind,
    },
    /// The table has no columns, which `MySQL` and `SQLite` cannot create.
    #[error("{} cannot create a table without columns", dialect.as_str())]
    EmptyTable {
        /// The dialect.
        dialect: Dialect,
    },
    /// This kind needs the server's definition text.
    #[error("{} needs the server's definition text", kind.as_str())]
    MissingDefinition {
        /// The kind of object.
        kind: ObjectKind,
    },
}
