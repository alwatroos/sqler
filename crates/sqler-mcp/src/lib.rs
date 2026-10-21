// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `MCP` server exposing the `sqler-ai-tools` tool set to external agents.
//!
//! [`McpServer`] wraps one `ToolSet` (surface `mcp`, audited) and serves it over stdio
//! ([`McpServer::serve_stdio`], [`McpServer::serve_io`]) or streamable HTTP
//! ([`McpServer::bind_http`]). `tools/list` is the tool set's definitions minus the UI-only tools;
//! `tools/call` goes through `ToolSet::call_detailed`, so guards, redaction, the untrusted-data
//! envelope and the audit log apply by construction. Approval is whatever the tool set carries:
//! a host without an approver refuses every statement that is not a plain read.
//!
//! The HTTP transport binds loopback only, checks `Host` and `Origin`, requires
//! `Authorization: Bearer <token>` ([`AuthToken`]) and caps the body size.

#![forbid(unsafe_code)]

mod auth;
mod error;
mod http;
mod map;
mod protocol;
mod server;
mod stdio;

pub use auth::AuthToken;
pub use error::McpError;
pub use http::HttpServer;
pub use map::UI_ONLY_TOOLS;
pub use server::{McpServer, ServerOptions};

/// The path of the streamable HTTP endpoint.
pub const MCP_PATH: &str = "/mcp";
/// The request header that carries the bearer token.
pub const AUTH_HEADER: &str = "Authorization";
/// The environment variable the CLI reads a token from.
pub const TOKEN_ENV: &str = "SQLER_MCP_TOKEN";
