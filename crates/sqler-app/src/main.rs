// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `SQLer` desktop application: wires config, drivers and the `GPUI` UI.

#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ai;
mod args;
mod connect;
mod open_sessions;
#[cfg(feature = "plugins")]
mod plugins;
#[cfg(feature = "update")]
mod update;

use std::process::ExitCode;
use std::sync::Arc;

use sqler_config::{ConfigDir, KeychainStore, Platform, SecretStore};
use sqler_driver_api::{DriverError, DriverRegistry};
use sqler_ui::ai::AiStartup;
use sqler_ui::connections::Connector;
use sqler_ui::export::FormatSource;
use sqler_ui::settings::SettingsModel;
use sqler_ui::workbench::WorkbenchStartup;
use sqler_ui::workspace::{Startup, run};

use crate::args::{Command, USAGE, UsageError};
use crate::connect::{Pipeline, SshTunnels};

fn main() -> ExitCode {
    // With SQL Server linked, rustls has two providers compiled in and an implicit one would panic (sqlx verify-ca):
    // ring is installed before anything connects (ADR-0071).
    #[cfg(feature = "driver-mssql")]
    sqler_driver_mssql::install_ring_provider();
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
            open_sessions,
            plugin_dirs,
        }) => start(exit_after_first_frame, open_sessions, plugin_dirs),
        Err(UsageError(msg)) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn start(
    exit_after_first_frame: bool,
    open_sessions: bool,
    plugin_dirs: Vec<std::path::PathBuf>,
) -> ExitCode {
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
        .and_then(|dir| dir.load_connection_store().map_err(|err| err.to_string()));
    // Saving is only offered when the file loaded: a broken file is never overwritten.
    let config_dir = dir.as_ref().ok().cloned().filter(|_| connections.is_ok());
    let history_dir = dir.as_ref().ok().cloned();
    let keybindings = dir.as_ref().map_err(ToString::to_string).and_then(|dir| {
        dir.load_keybinding_overrides(Platform::current())
            .map_err(|err| err.to_string())
    });
    let secrets: Arc<dyn SecretStore> = Arc::new(KeychainStore::new());
    let connector: Arc<dyn Connector> = Arc::new(Pipeline::new(
        Arc::new(registry),
        Arc::clone(&secrets),
        Arc::new(SshTunnels),
        sqler_tunnel::default_known_hosts_path(),
        runtime.handle().clone(),
    ));
    if open_sessions {
        // Copied before `Startup` takes the store; the error text is dropped on purpose.
        let profiles = connections
            .as_ref()
            .map(|store| store.connections().to_vec())
            .map_err(|_| ());
        open_sessions::spawn(runtime.handle(), Arc::clone(&connector), profiles);
    }
    let workbench = WorkbenchStartup::load(dir.as_ref().ok().cloned());
    // Reads `ai.toml` only; no provider is built and the keychain is not touched until the user
    // tests or uses a provider.
    let ai = AiStartup::load(
        dir.as_ref().ok().cloned(),
        Arc::new(ai::Providers::new(Arc::clone(&secrets))),
    );
    // Builds the grant book and a backend that does nothing until the first refresh, which the window requests after
    // the first frame: no plugin is discovered, compiled or instantiated here.
    #[cfg(feature = "plugins")]
    let plugins = plugins::startup(dir.as_ref().ok(), plugin_dirs, Arc::clone(&connector));
    #[cfg(not(feature = "plugins"))]
    let plugins = {
        drop(plugin_dirs);
        sqler_ui::plugins::PluginsStartup::unavailable()
    };
    // Spawns one idle task: no client is built and nothing is requested until the window starts it after the first
    // frame, and only while `check_for_updates` is on.
    #[cfg(feature = "update")]
    let update = update::startup(dir.as_ref().ok(), runtime.handle());
    #[cfg(not(feature = "update"))]
    let update = sqler_ui::update::UpdateStartup::unavailable();
    #[cfg(feature = "plugins")]
    let export_sources = vec![plugins::exporters::source() as Arc<dyn FormatSource>];
    #[cfg(not(feature = "plugins"))]
    let export_sources: Vec<Arc<dyn FormatSource>> = Vec::new();
    run(Startup {
        connections,
        config_dir,
        connector,
        secrets,
        runtime: runtime.handle().clone(),
        exit_after_first_frame,
        settings: SettingsModel::load(dir),
        keybindings,
        history_dir,
        workbench,
        ai,
        plugins,
        update,
        export_sources,
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
    // Drivers of plugins are asked after the built-in ones; the source offers nothing until the plugin host exists.
    #[cfg(feature = "plugins")]
    registry.add_source(plugins::drivers::source());
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

    /// With the `plugins` feature the registry asks the process-wide plugin source after the built-in drivers. The
    /// source has no host in this test, so it knows no driver, and `available_ids` adds nothing to `ids`.
    #[cfg(feature = "plugins")]
    #[tokio::test]
    async fn registry_asks_the_plugin_source() {
        use sqler_driver_api::DriverError;
        use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

        let registry = super::registry().unwrap();
        let built_in: Vec<_> = registry.ids().cloned().collect();
        assert_eq!(registry.available_ids(), built_in);
        let config = ConnectionConfig::new("t", DriverId::new("plugin-only").unwrap(), "x");
        let err = registry.connect(&config).await.err().unwrap();
        assert!(matches!(err, DriverError::UnknownDriver(_)), "{err:?}");
    }
}
