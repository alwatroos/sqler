// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The workbench state: panel layout, focus regions, recent commands and connections, the status
//! bar model and the `workbench:*` / `pane:*` commands. Plain Rust, no `GPUI`.

mod action;
mod layout;
mod recent;
mod status;

use sqler_config::{ConfigDir, WorkbenchLoad};

pub use action::{action_for, register_commands, register_pane_commands};
pub use layout::{
    PANEL_DEFAULT, PANEL_MAX_ABS, PANEL_MAX_FRACTION, PANEL_MIN, PanelView, Region, SIDE_DEFAULT,
    SIDE_MAX, SIDE_MIN, SideView, WorkbenchAction, WorkbenchLayout, next_region,
};
pub use recent::{RecentCommands, RecentConnections};
#[allow(unused_imports)] // `format_duration` is for the workspace wiring in this crate
pub(crate) use status::format_duration;
pub use status::{RowInfo, RunIndicator, StatusBarModel, database_label, format_rows};

/// Where `workbench.toml` lives and what it held at startup.
#[derive(Debug, Clone)]
pub struct WorkbenchStartup {
    /// The configuration directory, if there is one.
    pub dir: Option<ConfigDir>,
    /// What loading `workbench.toml` gave.
    pub load: WorkbenchLoad,
}

impl WorkbenchStartup {
    /// Loads `workbench.toml` from `dir`; without a directory the result is
    /// [`WorkbenchLoad::Missing`].
    #[must_use]
    pub fn load(dir: Option<ConfigDir>) -> Self {
        let load = dir
            .as_ref()
            .map_or(WorkbenchLoad::Missing, ConfigDir::load_workbench_state);
        Self { dir, load }
    }
}

impl Default for WorkbenchStartup {
    fn default() -> Self {
        Self::load(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dir_is_missing() {
        let s = WorkbenchStartup::load(None);
        assert!(s.dir.is_none());
        assert_eq!(s.load, WorkbenchLoad::Missing);
    }

    #[test]
    fn dir_without_file_is_missing() {
        let p = std::env::temp_dir().join(format!("sqler-ui-wb-{}", std::process::id()));
        let s = WorkbenchStartup::load(Some(ConfigDir::new(&p)));
        assert_eq!(s.load, WorkbenchLoad::Missing);
        assert!(s.dir.is_some());
    }
}
