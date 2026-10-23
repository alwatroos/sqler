// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Per-plugin preferences in the versioned `plugin-prefs.toml`: which plugins the user switched off and the values
//! of their settings. Not security-relevant (the grants live in `plugin-grants.toml`), but handled the same way: a
//! damaged, newer, unreadable or other-writable file is reported, never repaired and never overwritten.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::parse_message;
use crate::fs::write_atomic_private;
use crate::{ConfigDir, ConfigError};

/// Version of the `plugin-prefs.toml` format this build writes and reads.
pub const PLUGIN_PREFS_FORMAT_VERSION: u32 = 1;

/// The file is read at most up to this size.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Most plugins that may appear in the file (disabled or with settings).
pub const MAX_PREF_PLUGINS: usize = 1024;
/// Most settings kept per plugin.
pub const MAX_PREF_SETTINGS: usize = 256;
/// Longest id or setting key, in bytes.
const MAX_NAME_BYTES: usize = 128;
/// Longest text value, in bytes.
pub const MAX_PREF_TEXT_BYTES: usize = 4096;

/// A setting value as stored.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefValue {
    /// A boolean.
    Boolean(bool),
    /// An integer.
    Integer(i64),
    /// A finite float.
    Float(f64),
    /// Text.
    Text(String),
}

/// What the user chose per plugin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginPrefs {
    /// Ids of plugins the user switched off. Everything else is enabled.
    pub disabled: BTreeSet<String>,
    /// Setting values by plugin id, then by setting key.
    pub settings: BTreeMap<String, BTreeMap<String, PrefValue>>,
}

/// The outcome of loading `plugin-prefs.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum PrefsLoad {
    /// There is no file: nothing was ever chosen.
    Missing,
    /// The file was read and is valid.
    Loaded(PluginPrefs),
    /// The file is damaged, unreadable, too large or writable by other users. Use defaults and do not overwrite it.
    Invalid(String),
    /// The file was written by a newer version (the number found). Use defaults and do not overwrite it.
    Newer(u32),
}

/// Reads only the version, so a newer file is recognised whatever else it holds.
#[derive(Deserialize)]
struct Probe {
    version: Option<toml::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    disabled: Vec<String>,
    #[serde(default)]
    settings: BTreeMap<String, BTreeMap<String, toml::Value>>,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_BYTES && !s.chars().any(char::is_control)
}

fn value_ok(v: &PrefValue) -> Result<(), String> {
    match v {
        PrefValue::Float(f) if !f.is_finite() => Err("a float must be finite".into()),
        PrefValue::Text(t) if t.len() > MAX_PREF_TEXT_BYTES => Err("text is too long".into()),
        _ => Ok(()),
    }
}

fn check(prefs: &PluginPrefs) -> Result<(), String> {
    let mut ids: BTreeSet<&str> = prefs.disabled.iter().map(String::as_str).collect();
    ids.extend(prefs.settings.keys().map(String::as_str));
    if ids.len() > MAX_PREF_PLUGINS {
        return Err(format!("more than {MAX_PREF_PLUGINS} plugins"));
    }
    if !ids.iter().all(|i| name_ok(i)) {
        return Err("a plugin id is empty, too long or has control characters".into());
    }
    for values in prefs.settings.values() {
        if values.len() > MAX_PREF_SETTINGS {
            return Err(format!(
                "more than {MAX_PREF_SETTINGS} settings for a plugin"
            ));
        }
        for (k, v) in values {
            if !name_ok(k) {
                return Err("a setting key is empty, too long or has control characters".into());
            }
            value_ok(v)?;
        }
    }
    Ok(())
}

fn parse(text: &str) -> PrefsLoad {
    let probe: Probe = match toml::from_str(text) {
        Ok(p) => p,
        Err(e) => return PrefsLoad::Invalid(parse_message(text, &e)),
    };
    match probe.version {
        Some(toml::Value::Integer(n)) if n >= 1 => {
            if n > i64::from(PLUGIN_PREFS_FORMAT_VERSION) {
                return PrefsLoad::Newer(u32::try_from(n).unwrap_or(u32::MAX));
            }
        }
        Some(toml::Value::Integer(_)) => return PrefsLoad::Invalid("version must be 1".into()),
        Some(_) => return PrefsLoad::Invalid("version must be an integer".into()),
        None => return PrefsLoad::Invalid("missing `version`".into()),
    }
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => return PrefsLoad::Invalid(parse_message(text, &e)),
    };
    let mut prefs = PluginPrefs {
        disabled: file.disabled.into_iter().collect(),
        settings: BTreeMap::new(),
    };
    for (id, values) in file.settings {
        let mut out = BTreeMap::new();
        for (key, v) in values {
            let v = match v {
                toml::Value::Boolean(b) => PrefValue::Boolean(b),
                toml::Value::Integer(i) => PrefValue::Integer(i),
                toml::Value::Float(f) => PrefValue::Float(f),
                toml::Value::String(s) => PrefValue::Text(s),
                _ => return PrefsLoad::Invalid("a setting value has an unsupported type".into()),
            };
            out.insert(key, v);
        }
        prefs.settings.insert(id, out);
    }
    match check(&prefs) {
        Ok(()) => PrefsLoad::Loaded(prefs),
        Err(why) => PrefsLoad::Invalid(why),
    }
}

fn serialize(prefs: &PluginPrefs) -> Result<String, toml::ser::Error> {
    let file = File {
        version: PLUGIN_PREFS_FORMAT_VERSION,
        disabled: prefs.disabled.iter().cloned().collect(),
        settings: prefs
            .settings
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(id, values)| {
                let values = values
                    .iter()
                    .map(|(k, v)| {
                        let v = match v {
                            PrefValue::Boolean(b) => toml::Value::Boolean(*b),
                            PrefValue::Integer(i) => toml::Value::Integer(*i),
                            PrefValue::Float(f) => toml::Value::Float(*f),
                            PrefValue::Text(s) => toml::Value::String(s.clone()),
                        };
                        (k.clone(), v)
                    })
                    .collect();
                (id.clone(), values)
            })
            .collect(),
    };
    toml::to_string(&file)
}

impl ConfigDir {
    /// Path of `plugin-prefs.toml`, at the root next to `plugin-grants.toml`.
    #[must_use]
    pub fn plugin_prefs_path(&self) -> PathBuf {
        self.root().join("plugin-prefs.toml")
    }

    /// Loads `plugin-prefs.toml`. Never fails and never writes: any problem is a [`PrefsLoad::Invalid`] or
    /// [`PrefsLoad::Newer`] result, which callers treat as "all defaults".
    #[must_use]
    pub fn load_plugin_prefs(&self) -> PrefsLoad {
        let path = self.plugin_prefs_path();
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PrefsLoad::Missing,
            Err(e) => return PrefsLoad::Invalid(format!("cannot inspect the file: {}", e.kind())),
        };
        if !meta.is_file() {
            return PrefsLoad::Invalid("not a regular file".into());
        }
        if meta.len() > MAX_FILE_BYTES {
            return PrefsLoad::Invalid("file is larger than 1 MiB".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o022 != 0 {
                return PrefsLoad::Invalid("writable by other users".into());
            }
        }
        let mut bytes = Vec::new();
        let read =
            fs::File::open(&path).and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
        if let Err(e) = read {
            return PrefsLoad::Invalid(format!("cannot read the file: {}", e.kind()));
        }
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return PrefsLoad::Invalid("file is larger than 1 MiB".into());
        }
        match String::from_utf8(bytes) {
            Ok(text) => parse(&text),
            Err(_) => PrefsLoad::Invalid("not valid UTF-8".into()),
        }
    }

    /// Writes `prefs` to `plugin-prefs.toml` atomically, mode `0600` on Unix.
    ///
    /// The file is loaded first: if it is damaged, newer, unreadable or other-writable, nothing is written and its
    /// bytes stay as they were.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] ("refusing to overwrite") for such a file, or when `prefs` breaks the limits (counts,
    /// name and text lengths, a non-finite float); [`ConfigError::Write`] for a write failure.
    pub fn save_plugin_prefs(&self, prefs: &PluginPrefs) -> Result<(), ConfigError> {
        let path = self.plugin_prefs_path();
        let invalid = |message: String| ConfigError::Invalid {
            path: path.clone(),
            message,
        };
        match self.load_plugin_prefs() {
            PrefsLoad::Invalid(why) => {
                return Err(invalid(format!("refusing to overwrite the file: {why}")));
            }
            PrefsLoad::Newer(v) => {
                return Err(invalid(format!(
                    "refusing to overwrite a file of newer format version {v}"
                )));
            }
            PrefsLoad::Missing | PrefsLoad::Loaded(_) => {}
        }
        check(prefs).map_err(invalid)?;
        let text = serialize(prefs)
            .map_err(|e| invalid(format!("cannot serialise the preferences: {e}")))?;
        write_atomic_private(&path, text)
    }
}
