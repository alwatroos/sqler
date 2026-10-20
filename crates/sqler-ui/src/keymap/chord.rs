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
}
