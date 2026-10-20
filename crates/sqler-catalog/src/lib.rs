// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Schema cache with incremental refresh, search index and dependency graph.
//!
//! [`Catalog::new`] takes a session (or any [`sqler_driver_api::Introspect`]) and returns a
//! cloneable [`Catalog`] plus a [`CatalogWorker`]. The worker owns the session and runs every
//! introspection call in sequence; the crate spawns nothing, the caller runs
//! [`CatalogWorker::run`] on an executor.
//!
//! - [`Catalog::cached`] and [`Catalog::cached_table`] read the cache synchronously and never
//!   touch the database.
//! - [`Catalog::children`] and [`Catalog::table`] load a missing node once, however many callers ask.
//! - [`Catalog::refresh`] re-fetches cached nodes in the background, reports [`RefreshEvent`]s,
//!   can be cancelled, and replaces the cache in one step at the end.
//! - Errors are kept per node ([`Catalog::node_error`]) and never evict data.
//! - [`Catalog::snapshot`] returns an immutable [`CatalogSnapshot`] in one lock and an `Arc`
//!   clone. It answers name lookups ([`NameQuery`], prefix or fuzzy, scoped, bounded and ranked),
//!   reports whether a table's columns are loaded ([`TableState`]) and resolves foreign-key
//!   targets ([`ForeignKeyTarget`]). A refresh publishes a new snapshot; listings and tables that
//!   came back equal are shared with the old one.
//! - [`Catalog::load_tables`] loads the columns of named tables off the calling thread, one
//!   introspection call per table however many ask.

#![forbid(unsafe_code)]

mod catalog;
mod error;
mod index;
mod key;
mod shard;
mod snapshot;
mod store;
mod worker;

pub use catalog::{Catalog, RefreshEvent, RefreshHandle, RefreshOutcome};
pub use error::CatalogError;
pub use index::{DEFAULT_LIMIT, MatchMode, MatchQuality, NameHit, NameKind, NameQuery};
pub use key::{CacheKey, NodeKey, RefreshScope};
pub use snapshot::{CatalogSnapshot, ForeignKeyTarget, TableState};
pub use worker::{CatalogWorker, IntrospectSource};
