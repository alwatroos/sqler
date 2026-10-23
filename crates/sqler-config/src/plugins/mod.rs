// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin permission grants: what the user approved or denied per plugin, in the versioned
//! `plugin-grants.toml` file (ADR-0059).
//!
//! The file holds validated strings only; the typed parse of keys and capabilities belongs to
//! the layer that depends on the plugin API. A damaged, newer, unreadable or other-writable
//! file is reported, never repaired and never overwritten.

mod file;
mod model;
mod prefs;

pub use file::PLUGIN_GRANTS_FORMAT_VERSION;
pub use model::{
    GrantDecision, GrantRecord, GrantRecordError, MAX_GRANT_CAPABILITIES, MAX_PLUGIN_GRANTS,
    PluginGrants,
};
pub use prefs::{
    MAX_PREF_PLUGINS, MAX_PREF_SETTINGS, MAX_PREF_TEXT_BYTES, PLUGIN_PREFS_FORMAT_VERSION,
    PluginPrefs, PrefValue, PrefsLoad,
};

use std::fs;
use std::io::Read as _;
use std::path::PathBuf;

use crate::fs::write_atomic_private;
use crate::{ConfigDir, ConfigError};

/// The file is read at most up to this size.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The outcome of loading `plugin-grants.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantsLoad {
    /// There is no file: nothing was ever decided.
    Missing,
    /// The file was read and is valid.
    Loaded(PluginGrants),
    /// The file is damaged, unreadable, too large or writable by other users. Grant nothing and
    /// do not overwrite it.
    Invalid(String),
    /// The file was written by a newer version (the number found). Grant nothing and do not
    /// overwrite it.
    Newer(u32),
}

impl ConfigDir {
    /// Path of `plugin-grants.toml`, at the root next to `ai.toml`.
    #[must_use]
    pub fn plugin_grants_path(&self) -> PathBuf {
        self.root().join("plugin-grants.toml")
    }

    /// Loads `plugin-grants.toml`. Never fails and never writes: any problem is a
    /// [`GrantsLoad::Invalid`] or [`GrantsLoad::Newer`] result, which callers treat as
    /// "nothing is granted".
    #[must_use]
    pub fn load_plugin_grants(&self) -> GrantsLoad {
        let path = self.plugin_grants_path();
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return GrantsLoad::Missing,
            Err(e) => return GrantsLoad::Invalid(format!("cannot inspect the file: {}", e.kind())),
        };
        if !meta.is_file() {
            return GrantsLoad::Invalid("not a regular file".into());
        }
        if meta.len() > MAX_FILE_BYTES {
            return GrantsLoad::Invalid("file is larger than 1 MiB".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o022 != 0 {
                return GrantsLoad::Invalid("writable by other users".into());
            }
        }
        let mut bytes = Vec::new();
        let read =
            fs::File::open(&path).and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
        if let Err(e) = read {
            return GrantsLoad::Invalid(format!("cannot read the file: {}", e.kind()));
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return GrantsLoad::Invalid("file is larger than 1 MiB".into());
        }
        match String::from_utf8(bytes) {
            Ok(text) => file::parse(&text),
            Err(_) => GrantsLoad::Invalid("not valid UTF-8".into()),
        }
    }

    /// Writes `grants` to `plugin-grants.toml` atomically, mode `0600` on Unix.
    ///
    /// The file is loaded first: if it is damaged, newer, unreadable or other-writable, nothing
    /// is written and its bytes stay as they were.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] ("refusing to overwrite") for such a file, or when `grants` holds
    /// more than [`MAX_PLUGIN_GRANTS`] records; [`ConfigError::Write`] for a write failure.
    pub fn save_plugin_grants(&self, grants: &PluginGrants) -> Result<(), ConfigError> {
        let path = self.plugin_grants_path();
        let invalid = |message: String| ConfigError::Invalid {
            path: path.clone(),
            message,
        };
        match self.load_plugin_grants() {
            GrantsLoad::Invalid(why) => {
                return Err(invalid(format!("refusing to overwrite the file: {why}")));
            }
            GrantsLoad::Newer(v) => {
                return Err(invalid(format!(
                    "refusing to overwrite a file of newer format version {v}"
                )));
            }
            GrantsLoad::Missing | GrantsLoad::Loaded(_) => {}
        }
        if grants.len() > MAX_PLUGIN_GRANTS {
            return Err(invalid(format!(
                "more than {MAX_PLUGIN_GRANTS} grant records"
            )));
        }
        let text = file::serialize(grants)
            .map_err(|e| invalid(format!("cannot serialise the grants: {e}")))?;
        write_atomic_private(&path, text)
    }
}

/// Line and column of a parse error, never the source text (it may be user-chosen content).
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
