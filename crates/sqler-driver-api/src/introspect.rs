// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use sqler_core::Table;

use crate::{BoxFuture, DriverError};

/// Schema introspection. Shape only in M0; no driver implements it yet.
pub trait Introspect: Send {
    /// Names of all schemas.
    fn schemas(&mut self) -> BoxFuture<'_, Result<Vec<String>, DriverError>>;

    /// Tables of a schema. Their columns may be empty.
    fn tables<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<Vec<Table>, DriverError>>;

    /// One table with its columns filled, or `None` if it does not exist.
    fn table<'a>(
        &'a mut self,
        schema: &'a str,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>>;
}
