// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `gpui::Keystroke` to the framework-neutral `KeyInput`.

use super::KeyInput;

/// Converts a `GPUI` keystroke. Uses `key` (the unshifted key name), never `key_char`. `GPUI`
/// names the keys like the chord grammar (`enter`, `escape`, `pageup`, `f1`, punctuation as the
/// character); the function modifier is ignored. Unknown names fail later in `chord_from_input`.
pub(crate) fn key_input(k: &gpui::Keystroke) -> KeyInput {
    KeyInput {
        ctrl: k.modifiers.control,
        alt: k.modifiers.alt,
        shift: k.modifiers.shift,
        cmd: k.modifiers.platform,
        key: k.key.to_ascii_lowercase(),
    }
}
