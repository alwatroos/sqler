// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Where plugins live on disk.

use std::path::PathBuf;

/// The directories discovery looks at and the roots scopes resolve against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginDirs {
    /// The user's plugin directory (`<config>/plugins`); each child is a plugin, `<id>/data` is its data root.
    ///
    /// Callers pass an absolute, canonical path (and keep the data last segment spelled exactly `data`): a relative
    /// path such as `./plugins` is refused by the scope rules and shows up as a scope that escapes, without a hint.
    pub config_plugins: Option<PathBuf>,
    /// Development paths (`--plugin-dir`): a path holding `sqler.toml` is one plugin, otherwise its children are
    /// scanned. They win over `config_plugins` for a duplicate id.
    pub dev: Vec<PathBuf>,
    /// The user's home directory, the root of `home/...` scopes.
    pub home: Option<PathBuf>,
}

impl PluginDirs {
    /// The `data` root of plugin `id`: `<config_plugins>/<id>/data`. Without a config directory a plugin gets
    /// `<plugin dir>/data`, so a development plugin can still keep data next to it.
    pub(crate) fn data_dir(&self, id: &str, plugin_dir: &std::path::Path) -> PathBuf {
        match &self.config_plugins {
            Some(c) => c.join(id).join("data"),
            None => plugin_dir.join("data"),
        }
    }
}
