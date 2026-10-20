// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PostgreSQL` driver implementation built on `sqlx`.
//!
//! [`PostgresDriver`] implements [`sqler_driver_api::Driver`], one connection per session (no
//! pool).
//!
//! The `ConnectionConfig::target` is a `postgres://` or `postgresql://` URL. With `tls: None` the
//! URL's own `sslmode`, `sslrootcert`, `sslcert` and `sslkey` apply and the default is `prefer`;
//! with `tls: Some` those four keys are dropped from the URL and the explicit settings win. TLS
//! uses `rustls` with the `ring` provider and the OS trust roots.
//!
//! `TlsConfig::server_name` names the host the certificate is checked against (and sent as SNI)
//! when it differs from the dial host, as through an SSH tunnel. It only applies with
//! `verify-full` on a TCP target. `sqlx` always dials the host it verifies, so on Unix the driver
//! starts a private local bridge (a Unix socket in a `0700` directory, removed when the session
//! and its cancel handles are gone) that forwards to the dial address; verification is never
//! weakened. On other platforms such a connect fails with `DriverError::Unsupported`.
//!
//! The session also implements [`sqler_driver_api::Introspect`] from `pg_catalog` for all 13
//! object kinds. Only the database the session is connected to can be browsed.
//!
//! Cells are decoded from the binary wire format into the core `Value`s; types without a closer
//! match (uuid, arrays, interval, ...) come back as text.
//!
//! # Runtime
//! `sqlx` runs on Tokio: the futures returned by the driver must be polled inside a Tokio runtime
//! with the time driver enabled (the cancel request has a timeout).
//!
//! # Cancellation
//! Cancellation is server-side. `sqlx` has no cancel API, so [`sqler_driver_api::CancelHandle`]
//! opens a short side connection with the session's own options and runs
//! `select pg_cancel_backend(<pid>)`. The stream then ends with `DriverError::Cancelled`. A cancel
//! sent just as a statement ends can hit the next statement on the same session; the window is
//! kept small by only sending while a statement is active.

#![forbid(unsafe_code)]

mod cancel;
mod convert;
mod introspect;
mod options;
mod session;
mod stream;
#[cfg(unix)]
mod tls_bridge;

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{BoxFuture, Driver, DriverError, Session};

use crate::session::PgSession;

/// The id this driver registers under.
pub const DRIVER_ID: &str = "postgres";

/// The `PostgreSQL` driver.
#[derive(Debug, Default, Clone, Copy)]
pub struct PostgresDriver;

impl PostgresDriver {
    /// Creates the driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for PostgresDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            let session = PgSession::connect(config).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}
