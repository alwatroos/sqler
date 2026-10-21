// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the tool set.

use sqler_catalog::CatalogError;
use sqler_driver_api::DriverError;
use thiserror::Error;

/// Why a tool call failed.
///
/// A [`ToolSet`](crate::ToolSet) turns every error into an error result with
/// [`ToolError::code`] and the `Display` text; nothing here is ever shown as a panic.
/// Not `Clone`, because [`DriverError`] is not.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolError {
    /// No tool has this name.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    /// The arguments are not valid JSON, miss a field, carry an unknown field or a bad value.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// The host lends no connection with this id or name.
    #[error("unknown connection: {0}")]
    UnknownConnection(String),
    /// The tool or the connection's catalog is not available in this host.
    #[error("not available: {0}")]
    NotAvailable(String),
    /// The driver or the connection does not support the request.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The named object does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// A statement needs approval and the host gave no approver. Nothing was run.
    #[error("the statement needs approval and no approver is available; nothing was run")]
    NoApprover,
    /// A statement was not approved. Nothing was run. `index` counts from zero.
    #[error("statement {index} was not approved; nothing was run")]
    Denied {
        /// The zero-based index of the statement that was refused.
        index: usize,
    },
    /// The connection is read-only and the statement is not a plain read. Nothing was run.
    #[error("the connection is read-only; nothing was run")]
    ReadOnly,
    /// The catalog failed.
    #[error("catalog error: {0}")]
    Catalog(CatalogError),
    /// The driver failed.
    #[error("{0}")]
    Driver(DriverError),
    /// The host failed (for example it could not open a tab).
    #[error("host error: {0}")]
    Host(String),
}

impl ToolError {
    /// The stable `snake_case` code of the error, as put in the error result.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownTool(_) => "unknown_tool",
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::UnknownConnection(_) => "unknown_connection",
            Self::NotAvailable(_) => "not_available",
            Self::Unsupported(_) => "unsupported",
            Self::NotFound(_) => "not_found",
            Self::NoApprover => "no_approver",
            Self::Denied { .. } => "denied",
            Self::ReadOnly => "read_only",
            Self::Catalog(_) => "catalog",
            Self::Driver(_) => "driver",
            Self::Host(_) => "host",
        }
    }
}

impl From<CatalogError> for ToolError {
    fn from(err: CatalogError) -> Self {
        Self::Catalog(err)
    }
}

impl From<DriverError> for ToolError {
    fn from(err: DriverError) -> Self {
        Self::Driver(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_messages() {
        let cases: Vec<(ToolError, &str)> = vec![
            (ToolError::UnknownTool("x".into()), "unknown_tool"),
            (ToolError::InvalidArguments("x".into()), "invalid_arguments"),
            (
                ToolError::UnknownConnection("x".into()),
                "unknown_connection",
            ),
            (ToolError::NotAvailable("x".into()), "not_available"),
            (ToolError::Unsupported("x".into()), "unsupported"),
            (ToolError::NotFound("x".into()), "not_found"),
            (ToolError::NoApprover, "no_approver"),
            (ToolError::Denied { index: 1 }, "denied"),
            (ToolError::ReadOnly, "read_only"),
            (ToolError::Catalog(CatalogError::Closed), "catalog"),
            (ToolError::Driver(DriverError::Cancelled), "driver"),
            (ToolError::Host("x".into()), "host"),
        ];
        for (err, code) in cases {
            assert_eq!(err.code(), code);
            assert_ne!(err.to_string().len(), 0);
        }
        assert_eq!(
            ToolError::Denied { index: 2 }.to_string(),
            "statement 2 was not approved; nothing was run"
        );
        assert_eq!(
            ToolError::from(DriverError::Cancelled).to_string(),
            "operation cancelled"
        );
    }
}
