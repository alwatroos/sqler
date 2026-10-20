// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use sqler_core::{DbObject, Listing, ObjectKind, ObjectPath, Table};

use crate::{BoxFuture, DriverError};

/// Lazy, per-node schema introspection (ADR-0008).
///
/// The object browser loads one node at a time. Nothing here loads a whole catalog.
///
/// Rules every driver follows:
///
/// - [`Introspect::list`] returns `Ok(Listing::Unsupported)` if and only if
///   `!self.supports(kind)`. A kind the database lacks is never an error.
/// - A `None` database or schema in `parent` means the session's current or default one.
/// - For [`ObjectKind::Index`] and [`ObjectKind::Trigger`], `parent.table` narrows the list to
///   one table, and the returned objects carry `parent.table` when it is known.
/// - [`Introspect::table`] accepts `Table`, `View` and `MaterializedView` and returns `Ok(None)`
///   for other kinds or a missing object.
/// - [`Introspect::definition`] returns `Ok(None)` when the server has no text for the object.
/// - Results are ordered by name, except [`ObjectKind::Schema`], which keeps the server's order.
pub trait Introspect: Send {
    /// Whether this database has objects of `kind`.
    fn supports(&self, kind: ObjectKind) -> bool;

    /// The objects of `kind` directly under `parent`.
    ///
    /// `parent` points at the level given by [`ObjectKind::level`] for `kind`.
    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>>;

    /// A table, view or materialised view with its columns and keys, or `None`.
    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>>;

    /// The server-side definition text of `object` (view body, function source, `CREATE`
    /// statement), or `None` if the server has none.
    fn definition<'a>(
        &'a mut self,
        object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>>;
}
