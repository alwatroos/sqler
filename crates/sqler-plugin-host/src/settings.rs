// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin settings: the store trait and the declared-key rules.

use std::collections::HashMap;
use std::mem::discriminant;
use std::sync::Mutex;

use sqler_plugin_api::{Manifest, PluginId, SettingKey, SettingSpec, SettingValue};

use crate::error::SettingsError;

/// Where setting values are kept (P7 backs it with the config directory).
pub trait SettingsStore: Send + Sync {
    /// The stored value, if any.
    fn get(&self, plugin: &PluginId, key: &SettingKey) -> Option<SettingValue>;
    /// Stores a value.
    ///
    /// # Errors
    /// [`SettingsError::Store`] when the backing store fails.
    fn set(
        &self,
        plugin: &PluginId,
        key: &SettingKey,
        value: SettingValue,
    ) -> Result<(), SettingsError>;
}

/// An in-memory [`SettingsStore`] (tests, and hosts without persistence).
#[derive(Debug, Default)]
pub struct MemorySettings(Mutex<HashMap<(PluginId, SettingKey), SettingValue>>);

impl SettingsStore for MemorySettings {
    fn get(&self, plugin: &PluginId, key: &SettingKey) -> Option<SettingValue> {
        let map = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(&(plugin.clone(), key.clone())).cloned()
    }

    fn set(
        &self,
        plugin: &PluginId,
        key: &SettingKey,
        value: SettingValue,
    ) -> Result<(), SettingsError> {
        let mut map = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert((plugin.clone(), key.clone()), value);
        Ok(())
    }
}

fn declared<'m>(m: &'m Manifest, key: &str) -> Option<(SettingKey, &'m SettingSpec)> {
    let key: SettingKey = key.parse().ok()?;
    let spec = m.contributes.settings.iter().find(|s| s.key == key)?;
    Some((key, spec))
}

fn type_name(v: &SettingValue) -> &'static str {
    match v {
        SettingValue::Boolean(_) => "a boolean",
        SettingValue::Integer(_) => "an integer",
        SettingValue::Float(_) => "a float",
        SettingValue::Text(_) => "text",
        _ => "another type",
    }
}

/// The value of a declared setting: the stored one, or the declared default when nothing is stored or the stored
/// value has another type than the default. `None` for a key the manifest does not declare.
#[must_use]
pub fn read_setting(m: &Manifest, s: &dyn SettingsStore, key: &str) -> Option<SettingValue> {
    let (key, spec) = declared(m, key)?;
    match s.get(&m.id, &key) {
        Some(v) if discriminant(&v) == discriminant(&spec.default) => Some(v),
        _ => Some(spec.default.clone()),
    }
}

/// Writes a declared setting (the app's side; the guest's WIT view is read-only).
///
/// # Errors
/// [`SettingsError::Undeclared`] for an unknown or malformed key, [`SettingsError::TypeMismatch`] when the value's
/// type differs from the declared default, [`SettingsError::Store`] from the store.
pub fn write_setting(
    m: &Manifest,
    s: &dyn SettingsStore,
    key: &str,
    v: SettingValue,
) -> Result<(), SettingsError> {
    let Some((parsed, spec)) = declared(m, key) else {
        return Err(SettingsError::Undeclared {
            key: key.chars().take(64).collect(),
        });
    };
    if discriminant(&v) != discriminant(&spec.default) {
        return Err(SettingsError::TypeMismatch {
            key: parsed.to_string(),
            expected: type_name(&spec.default),
        });
    }
    s.set(&m.id, &parsed, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
[plugin]
id = "demo"
name = "Demo"
version = "1.0.0"
api = "0.1"
capabilities = []

[[settings]]
key = "url"
label = "URL"
default = ""

[[settings]]
key = "limit"
label = "Limit"
default = 5
"#,
        )
        .unwrap()
    }

    #[test]
    fn default_when_nothing_is_stored() {
        let (m, s) = (manifest(), MemorySettings::default());
        assert_eq!(
            read_setting(&m, &s, "url"),
            Some(SettingValue::Text(String::new()))
        );
        assert_eq!(
            read_setting(&m, &s, "limit"),
            Some(SettingValue::Integer(5))
        );
    }

    #[test]
    fn stored_value_wins_and_wrong_variant_falls_back() {
        let (m, s) = (manifest(), MemorySettings::default());
        write_setting(&m, &s, "limit", SettingValue::Integer(9)).unwrap();
        assert_eq!(
            read_setting(&m, &s, "limit"),
            Some(SettingValue::Integer(9))
        );
        // A store that holds a value of another type (edited by hand, older manifest) yields the default.
        s.set(
            &m.id,
            &"limit".parse().unwrap(),
            SettingValue::Text("x".into()),
        )
        .unwrap();
        assert_eq!(
            read_setting(&m, &s, "limit"),
            Some(SettingValue::Integer(5))
        );
    }

    #[test]
    fn undeclared_and_malformed_keys() {
        let (m, s) = (manifest(), MemorySettings::default());
        assert_eq!(read_setting(&m, &s, "nope"), None);
        assert_eq!(read_setting(&m, &s, "Bad Key"), None);
        assert!(matches!(
            write_setting(&m, &s, "nope", SettingValue::Boolean(true)),
            Err(SettingsError::Undeclared { .. })
        ));
        assert!(matches!(
            write_setting(&m, &s, "../x", SettingValue::Boolean(true)),
            Err(SettingsError::Undeclared { .. })
        ));
    }

    #[test]
    fn type_mismatch_is_refused_and_nothing_is_stored() {
        let (m, s) = (manifest(), MemorySettings::default());
        let err = write_setting(&m, &s, "limit", SettingValue::Text("1".into())).unwrap_err();
        assert!(matches!(
            err,
            SettingsError::TypeMismatch {
                expected: "an integer",
                ..
            }
        ));
        assert_eq!(s.get(&m.id, &"limit".parse().unwrap()), None);
    }
}
