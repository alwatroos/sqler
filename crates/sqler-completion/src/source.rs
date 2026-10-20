// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The schema view the engine reads, defined over `sqler-core` types.

use sqler_core::{ColumnInfo, ObjectKind};

/// Data that may not have been fetched yet.
///
/// Exactly two states, so this enum is not `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loadable<T> {
    /// The data is available (possibly empty).
    Loaded(T),
    /// The data has not been loaded. The engine reports a [`SchemaRequest`](crate::SchemaRequest)
    /// instead of failing.
    NotLoaded,
}

/// A relation (table, view, materialized view) that a schema lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Relation<'a> {
    /// The schema that holds it, if the source knows it.
    pub schema: Option<&'a str>,
    /// The relation name, exactly as stored.
    pub name: &'a str,
    /// The kind; the engine uses `Table`, `View` and `MaterializedView` and ignores the rest.
    pub kind: ObjectKind,
}

impl<'a> Relation<'a> {
    /// Builds a relation.
    #[must_use]
    pub fn new(schema: Option<&'a str>, name: &'a str, kind: ObjectKind) -> Self {
        Self { schema, name, kind }
    }
}

/// A read-only view of the schema, as far as it is loaded.
///
/// All names are exact stored names; resolving what the user typed (case, quoting) is the
/// engine's job. Implement it over any catalog; [`MemorySchema`](crate::MemorySchema) is the
/// in-memory implementation for tests.
pub trait SchemaSource {
    /// Every schema name.
    fn schemas(&self) -> Box<dyn Iterator<Item = &str> + '_>;

    /// The schemas that unqualified names resolve in, in order.
    fn search_path(&self) -> &[String];

    /// The relations of `schema`, or of the search path when `None`.
    fn relations(
        &self,
        schema: Option<&str>,
    ) -> Loadable<Box<dyn Iterator<Item = Relation<'_>> + '_>>;

    /// The columns of a relation, by exact stored names; `None` schema means the search path.
    fn columns(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ColumnInfo]>;
}
