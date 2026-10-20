// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! DDL and script generation for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite`.
//!
//! Every function is pure: no I/O, no async. Tables are rendered from the `sqler-core` object
//! model; other kinds use the server's definition text, which the caller fetches and passes in.
//!
//! Identifiers are always quoted and the dialect's quote character is doubled, so no keyword
//! list is needed. Empty names and names with NUL are rejected with a [`DdlError`]. Definition
//! text, column defaults and native types come from the server's catalog and are written as
//! they are.
//!
//! Dialects: [`Dialect::Postgres`], [`Dialect::MySql`] (also `MariaDB`) and [`Dialect::Sqlite`].

#![forbid(unsafe_code)]

mod ddl;
mod dialect;
mod error;

pub use ddl::{
    DEFAULT_ROW_LIMIT, Script, create_script, create_table, drop_script, insert_template,
    select_star, supports,
};
pub use dialect::{Dialect, qualified_name, quote_identifier};
pub use error::DdlError;
