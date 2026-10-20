// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `SQLer` desktop application: wires config, drivers and the `GPUI` UI.

#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod args;

use std::process::ExitCode;
use std::sync::Arc;

use sqler_config::{ConfigDir, Platform};
use sqler_driver_api::{DriverError, DriverRegistry};
use sqler_ui::settings::SettingsModel;
use sqler_ui::workspace::{Startup, run};

use crate::args::{Command, USAGE, UsageError};

fn main() -> ExitCode {
    match args::parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("sqler {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Run {
            exit_after_first_frame,
        }) => start(exit_after_first_frame),
        Err(UsageError(msg)) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn start(exit_after_first_frame: bool) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("sqler-db")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: failed to start the async runtime: {err}");
            return ExitCode::from(1);
        }
    };
    let registry = match registry() {
        Ok(registry) => registry,
        Err(err) => {
            eprintln!("error: failed to register the drivers: {err}");
            return ExitCode::from(1);
        }
    };
    let dir = ConfigDir::platform();
    let connections = dir
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|dir| dir.load_connections().map_err(|err| err.to_string()));
    let keybindings = dir.as_ref().map_err(ToString::to_string).and_then(|dir| {
        dir.load_keybinding_overrides(Platform::current())
            .map_err(|err| err.to_string())
    });
    run(Startup {
        connections,
        registry: Arc::new(registry),
        runtime: runtime.handle().clone(),
        exit_after_first_frame,
        settings: SettingsModel::load(dir),
        keybindings,
    });
    ExitCode::SUCCESS
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
fn registry() -> Result<DriverRegistry, DriverError> {
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

#[cfg(test)]
mod tests {
    #[cfg(any(
        feature = "driver-sqlite",
        feature = "driver-postgres",
        feature = "driver-mysql"
    ))]
    fn ids() -> Vec<String> {
        super::registry()
            .unwrap()
            .ids()
            .map(ToString::to_string)
            .collect()
    }

    #[cfg(feature = "driver-sqlite")]
    #[test]
    fn registry_lists_sqlite() {
        assert!(ids().contains(&"sqlite".to_string()));
    }

    #[cfg(feature = "driver-postgres")]
    #[test]
    fn registry_lists_postgres() {
        assert!(ids().contains(&"postgres".to_string()));
    }

    #[cfg(feature = "driver-mysql")]
    #[test]
    fn registry_lists_mysql() {
        assert!(ids().contains(&"mysql".to_string()));
    }
}
