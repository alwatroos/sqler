// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The session source: how a host lends connections, catalogs and sessions to the tool set.

use sqler_ai_api::BoxFuture;
use sqler_catalog::Catalog;
use sqler_driver_api::Session;
use sqler_driver_api::sqler_core::{DriverId, Environment};
use sqler_sql::Dialect;

use crate::error::ToolError;

/// What the model may know about a lent connection. Never carries a target or a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionInfo {
    /// The id tools use to name the connection.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The driver id.
    pub driver: DriverId,
    /// The environment tag, if the user set one. `prod` triggers the second confirmation.
    pub environment: Option<Environment>,
    /// Whether the connection refuses everything but plain reads.
    pub read_only: bool,
    /// Whether row values may reach the model. `false` withholds every `rows` array (ADR-0037).
    pub send_rows: bool,
    /// The SQL dialect the guard, the samples and the prompts use. [`ConnectionInfo::new`] sets
    /// [`Dialect::from_driver_id`] of the driver; a host with a plugin driver sets the dialect its
    /// manifest declares. `None` means no dialect is known: nothing is proven read-only.
    pub dialect: Option<Dialect>,
}

impl ConnectionInfo {
    /// Creates a connection description; `read_only` is `false`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        driver: DriverId,
        environment: Option<Environment>,
    ) -> Self {
        let dialect = Dialect::from_driver_id(driver.as_str());
        Self {
            id: id.into(),
            name: name.into(),
            driver,
            environment,
            read_only: false,
            send_rows: environment != Some(Environment::Prod),
            dialect,
        }
    }

    /// Sets the SQL dialect (`None` for a driver with no known dialect).
    #[must_use]
    pub fn with_dialect(mut self, dialect: Option<Dialect>) -> Self {
        self.dialect = dialect;
        self
    }

    /// Sets the read-only flag.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Sets whether row values may be sent to the model. Hosts pass
    /// `ConnectionProfile::ai_sends_rows()`; the default is `true` unless the environment is
    /// `prod`.
    #[must_use]
    pub fn with_send_rows(mut self, yes: bool) -> Self {
        self.send_rows = yes;
        self
    }
}

/// How a host lends database access to the tool set.
///
/// The host decides which connections the model may see (the AI opt-in filter is the host's
/// job). The tool set reads catalogs through [`SessionSource::catalog`] and asks for a session
/// only to run SQL or to read a function's definition; it always hands the session back through
/// [`SessionSource::release`].
pub trait SessionSource: Send + Sync {
    /// The connections the model may use.
    fn connections(&self) -> Vec<ConnectionInfo>;

    /// The catalog of a connection, if the host has one.
    fn catalog(&self, connection: &str) -> Option<Catalog>;

    /// Lends a session for one tool call.
    ///
    /// # Errors
    /// [`ToolError::NotAvailable`] or [`ToolError::Driver`] if no session can be had.
    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>>;

    /// Takes a lent session back. The default closes it and ignores the error.
    fn release(&self, connection: &str, session: Box<dyn Session>) -> BoxFuture<'static, ()> {
        let _ = connection;
        Box::pin(async move {
            let _ = session.close().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(env: Option<Environment>) -> ConnectionInfo {
        ConnectionInfo::new("a", "A", DriverId::new("sqlite").unwrap(), env)
    }

    #[test]
    fn send_rows_defaults_off_only_for_prod() {
        assert!(info(None).send_rows);
        assert!(info(Some(Environment::Dev)).send_rows);
        assert!(!info(Some(Environment::Prod)).send_rows);
        assert!(info(Some(Environment::Prod)).with_send_rows(true).send_rows);
        assert!(!info(None).with_send_rows(false).send_rows);
    }

    #[test]
    fn dialect_defaults_to_the_built_in_driver() {
        let of = |id: &str| ConnectionInfo::new("a", "A", DriverId::new(id).unwrap(), None);
        assert_eq!(of("postgres").dialect, Some(Dialect::Postgres));
        assert_eq!(of("mysql").dialect, Some(Dialect::MySql));
        assert_eq!(of("sqlite").dialect, Some(Dialect::Sqlite));
        // A plugin driver has no dialect until the host sets one; `tsql` is no driver id.
        assert_eq!(of("sqlserver").dialect, None);
        assert_eq!(of("tsql").dialect, None);
        assert_eq!(of("duckdb").dialect, None);
        let set = of("sqlserver").with_dialect(Some(Dialect::TSql));
        assert_eq!(set.dialect, Some(Dialect::TSql));
        assert_eq!(set.with_dialect(None).dialect, None);
    }
}
