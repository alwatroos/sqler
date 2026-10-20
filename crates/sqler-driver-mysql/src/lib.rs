// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `MySQL` and `MariaDB` driver implementation.
//!
//! [`MySqlDriver`] implements [`sqler_driver_api::Driver`] on `sqlx`'s `MySQL` backend, one
//! connection per session (no pool). The one driver id `mysql` serves both servers.
//!
//! The `ConnectionConfig::target` is a `mysql://` or `mariadb://` URL, parsed by `sqlx`
//! (`mysql://user:password@host:3306/database?ssl-mode=required`).
//!
//! Every statement is prepared, so results always use the binary protocol and stream in batches
//! of 256 rows. A statement the server refuses to prepare (error 1295, for example
//! `CREATE PROCEDURE`) and that has no parameters runs over the text protocol instead and
//! reports its affected rows. `DATE`, `DATETIME`, `TIMESTAMP` and `TIME` cells are decoded from
//! the documented binary layout into typed values; zero dates and `TIME` values that are
//! negative or 24 hours or longer come back as `Text`.
//!
//! The session also implements [`sqler_driver_api::Introspect`] from `information_schema` and
//! `SHOW CREATE`. There is no database level: the `MySQL` databases are the schemas.
//!
//! # Runtime
//! `sqlx` runs on Tokio (`runtime-tokio`): the futures returned by the driver must be polled
//! inside a Tokio runtime context.
//!
//! # Cancellation
//! Cancelling sets a flag, then runs `KILL QUERY <connection id>` on a short-lived side
//! connection opened with the same options. A stream that sees the flag, or error 1317, ends
//! with [`sqler_driver_api::DriverError::Cancelled`]. A cancel that reaches the server before the
//! statement does has no effect there, but the flag still ends the stream at its first poll.
//!
//! # TLS
//! TLS uses `rustls`. When `ConnectionConfig::tls` is `None`, the URL parameters (`ssl-mode`,
//! `ssl-ca`, `ssl-cert`, `ssl-key`) apply and the default is `PREFERRED`. When it is `Some`, it
//! overrides them: `Disable` is `DISABLED`, `Prefer` `PREFERRED`, `Require` `REQUIRED`, `VerifyCa`
//! `VERIFY_CA` and `VerifyFull` `VERIFY_IDENTITY`.
//!
//! The RSA password exchange (`caching_sha2_password` on a cold server cache, and
//! `sha256_password`) over an unencrypted connection is not compiled in: use TLS, or log in once
//! over TLS so the server caches the account.
//!
//! # Secrets
//! The connect options hold the password. The driver never prints them and never puts the target
//! in an error.
//!
//! # Binary collations and `MariaDB`
//! The protocol flags a column with a `_bin` collation as binary, so such text arrives as
//! `Binary`/`Bytes`. `MariaDB`'s `JSON` is an alias of `LONGTEXT` with a binary collation, so
//! its cells are `Bytes` too (`MySQL`'s native `JSON` is `Json`). `MariaDB` also quotes string
//! literals in `COLUMN_DEFAULT` and reports a `NULL` default as the text `NULL`.

#![forbid(unsafe_code)]

mod convert;
mod introspect;
mod session;
mod stream;

use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{BoxFuture, Driver, DriverError, Session};

use crate::session::MySqlSession;

/// The id this driver registers under.
pub const DRIVER_ID: &str = "mysql";

/// The `MySQL` and `MariaDB` driver.
#[derive(Debug, Default, Clone, Copy)]
pub struct MySqlDriver;

impl MySqlDriver {
    /// Creates the driver.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for MySqlDriver {
    fn id(&self) -> &'static str {
        DRIVER_ID
    }

    fn connect<'a>(
        &'a self,
        config: &'a ConnectionConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DriverError>> {
        Box::pin(async move {
            let session = MySqlSession::connect(config).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}
