// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` application: workspace, tabs, panels, grid, editor, command palette and themes.
//!
//! State (`sidebar`, `tabs`, `workspace`, `keymap`, `palette`) is plain Rust and unit-tested headless. The `GPUI`
//! views live behind the `gui` cargo feature, which only `sqler-app` turns on.

#![forbid(unsafe_code)]

pub mod command;
pub mod grid;
pub mod keymap;
pub mod palette;
pub mod settings;
pub mod sidebar;
pub mod tabs;
pub mod theme;
pub mod workspace;
