// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

#![forbid(unsafe_code)]

mod args;
mod export;
mod output;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{DriverError, DriverRegistry, QueryEvent};

use crate::args::{Command, USAGE, UsageError};
use crate::output::TsvWriter;

fn main() -> ExitCode {
    match args::parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("sqler-cli {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Run {
            driver,
            target,
            sql,
        }) => match run(&driver, &target, &sql) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("error: {err:#}");
                ExitCode::from(1)
            }
        },
        Ok(Command::Export(export_args)) => match export::run(&export_args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => {
                eprintln!("error: {}", failure.message());
                ExitCode::from(failure.exit_code())
            }
        },
        Err(UsageError(msg)) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run(driver: &str, target: &str, sql: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to start the async runtime")?;
    runtime.block_on(run_query(driver, target, sql))
}

// Without any driver feature nothing can fail or be registered.
#[cfg_attr(
    not(any(
        feature = "driver-sqlite",
        feature = "driver-postgres",
        feature = "driver-mysql"
    )),
    allow(clippy::unnecessary_wraps)
)]
pub(crate) fn registry() -> Result<DriverRegistry> {
    #[allow(unused_mut)]
    let mut registry = DriverRegistry::new();
    #[cfg(feature = "driver-sqlite")]
    registry.register(Box::new(sqler_driver_sqlite::SqliteDriver::new()))?;
    #[cfg(feature = "driver-postgres")]
    registry.register(Box::new(sqler_driver_postgres::PostgresDriver::new()))?;
    #[cfg(feature = "driver-mysql")]
    registry.register(Box::new(sqler_driver_mysql::MySqlDriver::new()))?;
    Ok(registry)
}

/// The connection for a CLI target. The name is the driver id: the target (a URL can carry a
/// password) must never end up in a name that errors or logs print.
pub(crate) fn connection_config(driver: &str, target: &str) -> Result<ConnectionConfig> {
    Ok(ConnectionConfig::new(driver, driver_id(driver)?, target))
}

/// The text for an unknown driver. Fixed: the argument may be a URL in the wrong place.
pub(crate) const UNKNOWN_DRIVER: &str = "unknown driver; see --help for the drivers";

/// Parses a driver id. The error never repeats the argument, which can be a URL with a password
/// that landed in the `--driver` slot.
pub(crate) fn driver_id(driver: &str) -> Result<DriverId> {
    DriverId::new(driver).map_err(|_| anyhow!("invalid driver id; run with --help for usage"))
}

/// The text printed for a driver or registry error. Fixed per kind: the texts of the driver layer
/// can carry the target (host, user, parts of a URL) or the argument that was rejected.
pub(crate) fn driver_message(err: &DriverError) -> &'static str {
    match err {
        DriverError::Cancelled => "cancelled",
        DriverError::Connection(_) => {
            "connection failed (details are withheld: they can contain the target)"
        }
        DriverError::Query(_) => "query failed (details are withheld: they can echo the input)",
        DriverError::Unsupported(_) => "the driver does not support this request",
        DriverError::UnknownDriver(_) => UNKNOWN_DRIVER,
        _ => "database error (details are withheld: they can contain the target)",
    }
}

fn driver_error(err: &DriverError) -> anyhow::Error {
    anyhow!(driver_message(err))
}

async fn run_query(driver: &str, target: &str, sql: &str) -> Result<()> {
    let registry = registry()?;
    let config = connection_config(driver, target)?;
    let mut session = registry
        .connect(&config)
        .await
        .map_err(|e| driver_error(&e))?;

    let query = Query::new(sql);
    let summary = {
        let mut stream = session
            .execute(&query)
            .await
            .map_err(|e| driver_error(&e))?;
        let stdout = io::stdout();
        let mut writer = TsvWriter::new(stdout.lock());
        let mut rows = 0_u64;
        let mut has_columns = false;
        let mut summary = String::from("(done)");
        while let Some(event) = stream.next_event().await {
            match event.map_err(|e| driver_error(&e))? {
                QueryEvent::Columns(columns) => {
                    has_columns = !columns.is_empty();
                    writer.columns(&columns)?;
                }
                QueryEvent::Rows(batch) => {
                    rows += batch.len() as u64;
                    writer.rows(&batch)?;
                }
                QueryEvent::Done { rows_affected } => {
                    if has_columns {
                        summary = format!("({rows} rows)");
                    } else if let Some(n) = rows_affected {
                        summary = format!("({n} rows affected)");
                    }
                }
                _ => {}
            }
        }
        writer.flush()?;
        summary
    };
    eprintln!("{summary}");
    io::stderr().flush().ok();

    session.close().await.map_err(|e| driver_error(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_debug_hides_the_url_password() {
        let config = connection_config("postgres", "postgres://u:s3cret@host/db").unwrap();
        assert!(!format!("{config:?}").contains("s3cret"));
        assert_eq!(config.name, "postgres");
    }
}
