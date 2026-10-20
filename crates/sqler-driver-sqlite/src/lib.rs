// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SQLite` driver implementation.
//!
//! [`SqliteDriver`] implements [`sqler_driver_api::Driver`] on `sqlx`'s bundled `SQLite` backend,
//! one connection per session (no pool; `:memory:` stays private to its session).
//!
//! The `ConnectionConfig::target` is `:memory:`, a `sqlite:` URL, or a file path. A missing file
//! is created.
//!
//! The session also implements [`sqler_driver_api::Introspect`] from `sqlite_master` and the
//! `pragma_*` functions, and decodes TEXT cells of DATE, TIME and DATETIME columns into typed
//! values (falling back to `Text`).
//!
//! # Runtime
//! `sqlx` runs on Tokio (`runtime-tokio`): the futures returned by the driver must be polled inside
//! a Tokio runtime context.
//!
//! # Cancellation
//! Cancellation is cooperative: the flag is checked before each batch of rows, so a single long
//! step before the first row (for example a large sort) cannot be interrupted.

#![forbid(unsafe_code)]

mod convert;
mod introspect;
mod session;
mod stream;

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{BoxFuture, Driver, DriverError, Session};

use crate::session::SqliteSession;

/// The id this driver registers under.
pub const DRIVER_ID: &str = "sqlite";

/// The `SQLite` driver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteDriver;

impl SqliteDriver {
    /// Creates the driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for SqliteDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            let session = SqliteSession::connect(config).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}
