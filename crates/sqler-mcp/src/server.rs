// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `McpServer`: one tool set behind the MCP protocol, independent of the transport.

use std::sync::Arc;

use serde_json::Value;
use sqler_ai_api::ToolDefinition;
use sqler_ai_tools::{AuditSink, Surface, ToolSet};
use tokio::sync::Semaphore;

use crate::map;

/// Tunables of the server.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ServerOptions {
    /// The server name sent in `serverInfo`.
    pub name: String,
    /// How many tool calls run at once; more wait for a permit.
    pub max_concurrent_calls: usize,
    /// The largest accepted request: a stdio line or an HTTP body, in bytes.
    pub max_request_bytes: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            name: "sqler".to_owned(),
            max_concurrent_calls: 4,
            max_request_bytes: 1024 * 1024,
        }
    }
}

/// What every protocol session of one server shares.
pub(crate) struct Shared {
    pub tools: ToolSet,
    pub definitions: Vec<ToolDefinition>,
    /// The `tools/list` entries, parallel to `definitions`.
    pub wire: Vec<Value>,
    pub permits: Semaphore,
    pub options: ServerOptions,
}

/// A tool set served over MCP. Cheap to clone; clones share the call limit.
#[derive(Clone)]
pub struct McpServer {
    pub(crate) shared: Arc<Shared>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServer")
            .field("tools", &self.shared.definitions.len())
            .field("options", &self.shared.options)
            .finish()
    }
}

impl McpServer {
    /// Serves `tools` with every call audited to `audit` as surface `mcp`.
    ///
    /// Approval is whatever the tool set carries: a host that attached no approver gets
    /// `no_approver` for every statement that is not a plain read. A definition whose schema is
    /// not an object schema is dropped (the built-in tools all pass; a test pins that).
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(tools: ToolSet, audit: Arc<dyn AuditSink>, options: ServerOptions) -> Self {
        let tools = tools.with_audit(audit, Surface::Mcp);
        let (definitions, wire): (Vec<_>, Vec<_>) = map::exposed(tools.definitions())
            .into_iter()
            .filter_map(|d| map::to_tool(&d).map(|tool| (d, tool)))
            .unzip();
        let permits = Semaphore::new(options.max_concurrent_calls.max(1));
        Self {
            shared: Arc::new(Shared {
                tools,
                definitions,
                wire,
                permits,
                options,
            }),
        }
    }

    /// The tools this server lists, in order.
    #[must_use]
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.shared.definitions
    }
}
