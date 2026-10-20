// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the browser jobs.

use sqler_catalog::CatalogError;
use sqler_driver_api::DriverError;
use sqler_sql::DdlError;
use thiserror::Error;

use super::connect::ConnectFailure;

/// Why a browser job failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BrowserError {
    /// The short-lived session could not be opened.
    #[error(transparent)]
    Connect(#[from] ConnectFailure),
    /// The catalog failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The script could not be generated.
    #[error(transparent)]
    Ddl(#[from] DdlError),
    /// The driver failed.
    #[error(transparent)]
    Driver(DriverError),
    /// The object does not exist (any more).
    #[error("the object was not found; refresh the tree and try again")]
    NotFound,
    /// The session offers no introspection.
    #[error("the session does not support introspection")]
    NoIntrospection,
    /// The job was cancelled.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<DriverError> for BrowserError {
    fn from(err: DriverError) -> Self {
        match err {
            DriverError::Cancelled => Self::Cancelled,
            other => Self::Driver(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_stable() {
        let cases: Vec<(BrowserError, &str)> = vec![
            (
                BrowserError::Connect(ConnectFailure::Aborted),
                "connect aborted",
            ),
            (
                BrowserError::Catalog(CatalogError::Closed),
                "catalog worker is closed",
            ),
            (
                BrowserError::Ddl(DdlError::InvalidIdentifier {
                    name: String::new(),
                }),
                "invalid identifier \"\": must be non-empty and free of NUL",
            ),
            (
                BrowserError::Driver(DriverError::Query("bad".into())),
                "query failed: bad",
            ),
            (
                BrowserError::NotFound,
                "the object was not found; refresh the tree and try again",
            ),
            (
                BrowserError::NoIntrospection,
                "the session does not support introspection",
            ),
            (BrowserError::Cancelled, "operation cancelled"),
        ];
        for (err, text) in cases {
            assert_eq!(err.to_string(), text);
        }
    }

    #[test]
    fn driver_cancel_maps_to_cancelled() {
        assert!(matches!(
            BrowserError::from(DriverError::Cancelled),
            BrowserError::Cancelled
        ));
        assert!(matches!(
            BrowserError::from(DriverError::Query("x".into())),
            BrowserError::Driver(_)
        ));
    }
}
