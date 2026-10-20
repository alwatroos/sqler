// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The object browser: a lazy tree per connection, its menus and commands, the DROP
//! confirmation flow, the content of the DDL and properties tabs, and the async jobs behind them.
//!
//! Everything here is plain Rust and headless. [`ObjectTree`] never calls the catalog: it returns
//! the [`sqler_catalog::CacheKey`]s to load and the workspace resolves them. The jobs
//! ([`generate_script`], [`load_properties`], [`open_data`], [`run_drop`]) are futures that the
//! workspace runs on tokio through `SessionBridge`; those that need SQL execution open a
//! short-lived session through the [`Connector`] and close it on every path.
//!
//! Script text comes only from `sqler-sql`, and the catalog only through `sqler-catalog`.

mod action;
mod connect;
mod drop;
mod error;
mod jobs;
mod menu;
mod tab;
#[cfg(test)]
pub(crate) mod testing;
mod tree;

pub use action::{BrowserAction, COMMANDS, action_for, register_commands};
pub use connect::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};
pub use drop::{DropDialog, DropFlow, DropStep, PROD_GUARD};
pub use error::BrowserError;
pub use jobs::{generate_script, load_properties, open_data, run_drop};
pub use menu::{MenuEntry, menu_for};
pub use tab::{ObjectTab, ObjectTabContent, Property, column_properties, properties};
pub use tree::{LoadState, ObjectTree, TreeNodeId, TreeNodeKind, TreeRow};
