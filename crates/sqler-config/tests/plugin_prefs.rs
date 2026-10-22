// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! File, permission and corruption tests for the plugin preferences store.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use sqler_config::{
    ConfigDir, ConfigError, MAX_PREF_PLUGINS, MAX_PREF_TEXT_BYTES, PluginPrefs, PrefValue,
    PrefsLoad,
};

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p =
            std::env::temp_dir().join(format!("sqler-config-{}-prefs-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
    fn dir(&self) -> ConfigDir {
        ConfigDir::new(&self.0)
    }
    /// Writes raw bytes as the prefs file, owner-only on Unix.
    fn put(&self, text: &str) {
        fs::create_dir_all(&self.0).unwrap();
        let path = self.dir().plugin_prefs_path();
        fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sample() -> PluginPrefs {
    let mut p = PluginPrefs::default();
    p.disabled.insert("zed".into());
    p.disabled.insert("alpha".into());
    let s = p.settings.entry("alpha".into()).or_default();
    s.insert("on".into(), PrefValue::Boolean(true));
    s.insert("limit".into(), PrefValue::Integer(-3));
    s.insert("ratio".into(), PrefValue::Float(0.5));
    s.insert("label".into(), PrefValue::Text("h\u{e9}llo \"q\"\n".into()));
    p
}

#[test]
fn missing_file_is_missing() {
    let s = Scratch::new("missing");
    assert_eq!(s.dir().load_plugin_prefs(), PrefsLoad::Missing);
}

#[test]
fn round_trip_is_sorted_and_versioned() {
    let s = Scratch::new("round-trip");
    let d = s.dir();
    d.save_plugin_prefs(&sample()).unwrap();
    assert_eq!(d.load_plugin_prefs(), PrefsLoad::Loaded(sample()));
    let text = fs::read_to_string(d.plugin_prefs_path()).unwrap();
    assert!(text.starts_with("version = 1\n"));
    assert!(text.find("alpha").unwrap() < text.find("zed").unwrap());
}

#[cfg(unix)]
#[test]
fn the_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("private");
    let d = s.dir();
    d.save_plugin_prefs(&sample()).unwrap();
    let mode = fs::metadata(d.plugin_prefs_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
}

#[test]
fn a_damaged_file_is_reported_and_left_untouched() {
    let s = Scratch::new("damaged");
    let d = s.dir();
    for bad in [
        "not toml [[",
        "",
        "version = 0\n",
        "version = \"1\"\n",
        "version = 1\nextra = 1\n",
        "version = 1\ndisabled = [1]\n",
        "version = 1\n[settings.a]\nk = [1]\n",
        "version = 1\n[settings.a]\nk = nan\n",
        "version = 1\n[settings.a]\n\"\" = 1\n",
    ] {
        s.put(bad);
        assert!(
            matches!(d.load_plugin_prefs(), PrefsLoad::Invalid(_)),
            "{bad:?}"
        );
        assert!(matches!(
            d.save_plugin_prefs(&sample()),
            Err(ConfigError::Invalid { .. })
        ));
        assert_eq!(fs::read_to_string(d.plugin_prefs_path()).unwrap(), bad);
    }
}

#[test]
fn a_newer_file_is_reported_and_left_untouched() {
    let s = Scratch::new("newer");
    let d = s.dir();
    let text = "version = 2\nfuture = true\n";
    s.put(text);
    assert_eq!(d.load_plugin_prefs(), PrefsLoad::Newer(2));
    assert!(d.save_plugin_prefs(&sample()).is_err());
    assert_eq!(fs::read_to_string(d.plugin_prefs_path()).unwrap(), text);
}

#[cfg(unix)]
#[test]
fn an_other_writable_file_is_invalid() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("other-writable");
    let d = s.dir();
    s.put("version = 1\n");
    fs::set_permissions(d.plugin_prefs_path(), fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(d.load_plugin_prefs(), PrefsLoad::Invalid(m) if m.contains("writable")));
    assert!(d.save_plugin_prefs(&PluginPrefs::default()).is_err());
}

#[test]
fn limits_are_enforced_on_save() {
    let s = Scratch::new("limits");
    let d = s.dir();
    let mut p = PluginPrefs::default();
    p.settings
        .entry("a".into())
        .or_default()
        .insert("f".into(), PrefValue::Float(f64::NAN));
    assert!(d.save_plugin_prefs(&p).is_err());
    let mut p = PluginPrefs::default();
    p.settings.entry("a".into()).or_default().insert(
        "t".into(),
        PrefValue::Text("x".repeat(MAX_PREF_TEXT_BYTES + 1)),
    );
    assert!(d.save_plugin_prefs(&p).is_err());
    let mut p = PluginPrefs::default();
    for i in 0..=MAX_PREF_PLUGINS {
        p.disabled.insert(format!("p-{i}"));
    }
    assert!(d.save_plugin_prefs(&p).is_err());
    assert_eq!(d.load_plugin_prefs(), PrefsLoad::Missing);
}

#[test]
fn errors_never_quote_the_source() {
    let s = Scratch::new("quote");
    s.put("version = 1\nsecret-token = = x\n");
    match s.dir().load_plugin_prefs() {
        PrefsLoad::Invalid(m) => assert!(!m.contains("secret-token"), "{m}"),
        other => panic!("{other:?}"),
    }
}
