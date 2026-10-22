// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The user's plugin preferences (`plugin-prefs.toml`): which plugins are switched off and the values of their
//! settings. The host reads settings through [`SettingsStore`]; the manager switches plugins on and off.

use std::sync::{Mutex, MutexGuard};

use sqler_config::{ConfigDir, PluginPrefs, PrefValue, PrefsLoad};
use sqler_plugin_api::{PluginId, SettingKey, SettingValue};
use sqler_plugin_host::{SettingsError, SettingsStore};

struct State {
    prefs: PluginPrefs,
}

/// `plugin-prefs.toml` behind a lock. Reads answer from memory; a change is applied to the file as it is on disk at
/// that moment and saved before memory changes, so a failed save changes nothing. Without a configuration folder
/// the preferences live in memory only. A damaged or newer file is never overwritten (`sqler-config` refuses).
pub(super) struct Prefs {
    dir: Option<ConfigDir>,
    state: Mutex<State>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn to_pref(v: &SettingValue) -> Option<PrefValue> {
    Some(match v {
        SettingValue::Boolean(b) => PrefValue::Boolean(*b),
        SettingValue::Integer(i) => PrefValue::Integer(*i),
        SettingValue::Float(f) => PrefValue::Float(*f),
        SettingValue::Text(t) => PrefValue::Text(t.clone()),
        _ => return None,
    })
}

fn from_pref(v: &PrefValue) -> SettingValue {
    match v {
        PrefValue::Boolean(b) => SettingValue::Boolean(*b),
        PrefValue::Integer(i) => SettingValue::Integer(*i),
        PrefValue::Float(f) => SettingValue::Float(*f),
        PrefValue::Text(t) => SettingValue::Text(t.clone()),
    }
}

impl Prefs {
    /// Preferences over `dir`. Does no I/O: [`Prefs::reload`] reads the file.
    pub(super) fn new(dir: Option<ConfigDir>) -> Self {
        Self {
            dir,
            state: Mutex::new(State {
                prefs: PluginPrefs::default(),
            }),
        }
    }

    /// Reads the file again. A missing file means defaults; a damaged or newer one means defaults too and is
    /// reported (the returned text), never repaired.
    pub(super) fn reload(&self) -> Option<String> {
        let dir = self.dir.as_ref()?;
        let (prefs, problem) = match dir.load_plugin_prefs() {
            PrefsLoad::Missing => (PluginPrefs::default(), None),
            PrefsLoad::Loaded(p) => (p, None),
            PrefsLoad::Invalid(why) => (
                PluginPrefs::default(),
                Some(format!("plugin-prefs.toml is not used: {why}")),
            ),
            PrefsLoad::Newer(n) => (
                PluginPrefs::default(),
                Some(format!(
                    "plugin-prefs.toml was written by a newer SQLer (format {n}) and is not used"
                )),
            ),
        };
        lock(&self.state).prefs = prefs;
        problem
    }

    /// True when the user switched the plugin off.
    pub(super) fn is_disabled(&self, id: &str) -> bool {
        lock(&self.state).prefs.disabled.contains(id)
    }

    /// Switches a plugin off or on and saves the choice.
    pub(super) fn set_disabled(&self, id: &str, off: bool) -> Result<(), String> {
        self.update(|p| {
            if off {
                p.disabled.insert(id.to_owned());
            } else {
                p.disabled.remove(id);
            }
        })
    }

    /// Applies `change` to the preferences as they are on disk and saves them.
    fn update(&self, change: impl FnOnce(&mut PluginPrefs)) -> Result<(), String> {
        let mut state = lock(&self.state);
        let mut next = match &self.dir {
            Some(dir) => match dir.load_plugin_prefs() {
                PrefsLoad::Missing => PluginPrefs::default(),
                PrefsLoad::Loaded(p) => p,
                PrefsLoad::Invalid(why) => {
                    return Err(format!("plugin-prefs.toml is not used: {why}"));
                }
                PrefsLoad::Newer(n) => {
                    return Err(format!(
                        "plugin-prefs.toml was written by a newer SQLer (format {n}) and is not changed"
                    ));
                }
            },
            None => state.prefs.clone(),
        };
        change(&mut next);
        next.settings.retain(|_, values| !values.is_empty());
        if let Some(dir) = &self.dir {
            dir.save_plugin_prefs(&next).map_err(|e| e.to_string())?;
        }
        state.prefs = next;
        Ok(())
    }
}

impl SettingsStore for Prefs {
    fn get(&self, plugin: &PluginId, key: &SettingKey) -> Option<SettingValue> {
        lock(&self.state)
            .prefs
            .settings
            .get(plugin.as_str())
            .and_then(|values| values.get(&key.to_string()))
            .map(from_pref)
    }

    fn set(
        &self,
        plugin: &PluginId,
        key: &SettingKey,
        value: SettingValue,
    ) -> Result<(), SettingsError> {
        let stored = to_pref(&value).ok_or_else(|| SettingsError::Store {
            message: "this kind of value cannot be kept".to_owned(),
        })?;
        self.update(|p| {
            p.settings
                .entry(plugin.as_str().to_owned())
                .or_default()
                .insert(key.to_string(), stored);
        })
        .map_err(|message| SettingsError::Store { message })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scratch::Scratch;

    fn id(s: &str) -> PluginId {
        s.parse().unwrap()
    }

    fn key(s: &str) -> SettingKey {
        s.parse().unwrap()
    }

    #[test]
    fn disabled_and_settings_survive_a_new_instance() {
        let s = Scratch::new("prefs-roundtrip");
        let dir = ConfigDir::new(s.path());
        let a = Prefs::new(Some(dir.clone()));
        assert!(a.reload().is_none());
        a.set_disabled("demo", true).unwrap();
        a.set(&id("demo"), &key("limit"), SettingValue::Integer(9))
            .unwrap();

        let b = Prefs::new(Some(dir));
        assert!(b.reload().is_none());
        assert!(b.is_disabled("demo"));
        assert!(!b.is_disabled("other"));
        assert_eq!(
            b.get(&id("demo"), &key("limit")),
            Some(SettingValue::Integer(9))
        );
        b.set_disabled("demo", false).unwrap();
        assert!(!b.is_disabled("demo"));
    }

    #[test]
    fn without_a_config_dir_it_is_memory_only() {
        let p = Prefs::new(None);
        assert!(p.reload().is_none());
        p.set_disabled("demo", true).unwrap();
        assert!(p.is_disabled("demo"));
        p.set(&id("demo"), &key("on"), SettingValue::Boolean(true))
            .unwrap();
        assert_eq!(
            p.get(&id("demo"), &key("on")),
            Some(SettingValue::Boolean(true))
        );
    }

    #[test]
    fn a_damaged_file_is_reported_and_left_alone() {
        let s = Scratch::new("prefs-damaged");
        let dir = ConfigDir::new(s.path());
        let path = dir.plugin_prefs_path();
        std::fs::write(&path, b"this = is not [valid").unwrap();
        let p = Prefs::new(Some(dir));
        assert!(p.reload().is_some());
        assert!(p.set_disabled("demo", true).is_err());
        assert!(
            p.set(&id("demo"), &key("limit"), SettingValue::Integer(1))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"this = is not [valid");
        assert!(!p.is_disabled("demo"));
    }

    #[test]
    fn a_newer_file_is_reported_and_left_alone() {
        let s = Scratch::new("prefs-newer");
        let dir = ConfigDir::new(s.path());
        let path = dir.plugin_prefs_path();
        std::fs::write(&path, b"version = 99\n").unwrap();
        let p = Prefs::new(Some(dir));
        assert!(p.reload().unwrap().contains("newer"));
        assert!(p.set_disabled("demo", true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"version = 99\n");
    }

    #[test]
    fn a_failed_save_changes_nothing_in_memory() {
        let s = Scratch::new("prefs-failed");
        let dir = ConfigDir::new(s.path());
        let p = Prefs::new(Some(dir.clone()));
        p.reload();
        // Over the limit for a text value: the store refuses it.
        let big = SettingValue::Text("x".repeat(sqler_config::MAX_PREF_TEXT_BYTES + 1));
        assert!(p.set(&id("demo"), &key("url"), big).is_err());
        assert_eq!(p.get(&id("demo"), &key("url")), None);
    }

    #[test]
    fn a_change_keeps_what_another_instance_saved() {
        let s = Scratch::new("prefs-merge");
        let dir = ConfigDir::new(s.path());
        let a = Prefs::new(Some(dir.clone()));
        let b = Prefs::new(Some(dir));
        a.reload();
        b.reload();
        a.set_disabled("one", true).unwrap();
        b.set_disabled("two", true).unwrap();
        a.reload();
        assert!(a.is_disabled("one") && a.is_disabled("two"));
    }
}
