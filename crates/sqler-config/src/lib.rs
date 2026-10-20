// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Settings, keybindings and the saved-connection list, stored as TOML files in
//! the platform configuration directory.
//!
//! Keychain access arrives in M2; no secret is ever stored in these files.

#![forbid(unsafe_code)]

mod connections;
mod error;
mod fs;
mod keymap;
mod paths;
mod settings;

pub use error::ConfigError;
pub use keymap::{CommandId, Conflict, KeyChord, KeybindingOverrides, Keymap, Modifiers, Platform};
pub use paths::ConfigDir;
pub use settings::{FONT_SIZE_RANGE, Settings, ThemePreference};
