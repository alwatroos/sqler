// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Command registry and palette query.
//!
//! Every user action is a command with a stable ID, a human title and a category. The ID follows
//! the `category:action` rule (from `AGENTS.md` section 4), for example `query:run-selection` or `app:quit`.
//! The category is the ID prefix, so it cannot drift from the ID. This module is pure logic: no
//! UI framework, no async, no I/O.

mod error;
mod id;
mod matcher;
mod registry;

pub use error::CommandError;
pub use id::CommandId;
pub use matcher::{MatchField, PaletteMatch};
pub use registry::{Command, CommandRegistry};
