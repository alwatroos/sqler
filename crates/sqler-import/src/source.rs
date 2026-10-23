// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `Source` trait: one implementation per foreign tool.

use std::path::{Path, PathBuf};

use crate::error::ImportError;
use crate::model::SourceReport;

/// The operating-system family, used to guess where a tool keeps its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    /// Linux and other Unix systems.
    Linux,
    /// macOS.
    Mac,
    /// Windows.
    Windows,
}

/// The environment the default locations are computed from. Injected so tests never read the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEnv {
    /// The operating system.
    pub os: HostOs,
    /// The home directory.
    pub home: Option<PathBuf>,
    /// The `APPDATA` directory (Windows).
    pub appdata: Option<PathBuf>,
}

impl HostEnv {
    /// The environment of this process.
    #[must_use]
    pub fn current() -> Self {
        let os = if cfg!(target_os = "macos") {
            HostOs::Mac
        } else if cfg!(windows) {
            HostOs::Windows
        } else {
            HostOs::Linux
        };
        Self {
            os,
            home: std::env::home_dir(),
            appdata: std::env::var_os("APPDATA")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }
}

/// A foreign tool whose connection list can be imported.
pub trait Source: Send + Sync {
    /// The stable id, for example `pgadmin`.
    fn id(&self) -> &'static str;

    /// The name shown to the user, for example `pgAdmin 4`.
    fn label(&self) -> &'static str;

    /// Candidate locations of the tool's file. They are not checked here; see [`discover`].
    fn default_locations(&self, env: &HostEnv) -> Vec<PathBuf>;

    /// Parses the bytes of a file already read within [`MAX_FILE_BYTES`](crate::MAX_FILE_BYTES).
    ///
    /// Pure: no I/O, no panic on hostile input.
    ///
    /// # Errors
    /// [`ImportError`] when the bytes are not the tool's format.
    fn parse(&self, bytes: &[u8]) -> Result<SourceReport, ImportError>;
}

/// The candidates of `source` that exist and are regular files.
#[must_use]
pub fn discover(source: &dyn Source, env: &HostEnv) -> Vec<PathBuf> {
    source
        .default_locations(env)
        .into_iter()
        .filter(|p| is_regular_file(p))
        .collect()
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}
