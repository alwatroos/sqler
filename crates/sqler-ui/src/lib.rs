// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` application: workspace, tabs, panels, grid, editor, command palette and themes.
//!
//! State (`browser`, `connections`, `editor`, `sidebar`, `tabs`, `workbench`, `workspace`, `keymap`, `palette`, `query`, `history`, `export`) is plain Rust and unit-tested
//! headless. The `GPUI` views live behind the `gui` cargo feature, which only `sqler-app` turns on.

#![forbid(unsafe_code)]

pub mod ai;
pub mod browser;
pub mod command;
pub mod connections;
pub mod editor;
pub mod export;
pub mod grid;
pub mod history;
pub mod keymap;
pub mod palette;
pub mod plugins;
pub mod query;
pub mod settings;
pub mod sidebar;
pub mod tabs;
pub mod theme;
pub mod workbench;
pub mod workspace;
