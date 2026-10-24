// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `sqler-cli mcp`: serves the command-line databases to an AI agent over MCP.
//!
//! Connections are named `db1..dbN`. A target never appears in a name, an error or a log line.
//! No approver is attached, so everything but a plain read is refused with `no_approver`.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use sqler_ai_api::BoxFuture;
use sqler_ai_tools::{AuditLog, ConnectionInfo, SessionSource, ToolError, ToolSet};
use sqler_catalog::Catalog;
use sqler_config::ConfigDir;
use sqler_driver_api::sqler_core::ConnectionConfig;
use sqler_driver_api::{DriverRegistry, Session};
use sqler_mcp::{AuthToken, McpServer, ServerOptions, TOKEN_ENV};

use crate::args::{McpArgs, McpTransport};
use crate::{connection_config_named, driver_id, driver_message, registry};

/// One database lent to the tool set.
struct Entry {
    info: ConnectionInfo,
    config: ConnectionConfig,
    catalog: Catalog,
}

/// The databases of the command line. One introspection session per target lives in its catalog;
/// tool calls that run SQL connect afresh.
struct CliSource {
    registry: DriverRegistry,
    entries: Vec<Entry>,
}

impl SessionSource for CliSource {
    fn connections(&self) -> Vec<ConnectionInfo> {
        self.entries.iter().map(|e| e.info.clone()).collect()
    }

    fn catalog(&self, connection: &str) -> Option<Catalog> {
        self.entries
            .iter()
            .find(|e| e.info.id == connection)
            .map(|e| e.catalog.clone())
    }

    fn acquire<'a>(
        &'a self,
        connection: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, ToolError>> {
        Box::pin(async move {
            let entry = self
                .entries
                .iter()
                .find(|e| e.info.id == connection)
                .ok_or_else(|| ToolError::UnknownConnection(connection.to_owned()))?;
            self.registry
                .connect(&entry.config)
                .await
                .map_err(|e| ToolError::NotAvailable(driver_message(&e).into_owned()))
        })
    }
}

/// Runs the command; the error text is safe to print.
pub(crate) fn run(args: &McpArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    let result = runtime.block_on(serve(args));
    // A blocking stdin read cannot be cancelled; do not let it hold the process after Ctrl+C.
    runtime.shutdown_timeout(Duration::from_millis(500));
    result
}

/// A usage problem that only the environment reveals (the arguments alone parse fine).
pub(crate) fn usage_problem(args: &McpArgs) -> Option<String> {
    let env_set = std::env::var(TOKEN_ENV).is_ok_and(|v| !v.is_empty());
    (env_set && args.token_file.is_some())
        .then(|| format!("give {TOKEN_ENV} or --token-file, not both"))
}

async fn connect_all(targets: &[(String, String)]) -> Result<CliSource> {
    let registry = registry()?;
    let mut entries = Vec::with_capacity(targets.len());
    for (i, (driver, target)) in targets.iter().enumerate() {
        let name = format!("db{}", i + 1);
        let id = driver_id(driver)?;
        let config = connection_config_named(&name, driver, target)?;
        let session = registry
            .connect(&config)
            .await
            .map_err(|e| anyhow!("{name}: {}", driver_message(&e)))?;
        let (catalog, worker) = Catalog::new(session);
        tokio::spawn(async move {
            let _ = worker.run().await;
        });
        entries.push(Entry {
            info: ConnectionInfo::new(&name, &name, id, None)
                .with_dialect(crate::dialect_of_driver(driver)),
            config,
            catalog,
        });
    }
    Ok(CliSource { registry, entries })
}

async fn serve(args: &McpArgs) -> Result<()> {
    let dir = ConfigDir::platform().context("cannot find the configuration directory")?;
    let ai = dir.load_ai_config().map_err(|e| anyhow!("ai.toml: {e}"))?;
    let token = match args.transport {
        McpTransport::Http { .. } => Some(resolve_token(args.token_file.as_deref())?),
        McpTransport::Stdio => None,
    };
    let source = Arc::new(connect_all(&args.targets).await?);
    let tools = ToolSet::configured(source, &ai);
    let audit = Arc::new(AuditLog::open(&dir));
    let server = McpServer::new(tools, audit, ServerOptions::default());
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match (args.transport, token) {
        (McpTransport::Http { port }, Some((token, show))) => {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let http = server.bind_http(addr, token.clone()).await?;
            let mut out = io::stdout().lock();
            writeln!(out, "sqler MCP server: {}", http.url())?;
            if show {
                writeln!(out, "Authorization: Bearer {}", token.expose())?;
            }
            out.flush()?;
            drop(out);
            http.run(shutdown).await?;
        }
        _ => server.serve_stdio(shutdown).await?,
    }
    Ok(())
}

/// The HTTP token and whether the start block must show it (generated, with no file to read it
/// from).
fn resolve_token(file: Option<&Path>) -> Result<(AuthToken, bool)> {
    let env = std::env::var(TOKEN_ENV).ok().filter(|v| !v.is_empty());
    match (env, file) {
        (Some(_), Some(_)) => bail!("give {TOKEN_ENV} or --token-file, not both"),
        (Some(text), None) => Ok((
            AuthToken::new(text).map_err(|e| anyhow!("{TOKEN_ENV}: {e}"))?,
            false,
        )),
        (None, Some(path)) => Ok((token_from_file(path)?, false)),
        (None, None) => Ok((AuthToken::generate()?, true)),
    }
}

/// Reads the token file, or creates it (mode 0600 on Unix) with a generated token. The mode and the
/// "readable by others" refusal are Unix only; on Windows the file inherits the directory ACL.
fn token_from_file(path: &Path) -> Result<AuthToken> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .context("cannot read the token file")?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                bail!("the token file is readable by other users; run chmod 600 on it");
            }
        }
        let text = std::fs::read_to_string(path).context("cannot read the token file")?;
        return AuthToken::new(text).map_err(|e| anyhow!("token file: {e}"));
    }
    let token = AuthToken::generate()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).context("cannot create the token file")?;
    file.write_all(token.expose().as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .context("cannot write the token file")?;
    Ok(token)
}
