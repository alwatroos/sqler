// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A framework-neutral key press and its conversion to a `KeyChord`.

use sqler_config::{KeyChord, Platform};

/// A key press as the UI layer reports it, with no framework types.
#[allow(clippy::struct_excessive_bools)] // one flag per modifier key
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyInput {
    /// Control is held.
    pub ctrl: bool,
    /// Alt (Option) is held.
    pub alt: bool,
    /// Shift is held.
    pub shift: bool,
    /// Cmd (macOS), Windows or Super is held.
    pub cmd: bool,
    /// The key name: `a`, `,`, `enter`, `f5`, ...
    pub key: String,
}

/// Builds the chord for a key press, or `None` when the key is not in the chord grammar
/// (empty, a bare modifier, an unknown name).
#[must_use]
pub fn chord_from_input(input: &KeyInput) -> Option<KeyChord> {
    let mut text = String::new();
    for (on, name) in [
        (input.ctrl, "ctrl"),
        (input.alt, "alt"),
        (input.shift, "shift"),
        (input.cmd, "cmd"),
    ] {
        if on {
            text.push_str(name);
            text.push('+');
        }
    }
    text.push_str(&input.key);
    // Modifier names are not keys: `KeyChord::parse` rejects them as the last token.
    KeyChord::parse(&text, Platform::Other).ok()
}

/// The chord as a user-facing label: glyphs on macOS (`⌘⇧P`), words elsewhere (`Ctrl+Shift+P`).
///
/// macOS orders modifiers ⌘ ⌃ ⌥ ⇧ before the key; other platforms use `Ctrl`, `Alt`, `Shift`,
/// `Super`, joined with `+`.
#[must_use]
pub fn display_chord(chord: &KeyChord, p: Platform) -> String {
    let m = chord.modifiers();
    let mac = p == Platform::MacOs;
    let key = display_key(chord.key(), mac);
    if mac {
        let mut out = String::new();
        for (on, glyph) in [(m.cmd, '⌘'), (m.ctrl, '⌃'), (m.alt, '⌥'), (m.shift, '⇧')] {
            if on {
                out.push(glyph);
            }
        }
        out.push_str(&key);
        out
    } else {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.cmd, "Super"),
        ] {
            if on {
                parts.push(name);
            }
        }
        parts.push(&key);
        parts.join("+")
    }
}

fn display_key(key: &str, mac: bool) -> String {
    let named = match (key, mac) {
        ("enter", true) => "↩",
        ("enter", false) => "Enter",
        ("escape", true) => "⎋",
        ("escape", false) => "Esc",
        ("tab", true) => "⇥",
        ("tab", false) => "Tab",
        ("space", _) => "Space",
        ("backspace", true) => "⌫",
        ("backspace", false) => "Backspace",
        ("delete", true) => "⌦",
        ("delete", false) => "Delete",
        ("insert", _) => "Insert",
        ("home", true) => "↖",
        ("home", false) => "Home",
        ("end", true) => "↘",
        ("end", false) => "End",
        ("pageup", true) => "⇞",
        ("pageup", false) => "PageUp",
        ("pagedown", true) => "⇟",
        ("pagedown", false) => "PageDown",
        ("up", true) => "↑",
        ("up", false) => "Up",
        ("down", true) => "↓",
        ("down", false) => "Down",
        ("left", true) => "←",
        ("left", false) => "Left",
        ("right", true) => "→",
        ("right", false) => "Right",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    // Letters upper case; digits, punctuation and `F1..F24` as is.
    key.to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `mods` holds `c`trl, `a`lt, `s`hift and `m` (cmd) for the held modifiers.
    fn input(mods: &str, key: &str) -> KeyInput {
        KeyInput {
            ctrl: mods.contains('c'),
            alt: mods.contains('a'),
            shift: mods.contains('s'),
            cmd: mods.contains('m'),
            key: key.into(),
        }
    }

    fn text(i: &KeyInput) -> Option<String> {
        chord_from_input(i).map(|c| c.to_string())
    }

    #[test]
    fn modifier_order_is_canonical() {
        let i = input("casm", "k");
        assert_eq!(text(&i).unwrap(), "ctrl+alt+shift+cmd+k");
        let i = input("sm", "p");
        assert_eq!(text(&i).unwrap(), "shift+cmd+p");
    }

    #[test]
    fn plain_named_and_function_keys() {
        for key in [
            "a", "5", ",", "`", "enter", "escape", "pageup", "f1", "f24", "tab",
        ] {
            assert_eq!(text(&input("", key)).unwrap(), key);
        }
        let c = chord_from_input(&input("c", "w")).unwrap();
        assert_eq!(c, KeyChord::parse("ctrl+w", Platform::Other).unwrap());
    }

    #[test]
    fn rejected_keys() {
        for key in [
            "", "ctrl", "shift", "cmd", "alt", "+", "f25", "ab", "é", "unknown",
        ] {
            assert_eq!(chord_from_input(&input("", key)), None, "{key}");
        }
        assert_eq!(chord_from_input(&input("c", "shift")), None);
    }

    fn shown(chord: &str, p: Platform) -> String {
        display_chord(&KeyChord::parse(chord, p).unwrap(), p)
    }

    #[test]
    fn display_examples() {
        let cases = [
            ("mod+shift+p", "⌘⇧P", "Ctrl+Shift+P"),
            ("mod+enter", "⌘↩", "Ctrl+Enter"),
            ("ctrl+tab", "⌃⇥", "Ctrl+Tab"),
            ("shift+f6", "⇧F6", "Shift+F6"),
            ("mod+\\", "⌘\\", "Ctrl+\\"),
            ("mod+shift+pagedown", "⌘⇧⇟", "Ctrl+Shift+PageDown"),
            ("escape", "⎋", "Esc"),
            ("ctrl+alt+shift+cmd+a", "⌘⌃⌥⇧A", "Ctrl+Alt+Shift+Super+A"),
        ];
        for (c, mac, other) in cases {
            assert_eq!(shown(c, Platform::MacOs), mac, "{c}");
            assert_eq!(shown(c, Platform::Other), other, "{c}");
        }
        assert_eq!(shown("cmd+k", Platform::Other), "Super+K");
        assert_eq!(shown("mod+1", Platform::MacOs), "⌘1");
    }

    #[test]
    fn every_key_has_a_label() {
        let mut keys: Vec<String> = [
            "enter",
            "escape",
            "tab",
            "space",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
        ]
        .map(String::from)
        .to_vec();
        keys.extend((1..=24).map(|n| format!("f{n}")));
        for k in keys {
            for p in [Platform::MacOs, Platform::Other] {
                let label = shown(&k, p);
                assert!(!label.is_empty(), "{k}");
                if k.starts_with('f') && k.len() > 1 {
                    assert_eq!(label, k.to_ascii_uppercase());
                }
            }
        }
    }
}
