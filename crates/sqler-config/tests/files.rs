// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! File round-trip tests for `sqler-config`.

use std::fs;
use std::path::PathBuf;

use sqler_config::{
    CommandId, ConfigDir, ConfigError, KeyChord, KeybindingOverrides, Keymap, Platform, Settings,
    ThemePreference,
};

/// Fresh per-test directory (not created), removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!("sqler-config-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        Self(p)
    }
    fn dir(&self) -> ConfigDir {
        ConfigDir::new(&self.0)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_files_give_defaults() {
    let s = Scratch::new("missing");
    let d = s.dir();
    assert_eq!(d.load_settings().unwrap(), Settings::default());
    assert!(
        d.load_keybinding_overrides(Platform::Other)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        d.load_keymap(Platform::Other).unwrap(),
        Keymap::defaults(Platform::Other)
    );
    assert_eq!(d.load_connections().unwrap().len(), 0);
}

#[test]
fn settings_round_trip_creates_directory() {
    let s = Scratch::new("settings-rt");
    let d = s.dir();
    assert!(!s.0.exists());
    let mut settings = Settings::default();
    settings.theme = ThemePreference::Dark;
    settings.ui_font_size = 18;
    d.save_settings(&settings).unwrap();
    assert!(d.settings_path().is_file());
    assert!(
        !d.settings_path()
            .with_file_name("settings.toml.tmp")
            .exists()
    );
    assert_eq!(d.load_settings().unwrap(), settings);
}

#[test]
fn save_rejects_out_of_range_font() {
    let s = Scratch::new("settings-range");
    let mut settings = Settings::default();
    settings.ui_font_size = 7;
    let e = s.dir().save_settings(&settings).unwrap_err();
    assert!(matches!(e, ConfigError::Invalid { .. }));
    assert!(!s.0.exists());
}

#[test]
fn overrides_round_trip() {
    let s = Scratch::new("keys-rt");
    let d = s.dir();
    let p = Platform::MacOs;
    let mut o = KeybindingOverrides::new();
    o.insert(
        CommandId::new("tab:close").unwrap(),
        vec![
            KeyChord::parse("mod+k", p).unwrap(),
            KeyChord::parse("f4", p).unwrap(),
        ],
    );
    o.insert(CommandId::new("app:quit").unwrap(), vec![]);
    d.save_keybinding_overrides(&o).unwrap();
    assert_eq!(d.load_keybinding_overrides(p).unwrap(), o);
    let km = d.load_keymap(p).unwrap();
    assert_eq!(km.chords_for(&CommandId::new("app:quit").unwrap()).len(), 0);
}

#[test]
fn bad_keybindings_name_the_command() {
    let s = Scratch::new("keys-bad");
    let d = s.dir();
    fs::create_dir_all(&s.0).unwrap();
    fs::write(
        d.keybindings_path(),
        "[bindings]\n\"tab:close\" = [\"ctrl+ctrl+w\"]\n",
    )
    .unwrap();
    let e = d
        .load_keybinding_overrides(Platform::Other)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("keybindings.toml") && e.contains("tab:close"),
        "{e}"
    );
    fs::write(d.keybindings_path(), "[bindings]\n\"Tab\" = []\n").unwrap();
    assert!(matches!(
        d.load_keybinding_overrides(Platform::Other),
        Err(ConfigError::Invalid { .. })
    ));
}

#[test]
fn malformed_files_give_parse_with_path() {
    let s = Scratch::new("malformed");
    let d = s.dir();
    fs::create_dir_all(&s.0).unwrap();
    for (path, name) in [
        (d.settings_path(), "settings.toml"),
        (d.keybindings_path(), "keybindings.toml"),
        (d.connections_path(), "connections.toml"),
    ] {
        fs::write(&path, "this is = = not toml").unwrap();
        let e = match name {
            "settings.toml" => d.load_settings().unwrap_err(),
            "keybindings.toml" => d.load_keybinding_overrides(Platform::Other).unwrap_err(),
            _ => d.load_connections().unwrap_err(),
        };
        assert!(matches!(e, ConfigError::Parse { .. }), "{name}: {e:?}");
        assert!(e.to_string().contains(name), "{e}");
    }
}

#[test]
fn connections_load_from_file() {
    let s = Scratch::new("conns");
    let d = s.dir();
    fs::create_dir_all(&s.0).unwrap();
    fs::write(
        d.connections_path(),
        "[[connection]]\nname = \"local\"\ndriver = \"sqlite\"\ntarget = \"app.db\"\nenvironment = \"prod\"\n",
    )
    .unwrap();
    let v = d.load_connections().unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].environment, Some(sqler_core::Environment::Prod));
}
