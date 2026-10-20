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
//!
//! The crate also has pure SQL text services built on one hand-written lexer that never fails:
//! [`split()`] a script into statements with exact byte ranges, find the [`statement_at`] a
//! cursor offset, find parameter [`placeholders`], [`classify()`] statements as read, write,
//! DDL or unknown (anything not proven read-only is not read) and [`format()`] SQL.

#![forbid(unsafe_code)]

mod classify;
mod ddl;
mod dialect;
mod error;
mod format;
mod lexer;
mod placeholder;
mod split;

pub use classify::{StatementClass, classify, classify_each};
pub use ddl::{
    DEFAULT_ROW_LIMIT, Script, create_script, create_table, drop_script, insert_template,
    select_star, supports,
};
pub use dialect::{Dialect, qualified_name, quote_identifier};
pub use error::{DdlError, FormatError};
pub use format::{FormatOptions, format};
pub use placeholder::{Placeholder, PlaceholderKind, placeholders};
pub use split::{Segment, SegmentKind, split, statement_at};
