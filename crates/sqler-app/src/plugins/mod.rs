// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin support of the desktop app (feature `plugins`, ADR-0060): the host behind the UI's `PluginsBackend`.
//!
//! [`startup`] only builds objects. It reads `plugin-grants.toml` (the grant book the dialog decides in) and nothing
//! else: no folder is scanned, no runtime or engine exists, no plugin is compiled or instantiated. The window asks for
//! the first `refresh` after its first frame.

mod approver;
mod backend;
mod data;
mod dirs;
pub(crate) mod drivers;
mod settings;
mod ui;

#[cfg(test)]
mod scratch;

use std::path::PathBuf;
use std::sync::Arc;

use sqler_config::ConfigDir;
use sqler_plugin_host::{PluginData, PluginUi, Services};
use sqler_ui::connections::Connector;
use sqler_ui::plugins::backend::{UiSender, ui_channel};
use sqler_ui::plugins::grant::GrantBook;
use sqler_ui::plugins::{PluginsStartup, WriteSender};

use approver::UiApprover;
use backend::AppPlugins;
use data::AppData;
use settings::Prefs;
use ui::AppUiSink;

/// The host services every plugin sees: the saved connections (opened on `connector`, one fresh session per
/// statement), the approver that sends every write to the window through `writes`, and the UI sink that sends status
/// texts, panel refreshes and notifications to the window through `ui`.
fn services(
    config: Option<&ConfigDir>,
    connector: Arc<dyn Connector>,
    writes: WriteSender,
    ui: UiSender,
) -> Services {
    let data = PluginData::new(Arc::new(AppData::new(config.cloned(), connector)))
        .with_approver(Arc::new(UiApprover::new(writes)));
    let mut services = Services::new();
    services.insert(Arc::new(data));
    services.insert(Arc::new(PluginUi::new(Arc::new(AppUiSink(ui)))));
    services
}

/// The plugin side of `Startup`. `dev` is the list of `--plugin-dir` paths, the only development paths there are.
///
/// Without a configuration folder the grant book and the preferences live in memory, so nothing a plugin asks for
/// is ever approved on disk and no plugin folder is searched except `dev`.
pub(crate) fn startup(
    config: Option<&ConfigDir>,
    dev: Vec<PathBuf>,
    connector: Arc<dyn Connector>,
) -> PluginsStartup {
    let grants =
        Arc::new(config.map_or_else(GrantBook::in_memory, |dir| GrantBook::open(dir.clone())));
    let prefs = Arc::new(Prefs::new(config.cloned()));
    let (writes, inbox) = sqler_ui::plugins::write_channel();
    let (ui_sender, ui_inbox) = ui_channel();
    let backend = AppPlugins::new(
        Arc::clone(&grants),
        prefs,
        config,
        dev,
        std::env::home_dir(),
    )
    .with_services(services(config, connector, writes, ui_sender));
    PluginsStartup::new(Some(Arc::new(backend)), grants)
        .with_writes(inbox)
        .with_ui_events(ui_inbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scratch::Scratch;
    use sqler_driver_api::sqler_core::DriverId;
    use sqler_driver_api::{BoxFuture, Session};
    use sqler_ui::connections::{ConnectFailure, ConnectRequest, HostKeyPrompt};

    /// A connector that never connects.
    struct Offline;

    impl Connector for Offline {
        fn driver_ids(&self) -> Vec<DriverId> {
            Vec::new()
        }

        fn connect(
            &self,
            _request: ConnectRequest,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
            Box::pin(std::future::ready(Err(ConnectFailure::Aborted)))
        }

        fn trust_host_key(
            &self,
            _key: HostKeyPrompt,
        ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn startup(config: Option<&ConfigDir>, dev: Vec<PathBuf>) -> PluginsStartup {
        super::startup(config, dev, Arc::new(Offline))
    }

    #[test]
    fn startup_hands_the_window_the_inbox_of_write_approvals() {
        let started = startup(None, Vec::new());
        assert!(started.writes.is_set());
    }

    #[test]
    fn startup_hands_the_window_the_inbox_of_plugin_ui_events() {
        let started = startup(None, Vec::new());
        assert!(started.ui_events.is_set());
    }

    #[test]
    fn startup_builds_a_backend_and_does_no_plugin_work() {
        let s = Scratch::new("startup");
        let dir = ConfigDir::new(s.path());
        let started = startup(Some(&dir), Vec::new());
        assert!(started.backend.is_some());
        // Nothing was scanned or created.
        assert_eq!(std::fs::read_dir(s.path()).unwrap().count(), 0);
    }

    #[test]
    fn startup_without_a_config_folder_still_gives_a_backend() {
        let started = startup(None, Vec::new());
        assert!(started.backend.is_some());
    }
}
