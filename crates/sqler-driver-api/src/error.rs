// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use sqler_core::CoreError;
use thiserror::Error;

/// Errors a driver or the registry can return.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DriverError {
    /// Connecting to the database failed.
    #[error("connection failed: {0}")]
    Connection(String),
    /// A query failed.
    #[error("query failed: {0}")]
    Query(String),
    /// The operation was cancelled.
    #[error("operation cancelled")]
    Cancelled,
    /// The driver does not support the request.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A driver broke the stream protocol.
    #[error("driver protocol violation: {0}")]
    Protocol(String),
    /// No driver is registered under this id.
    #[error("unknown driver: {0}")]
    UnknownDriver(String),
    /// A driver with this id is already registered.
    #[error("driver already registered: {0}")]
    DuplicateDriver(String),
    /// The driver exists but cannot be used right now. The text is shown to the user as is.
    #[error("driver unavailable: {0}")]
    Unavailable(String),
    /// An error from the database library, passed through.
    #[error("database error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A domain-type error.
    #[error(transparent)]
    Core(#[from] CoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn messages() {
        assert_eq!(
            DriverError::Connection("x".into()).to_string(),
            "connection failed: x"
        );
        assert_eq!(
            DriverError::Query("x".into()).to_string(),
            "query failed: x"
        );
        assert_eq!(DriverError::Cancelled.to_string(), "operation cancelled");
        assert_eq!(
            DriverError::Unsupported("x".into()).to_string(),
            "unsupported: x"
        );
        assert_eq!(
            DriverError::Protocol("x".into()).to_string(),
            "driver protocol violation: x"
        );
        assert_eq!(
            DriverError::UnknownDriver("x".into()).to_string(),
            "unknown driver: x"
        );
        assert_eq!(
            DriverError::DuplicateDriver("x".into()).to_string(),
            "driver already registered: x"
        );
        assert_eq!(
            DriverError::Unavailable("x".into()).to_string(),
            "driver unavailable: x"
        );
    }

    #[test]
    fn backend_has_source() {
        let e = DriverError::Backend("boom".into());
        assert_eq!(e.to_string(), "database error: boom");
        assert!(e.source().is_some());
    }

    #[test]
    fn core_is_transparent() {
        let e: DriverError = CoreError::InvalidDriverId("X".into()).into();
        assert_eq!(e.to_string(), "invalid driver id: X");
    }
}
