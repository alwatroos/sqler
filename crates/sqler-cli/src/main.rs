// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

#![forbid(unsafe_code)]

mod args;
mod output;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{DriverRegistry, QueryEvent};

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
fn registry() -> Result<DriverRegistry> {
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
fn connection_config(driver: &str, target: &str) -> Result<ConnectionConfig> {
    Ok(ConnectionConfig::new(
        driver,
        DriverId::new(driver)?,
        target,
    ))
}

async fn run_query(driver: &str, target: &str, sql: &str) -> Result<()> {
    let registry = registry()?;
    let config = connection_config(driver, target)?;
    let mut session = registry.connect(&config).await?;

    let query = Query::new(sql);
    let summary = {
        let mut stream = session.execute(&query).await?;
        let stdout = io::stdout();
        let mut writer = TsvWriter::new(stdout.lock());
        let mut rows = 0_u64;
        let mut has_columns = false;
        let mut summary = String::from("(done)");
        while let Some(event) = stream.next_event().await {
            match event? {
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

    session.close().await?;
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
