// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Keyboard shortcuts: key press to chord, chord to command, command to workspace action.
//!
//! Plain Rust, unit-tested headless. The default keymap and the user's `keybindings.toml` come
//! from `sqler-config`; this module resolves chords against the registered commands. The `GPUI`
//! key conversion lives behind the `gui` feature.

mod chord;
mod dispatcher;
#[cfg(feature = "gui")]
pub(crate) mod input;
mod route;

pub use chord::{KeyInput, chord_from_input};
pub use dispatcher::KeyDispatcher;
pub use route::{KeyOutcome, action_for, pane_action_for, route_key};
