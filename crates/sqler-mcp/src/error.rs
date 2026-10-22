// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The error type of the MCP server. No variant holds a token.

use std::net::SocketAddr;

/// Why the server could not start or stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The HTTP transport binds loopback addresses only.
    #[error("the HTTP transport binds loopback addresses only, not {0}")]
    NonLoopbackBind(SocketAddr),
    /// The listening socket could not be bound or used.
    #[error("cannot bind the HTTP listener: {0}")]
    Bind(std::io::Error),
    /// The token text is not acceptable.
    #[error("invalid MCP token: {0}")]
    InvalidToken(&'static str),
    /// The system random number generator failed.
    #[error("the system random number generator failed")]
    Random,
    /// A stdio line was longer than the request cap.
    #[error("a request line exceeded the size cap")]
    RequestTooLarge,
    /// The protocol layer failed.
    #[error("MCP transport failure: {0}")]
    Transport(String),
}
