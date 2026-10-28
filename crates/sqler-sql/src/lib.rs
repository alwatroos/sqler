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
//! Dialects: [`Dialect::Postgres`], [`Dialect::MySql`] (also `MariaDB`), [`Dialect::Sqlite`] and
//! [`Dialect::TSql`] (Microsoft SQL Server). The first three are built-in driver ids
//! ([`Dialect::from_driver_id`]); a plugin driver names its dialect in its manifest and the host
//! maps the name with [`Dialect::from_name`]. T-SQL has no create or drop scripts.
//!
//! The crate also has pure SQL text services built on one hand-written lexer that never fails:
//! [`split()`] a script into statements with exact byte ranges, find the [`statement_at`] a
//! cursor offset, find parameter [`placeholders`], [`classify()`] statements as read, write,
//! DDL or unknown (anything not proven read-only is not read) and [`format()`] SQL.
//!
//! [`sql_literal`] renders a `sqler-core` value as a SQL literal that cannot end its string, and
//! the lexer is public ([`tokenize`], [`Token`], [`TokenKind`]) so callers can re-lex generated SQL.
//! [`update()`], [`insert()`] and [`delete()`] generate bound DML for staged grid edits: every
//! value is a parameter, never text in the SQL; [`single_table_source`] proves a `SELECT` reads one table.

#![forbid(unsafe_code)]

mod classify;
mod ddl;
mod dialect;
mod dml;
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
pub use dialect::{
    Dialect, push_literal, qualified_name, quote_identifier, sql_literal, string_literal,
};
pub use dml::{
    BoundDml, ColumnValue, DmlError, SelectList, SourceTable, SqlIdent, TxStep, delete,
    delete_bound, insert, insert_bound, param_marker, same_column, single_table_source,
    transaction_sql, unwritable_reason, update, update_bound,
};
pub use error::{DdlError, FormatError};
pub use format::{FormatOptions, format};
pub use lexer::{Token, TokenKind, tokenize};
pub use placeholder::{Placeholder, PlaceholderKind, placeholders};
pub use split::{Segment, SegmentKind, split, statement_at};
