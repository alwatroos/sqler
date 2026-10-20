// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The saved-connection store and its `connections.toml` format (ADR-0011).

mod file;
mod model;
mod store;

pub use model::{
    CONNECTIONS_FORMAT_VERSION, ConnectionId, ConnectionProfile, ConnectionSpec, Group, GroupId,
    ProfileSecret, SecretKind, SshAuth, SshProfile, TlsMode, TlsProfile,
};
pub use store::{ConnectionStore, StoreError};

use sqler_core::ConnectionConfig;

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

impl ConfigDir {
    /// Loads `connections.toml` as an editable [`ConnectionStore`]; a missing
    /// file yields an empty store. A file without a `version` key is the M1
    /// format and is migrated in memory; the file is not rewritten until the
    /// next [`ConfigDir::save_connection_store`].
    ///
    /// # Errors
    /// [`ConfigError::Read`], [`ConfigError::Parse`] for malformed TOML, wrong
    /// types or unknown keys, and [`ConfigError::Invalid`] naming the group or
    /// connection (index and name) with a bad value, a duplicate or dangling
    /// id, a parent cycle, or a `version` newer than
    /// [`CONNECTIONS_FORMAT_VERSION`]. A target that embeds a secret is
    /// rejected without being echoed.
    pub fn load_connection_store(&self) -> Result<ConnectionStore, ConfigError> {
        let path = self.connections_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(ConnectionStore::new());
        };
        file::parse(&path, &text)
    }

    /// Writes the store to `connections.toml` as `version = 1`, creating the
    /// directory when missing. The write is atomic: a temporary file is flushed
    /// and renamed over the target. No secret is ever written.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] (for example a non-UTF-8 path) or
    /// [`ConfigError::Write`].
    pub fn save_connection_store(&self, store: &ConnectionStore) -> Result<(), ConfigError> {
        let path = self.connections_path();
        let text = file::serialize(&path, store)?;
        write_atomic(&path, &text)
    }

    /// Loads the saved connections from `connections.toml` in file order,
    /// flattening groups; a missing file yields an empty list.
    ///
    /// # Errors
    /// The errors of [`ConfigDir::load_connection_store`].
    pub fn load_connections(&self) -> Result<Vec<ConnectionConfig>, ConfigError> {
        Ok(self.load_connection_store()?.to_connection_configs())
    }
}

/// Describes a parse error by line, column and message only. The `toml` Display
/// output quotes the offending source line, which may hold a password.
fn parse_message(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            format!("line {line}, column {col}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}
