// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Server-side cancel through a short side connection.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqler_driver_api::{BoxFuture, Canceller, DriverError};
use sqlx::Connection as _;
use sqlx::postgres::{PgConnectOptions, PgConnection};

/// How long a cancel request may take, connect included.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);

/// Cancels the statement running on one backend with `pg_cancel_backend`.
///
/// `sqlx` has no cancel API, so the request goes over a second connection opened with the
/// session's own options (and so its own credentials and TLS settings).
pub(crate) struct PgCanceller {
    opts: PgConnectOptions,
    pid: i32,
    requested: AtomicBool,
    active: AtomicBool,
}

// `PgConnectOptions` prints the password in `Debug`, so this is written by hand.
impl fmt::Debug for PgCanceller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PgCanceller {{ pid: {} }}", self.pid)
    }
}

impl PgCanceller {
    pub(crate) fn new(opts: PgConnectOptions, pid: i32) -> Self {
        Self {
            opts,
            pid,
            requested: AtomicBool::new(false),
            active: AtomicBool::new(false),
        }
    }

    /// Whether a cancel was requested since the last `begin`.
    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Starts a statement: forgets an earlier request and allows the server round trip.
    pub(crate) fn begin(&self) {
        self.requested.store(false, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
    }

    /// Ends the statement; a later cancel only sets the flag.
    pub(crate) fn finish(&self) {
        self.active.store(false, Ordering::SeqCst);
    }

    async fn send(&self) -> Result<(), DriverError> {
        let mut conn = PgConnection::connect_with(&self.opts)
            .await
            .map_err(|e| DriverError::Connection(e.to_string()))?;
        let sent = sqlx::query("select pg_cancel_backend($1)")
            .bind(self.pid)
            .execute(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| DriverError::Connection(format!("cancel request failed: {e}")));
        // The side connection is closed on both paths; its own close error is not interesting.
        let _ = conn.close().await;
        sent
    }
}

impl Canceller for PgCanceller {
    fn cancel(&self) -> BoxFuture<'_, Result<(), DriverError>> {
        self.requested.store(true, Ordering::SeqCst);
        Box::pin(async move {
            if !self.active.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::timeout(CANCEL_TIMEOUT, self.send())
                .await
                .unwrap_or_else(|_| {
                    Err(DriverError::Connection(
                        "cancel request timed out".to_owned(),
                    ))
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;

    fn canceller() -> PgCanceller {
        let opts = PgConnectOptions::from_str("postgres://u:s3cret@127.0.0.1:1/x").unwrap();
        PgCanceller::new(opts, 42)
    }

    #[test]
    fn debug_hides_the_password() {
        let text = format!("{:?}", canceller());
        assert_eq!(text, "PgCanceller { pid: 42 }");
    }

    #[test]
    fn flags_follow_the_statement() {
        let c = canceller();
        assert!(!c.is_requested());
        c.begin();
        c.requested.store(true, Ordering::SeqCst);
        assert!(c.is_requested());
        c.begin();
        assert!(!c.is_requested());
        assert!(c.active.load(Ordering::SeqCst));
        c.finish();
        assert!(!c.active.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_while_idle_only_sets_the_flag() {
        let c = canceller();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Not active: no connection is attempted (port 1 would fail), so this is Ok.
        rt.block_on(c.cancel()).unwrap();
        assert!(c.is_requested());
    }

    #[test]
    fn cancel_error_has_no_secret() {
        let c = canceller();
        c.begin();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(c.cancel()).unwrap_err();
        assert!(matches!(err, DriverError::Connection(_)));
        assert!(!err.to_string().contains("s3cret"));
    }
}
