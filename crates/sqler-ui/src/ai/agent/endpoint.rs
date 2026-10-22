// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tool endpoint of one agent tab (ADR-0051): the tab's own tool set served over the loopback
//! MCP transport of `sqler-mcp`, behind a bearer token that exists only in [`McpToken`] and the
//! server's `AuthToken`.
//!
//! Every call is audited as surface `agent`: `McpServer::new` forces `mcp`, so the sink of
//! [`AiAudit::mcp_sink`] rewrites the surface of each entry.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use sqler_acp::McpToken;
use sqler_ai_tools::{Surface, ToolSet};
use sqler_mcp::{AuthToken, McpError, McpServer, ServerOptions};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::ai::AiAudit;

/// How long [`Endpoint::stop`] waits for the server to end before it is aborted.
pub const ENDPOINT_STOP_LIMIT: Duration = Duration::from_millis(250);

/// A running endpoint. Stop it with [`Endpoint::stop`]; dropping it aborts the server.
pub struct Endpoint {
    url: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), McpError>>>,
}

// The URL holds the port and no token.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Endpoint {
    /// `http://127.0.0.1:<port>/mcp`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stops the server and waits at most [`ENDPOINT_STOP_LIMIT`] for it; past that the server
    /// task is aborted, which closes the listener and every connection. Afterwards the port
    /// refuses connections.
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(ENDPOINT_STOP_LIMIT, &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Serves `tools` on a free loopback port behind a fresh token. Runs the server on tokio, so
/// call it from a tokio task. The audit entries go to `audit` as surface `agent`.
///
/// # Errors
/// A text for the user when the token cannot be made or the port cannot be bound.
pub async fn bind_endpoint(
    tools: ToolSet,
    audit: &AiAudit,
) -> Result<(Endpoint, McpToken), String> {
    let server = McpServer::new(
        tools,
        audit.mcp_sink(Surface::Agent),
        ServerOptions::default(),
    );
    let auth = AuthToken::generate().map_err(|err| failed(&err))?;
    let token = McpToken::new(auth.expose().to_owned());
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let http = server
        .bind_http(addr, auth)
        .await
        .map_err(|err| failed(&err))?;
    let url = http.url();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(http.run(async move {
        let _ = stopped.await;
    }));
    let endpoint = Endpoint {
        url,
        stop: Some(stop),
        task: Some(task),
    };
    Ok((endpoint, token))
}

fn failed(err: &McpError) -> String {
    format!("SQLer's tool endpoint could not start: {err}.")
}
