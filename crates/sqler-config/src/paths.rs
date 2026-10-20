// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::ConfigError;

/// Operating-system family used to pick the configuration directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Linux,
    Mac,
    Windows,
}

impl Os {
    const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Computes the configuration directory from injected environment lookups.
pub(crate) fn resolve(
    os: Os,
    var: impl Fn(&str) -> Option<OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    match os {
        Os::Mac => home.map(|h| h.join("Library").join("Application Support").join("SQLer")),
        Os::Windows => var("APPDATA")
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("SQLer")),
        Os::Linux => var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("sqler"))
            .or_else(|| home.map(|h| h.join(".config").join("sqler"))),
    }
}

/// Handle on the directory holding `settings.toml`, `keybindings.toml` and
/// `connections.toml`.
#[derive(Debug, Clone)]
pub struct ConfigDir {
    root: PathBuf,
}

impl ConfigDir {
    /// The platform configuration directory.
    ///
    /// # Errors
    /// Returns [`ConfigError::NoConfigDir`] when it cannot be determined.
    pub fn platform() -> Result<Self, ConfigError> {
        resolve(Os::current(), |k| std::env::var_os(k), std::env::home_dir())
            .map(Self::new)
            .ok_or(ConfigError::NoConfigDir)
    }

    /// A handle on an explicit directory (used by tests and embedders).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of `settings.toml`.
    #[must_use]
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// Path of `keybindings.toml`.
    #[must_use]
    pub fn keybindings_path(&self) -> PathBuf {
        self.root.join("keybindings.toml")
    }

    /// Path of `connections.toml`.
    #[must_use]
    pub fn connections_path(&self) -> PathBuf {
        self.root.join("connections.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    fn abs(s: &str) -> PathBuf {
        std::env::temp_dir().join(s)
    }

    #[test]
    fn linux_prefers_absolute_xdg() {
        let xdg = abs("xdg");
        let xdg_s = xdg.to_str().unwrap().to_owned();
        let got = resolve(
            Os::Linux,
            env(&[("XDG_CONFIG_HOME", &xdg_s)]),
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(got, Some(xdg.join("sqler")));
    }

    #[test]
    fn linux_ignores_relative_xdg() {
        let got = resolve(
            Os::Linux,
            env(&[("XDG_CONFIG_HOME", "rel/dir")]),
            Some(PathBuf::from("/home/u")),
        );
        assert_eq!(
            got,
            Some(PathBuf::from("/home/u").join(".config").join("sqler"))
        );
    }

    #[test]
    fn linux_falls_back_to_home() {
        let got = resolve(Os::Linux, env(&[]), Some(PathBuf::from("/home/u")));
        assert_eq!(
            got,
            Some(PathBuf::from("/home/u").join(".config").join("sqler"))
        );
    }

    #[test]
    fn macos_uses_application_support() {
        let got = resolve(Os::Mac, env(&[]), Some(PathBuf::from("/Users/u")));
        assert_eq!(
            got,
            Some(
                PathBuf::from("/Users/u")
                    .join("Library")
                    .join("Application Support")
                    .join("SQLer")
            )
        );
    }

    #[test]
    fn windows_uses_appdata() {
        let got = resolve(Os::Windows, env(&[("APPDATA", "C:\\A")]), None);
        assert_eq!(got, Some(PathBuf::from("C:\\A").join("SQLer")));
        assert_eq!(
            resolve(Os::Windows, env(&[]), Some(PathBuf::from("/h"))),
            None
        );
    }

    #[test]
    fn missing_home_gives_none() {
        assert_eq!(resolve(Os::Linux, env(&[]), None), None);
        assert_eq!(resolve(Os::Mac, env(&[]), None), None);
    }

    #[test]
    fn file_paths() {
        let d = ConfigDir::new("cfg");
        assert_eq!(d.root(), Path::new("cfg"));
        assert_eq!(d.settings_path(), Path::new("cfg").join("settings.toml"));
        assert_eq!(
            d.keybindings_path(),
            Path::new("cfg").join("keybindings.toml")
        );
        assert_eq!(
            d.connections_path(),
            Path::new("cfg").join("connections.toml")
        );
    }
}
