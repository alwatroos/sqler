// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `DuckDB` driver implementation.
//!
//! [`DuckDbDriver`] implements [`sqler_driver_api::Driver`] on the `duckdb` crate with the
//! bundled engine. It is the native engine behind the declarative `duckdb` plugin shell
//! (ADR-0061, ADR-0070): the host registers it under [`ENGINE_ID`], and the plugin manifest
//! only names it. The engine ignores `ConnectionConfig::driver`.
//!
//! The `ConnectionConfig::target` is `:memory:` (private to its session) or a file path. A
//! missing file is created. Sessions on one file share one `DuckDB` instance, so the object
//! browser, a query tab and an export can use the same file at once.
//!
//! The session also implements [`sqler_driver_api::Introspect`] from the `duckdb_*()` catalog
//! functions (databases, schemas, tables, views, indexes, sequences).
//!
//! # Sandbox
//! The engine opens with external file access, extension install and autoload, and community
//! extensions off, then locks the configuration. `read_csv('file')`, `INSTALL`, `ATTACH` of
//! another file and `SET enable_external_access = true` fail. Error texts never contain the
//! database path (it is replaced by `<database>`).
//!
//! # Runtime
//! Each session runs one worker thread that owns the blocking connection. The futures only wait
//! on channels, so they need no particular runtime, except `close`, which joins the worker with
//! `tokio::task::spawn_blocking` and so must run inside a Tokio runtime.
//!
//! # Cancellation
//! Cancel sets a flag and interrupts the running statement through `DuckDB`'s interrupt handle,
//! so a long single step (a large aggregate, a sort) stops too. The session stays usable.

#![forbid(unsafe_code)]

mod convert;
mod database;
mod error;
mod introspect;
mod session;
mod stream;

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{BoxFuture, Driver, DriverError, Session};

use crate::session::DuckSession;

/// The id this driver registers under.
pub const DRIVER_ID: &str = "duckdb";

/// The host-engine id: the `HostEngines` key of the plugin shell, `driver.host:duckdb`.
pub const ENGINE_ID: &str = "duckdb";

/// The `DuckDB` driver.
#[derive(Debug, Default, Clone, Copy)]
pub struct DuckDbDriver;

impl DuckDbDriver {
    /// Creates the driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for DuckDbDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            let session = DuckSession::connect(config).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}
