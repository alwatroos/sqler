// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The session source: how a host lends connections, catalogs and sessions to the tool set.

use sqler_ai_api::BoxFuture;
use sqler_catalog::Catalog;
use sqler_driver_api::Session;
use sqler_driver_api::sqler_core::{DriverId, Environment};

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
        Self {
            id: id.into(),
            name: name.into(),
            driver,
            environment,
            read_only: false,
        }
    }

    /// Sets the read-only flag.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
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
