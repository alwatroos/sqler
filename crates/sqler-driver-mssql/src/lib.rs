// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SQL Server` driver implementation built on `tiberius`.
//!
//! [`MssqlDriver`] implements [`sqler_driver_api::Driver`], one connection per session (no pool).
//! It registers under the engine id [`ENGINE_ID`]; the `sqlserver` plugin (ADR-0054, ADR-0061)
//! reaches it as a host engine.
//!
//! The `ConnectionConfig::target` is `sqlserver://[user[:password]@]host[:port][/database]`
//! (`mssql://` is accepted as an alias). The port defaults to 1433 and the database to the
//! login's default. User, password and database are percent-decoded. The URL query must be empty:
//! any key is refused with "unsupported option `<key>`", naming the key and never the value. Only
//! SQL logins are supported; Windows authentication and Azure AD are not.
//!
//! Results come as `Columns`, `Rows` in batches of 256, then `Done`. Only the first result set of
//! a batch is returned; later ones are read and dropped. Dates and decimals are decoded from the
//! raw wire fields into typed values; `money` arrives as a float (lossy), `uniqueidentifier`,
//! `xml` and types without a closer match as text.
//!
//! The session also implements [`sqler_driver_api::Introspect`] from the `sys.*` catalog views.
//! Only the database the session is connected to can be browsed.
//!
//! # Limits
//! `rows_affected` is always `None`: statements run through `tiberius`' `query`, whose stream does
//! not expose the `DONE` counts. A statement with parameters runs through `sp_executesql`, so a
//! `USE` or `SET` inside it does not outlive it; a statement without parameters runs as a plain
//! batch and keeps its session state.
//!
//! # Runtime
//! `tiberius` runs on Tokio here: the futures returned by the driver must be polled inside a Tokio
//! runtime with the time driver enabled. A task per session owns the connection.
//!
//! # TLS
//! TLS uses `rustls` with the `ring` provider, installed as the process default by
//! [`install_ring_provider`] (the driver calls it before it connects). `tiberius`' `rustls`
//! feature also compiles `aws-lc-rs` in (it is already in the lock for `sqlx-mysql`); it is never
//! the provider used here. `ConnectionConfig::tls` maps as follows; `None` means `Prefer`.
//!
//! | Mode | Encryption | Trust |
//! | ---- | ---------- | ----- |
//! | `Disable` | none (a server that forces encryption refuses) | n/a |
//! | `Prefer` | on | any certificate |
//! | `Require` | required | any certificate |
//! | `VerifyFull` | required | the CA file if set, else the OS roots; the host name is checked |
//! | `VerifyCa` | `Unsupported` | n/a |
//!
//! `VerifyFull` with `server_name` checks the certificate against that name instead of the dial
//! host. Client certificates are `Unsupported`.
//!
//! # Cancellation
//! Cancel closes the connection: the statement ends with `DriverError::Cancelled`, the socket is
//! dropped, and the server ends the statement and rolls back an open transaction. The next
//! `execute` on that session fails with `DriverError::Connection` ("the SQL Server connection was
//! closed by a cancel; connect again"); connect again for a new session. A cancel that arrives
//! while no statement is active does nothing.
//!
//! # Secrets
//! The password lives in the connect options only. No error text contains it, and no error
//! contains the target.

#![forbid(unsafe_code)]

mod convert;
mod introspect;
mod options;
mod session;
mod stream;

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{BoxFuture, Driver, DriverError, Session};

use crate::session::MssqlSession;

/// The engine id this driver registers under.
pub const ENGINE_ID: &str = "mssql";

/// The `SQL Server` driver.
#[derive(Debug, Default, Clone, Copy)]
pub struct MssqlDriver;

impl MssqlDriver {
    /// Creates the driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for MssqlDriver {
    fn id(&self) -> &'static str {
        ENGINE_ID
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            let session = Box::pin(MssqlSession::connect(config)).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}

/// Installs rustls' `ring` provider as the process default; a no-op when one is installed.
pub fn install_ring_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Losing a race against another installer is fine: a provider is installed either way.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_provider() {
        assert_eq!(MssqlDriver::new().id(), "mssql");
        install_ring_provider();
        install_ring_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
