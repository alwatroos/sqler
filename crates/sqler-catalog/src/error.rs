// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the catalog.

use sqler_driver_api::DriverError;
use thiserror::Error;

/// Why a catalog request failed.
///
/// Cloneable and comparable so it can be cached per node and carried in events.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// The driver failed; the text is the driver error's message.
    #[error("introspection failed: {0}")]
    Driver(String),
    /// The operation was cancelled.
    #[error("operation cancelled")]
    Cancelled,
    /// The catalog worker has stopped.
    #[error("catalog worker is closed")]
    Closed,
    /// The source has no introspection.
    #[error("the session does not support introspection")]
    NoIntrospection,
}

impl From<DriverError> for CatalogError {
    fn from(err: DriverError) -> Self {
        match err {
            DriverError::Cancelled => Self::Cancelled,
            other => Self::Driver(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages() {
        assert_eq!(
            CatalogError::Driver("x".into()).to_string(),
            "introspection failed: x"
        );
        assert_eq!(CatalogError::Cancelled.to_string(), "operation cancelled");
        assert_eq!(CatalogError::Closed.to_string(), "catalog worker is closed");
        assert_eq!(
            CatalogError::NoIntrospection.to_string(),
            "the session does not support introspection"
        );
    }

    #[test]
    fn driver_errors_map() {
        assert_eq!(
            CatalogError::from(DriverError::Cancelled),
            CatalogError::Cancelled
        );
        assert_eq!(
            CatalogError::from(DriverError::Query("boom".into())),
            CatalogError::Driver("query failed: boom".into())
        );
    }
}
