// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Loads `keybindings.toml` from a config directory and resolves chords through the dispatcher.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use sqler_config::{ConfigDir, KeyChord, Platform};
use sqler_ui::command::{CommandId, CommandRegistry};
use sqler_ui::keymap::KeyDispatcher;
use sqler_ui::{palette, workspace};

/// Fresh per-test directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!("sqler-ui-keymap-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    workspace::register_commands(&mut r).unwrap();
    palette::register_commands(&mut r).unwrap();
    r
}

fn chord(s: &str) -> KeyChord {
    KeyChord::parse(s, Platform::Other).unwrap()
}

fn dispatcher(dir: &ConfigDir) -> KeyDispatcher {
    let loaded = dir
        .load_keybinding_overrides(Platform::Other)
        .map_err(|e| e.to_string());
    KeyDispatcher::new(Platform::Other, loaded, &registry())
}

#[test]
fn user_override_changes_the_trigger() {
    let s = Scratch::new("override");
    fs::write(
        s.0.join("keybindings.toml"),
        "[bindings]\n\"tab:close\" = [\"mod+k\"]\n",
    )
    .unwrap();
    let keys = dispatcher(&ConfigDir::new(&s.0));
    assert_eq!(
        keys.resolve(&chord("ctrl+k")),
        Some(&CommandId::new("tab:close").unwrap())
    );
    assert_eq!(keys.resolve(&chord("ctrl+w")), None);
    assert_eq!(keys.notices().len(), 0);
}

#[test]
fn malformed_file_gives_defaults_and_one_notice() {
    let s = Scratch::new("malformed");
    fs::write(s.0.join("keybindings.toml"), "this is = not [valid").unwrap();
    let keys = dispatcher(&ConfigDir::new(&s.0));
    assert_eq!(
        keys.resolve(&chord("ctrl+w")),
        Some(&CommandId::new("tab:close").unwrap())
    );
    assert_eq!(keys.notices().len(), 1);
}

#[test]
fn missing_file_gives_defaults() {
    let s = Scratch::new("missing");
    let keys = dispatcher(&ConfigDir::new(&s.0));
    assert_eq!(
        keys.resolve(&chord("ctrl+shift+p")),
        Some(&CommandId::new(palette::OPEN_PALETTE).unwrap())
    );
    assert_eq!(keys.notices().len(), 0);
}
