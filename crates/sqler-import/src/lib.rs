// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Imports saved connections from other database tools (ADR-0067).
//!
//! The import has three pure stages. A [`Source`] parses the bytes of a foreign file into a
//! [`SourceReport`]. [`plan()`] checks the report against the current `ConnectionStore` and the
//! drivers the caller has, and gives an [`ImportPlan`], the dry run a wizard shows. [`apply()`] adds
//! the new connections and groups to an in-memory store; saving `connections.toml` stays with the
//! caller (`ConfigDir::save_connection_store`).
//!
//! Foreign files never supply secrets. A needed secret is named as a `ProfileSecretKind`; the
//! user enters it at apply time and it goes to the `SecretStore` only, or it is skipped. There
//! is no network access and no process is started. Reading is bounded by [`MAX_FILE_BYTES`],
//! [`MAX_ENTRIES`], [`MAX_FIELD_CHARS`] and [`MAX_GROUP_DEPTH`]; paths found in a file are checked
//! by [`foreign_path`] and never opened.
//!
//! `pgAdmin` `servers.json` is the first source; the `Beekeeper Studio` connection export is another. A new source is one module plus one line in
//! [`sources`].

#![forbid(unsafe_code)]

mod apply;
mod beekeeper;
mod error;
mod model;
mod path;
mod pgadmin;
mod plan;
mod read;
mod source;

pub use apply::apply;
pub use beekeeper::Beekeeper;
pub use error::ImportError;
pub use model::{
    ApplyReport, EntryOutcome, ImportOptions, ImportPlan, PlanAction, PlanItem, ProvidedSecret,
    SkipReason, SourceEntry, SourceReport,
};
pub use path::foreign_path;
pub use pgadmin::PgAdmin;
pub use plan::plan;
pub use read::read_source;
pub use source::{HostEnv, HostOs, Source, discover};

/// Largest file that is read, in bytes.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Most entries a file may hold.
pub const MAX_ENTRIES: usize = 5_000;
/// Longest accepted text value of a foreign field, in characters.
pub const MAX_FIELD_CHARS: usize = 4_096;
/// Deepest group path; deeper levels are folded into the last one.
pub const MAX_GROUP_DEPTH: usize = 8;

/// Every source, one line each.
#[must_use]
pub fn sources() -> Vec<Box<dyn Source>> {
    vec![Box::new(pgadmin::PgAdmin), Box::new(beekeeper::Beekeeper)]
}

/// The source with this id.
#[must_use]
pub fn source(id: &str) -> Option<Box<dyn Source>> {
    sources().into_iter().find(|s| s.id() == id)
}
