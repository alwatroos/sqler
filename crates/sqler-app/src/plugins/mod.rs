// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin support of the desktop app (feature `plugins`, ADR-0060): the host behind the UI's `PluginsBackend`.
//!
//! [`startup`] only builds objects. It reads `plugin-grants.toml` (the grant book the dialog decides in) and nothing
//! else: no folder is scanned, no runtime or engine exists, no plugin is compiled or instantiated. The window asks for
//! the first `refresh` after its first frame.

mod backend;
mod dirs;
mod settings;

#[cfg(test)]
mod scratch;

use std::path::PathBuf;
use std::sync::Arc;

use sqler_config::ConfigDir;
use sqler_plugin_host::Services;
use sqler_ui::plugins::PluginsStartup;
use sqler_ui::plugins::grant::GrantBook;

use backend::AppPlugins;
use settings::Prefs;

/// The host services every plugin sees. Later items add theirs here (the query data service and the write approver).
fn services() -> Services {
    Services::new()
}

/// The plugin side of `Startup`. `dev` is the list of `--plugin-dir` paths, the only development paths there are.
///
/// Without a configuration folder the grant book and the preferences live in memory, so nothing a plugin asks for
/// is ever approved on disk and no plugin folder is searched except `dev`.
pub(crate) fn startup(config: Option<&ConfigDir>, dev: Vec<PathBuf>) -> PluginsStartup {
    let grants =
        Arc::new(config.map_or_else(GrantBook::in_memory, |dir| GrantBook::open(dir.clone())));
    let prefs = Arc::new(Prefs::new(config.cloned()));
    let backend = AppPlugins::new(
        Arc::clone(&grants),
        prefs,
        config,
        dev,
        std::env::home_dir(),
    );
    PluginsStartup::new(Some(Arc::new(backend)), grants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scratch::Scratch;

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
