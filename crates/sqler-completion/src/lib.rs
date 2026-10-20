// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Intellisense engine: scope analysis, alias resolution and ranking. Pure functions, heavily tested, no async, no I/O, no UI.
//!
//! [`complete`] takes SQL text, a cursor (UTF-8 byte offset), a dialect and a [`SchemaSource`]
//! and returns ranked candidates: schemas, tables and views, columns in scope, aliases, CTEs,
//! built-in functions, keywords and snippets. It works on incomplete SQL: a hand-written lexer
//! never fails, and a clause tracker decides the context. Names the schema source has not loaded
//! come back as [`SchemaRequest`]s, never as errors.
//!
//! ```
//! use sqler_completion::{CompletionRequest, ItemKind, MemorySchema, complete};
//! use sqler_core::{ColumnInfo, DataType, ObjectKind};
//! use sqler_sql::Dialect;
//!
//! let mut schema = MemorySchema::new(["public"]);
//! schema.add_table(
//!     "public",
//!     "users",
//!     ObjectKind::Table,
//!     Some(vec![
//!         ColumnInfo::new("id", DataType::Integer),
//!         ColumnInfo::new("name", DataType::Text),
//!     ]),
//! );
//!
//! let sql = "SELECT u.na FROM users u";
//! let req = CompletionRequest::new(sql, "SELECT u.na".len(), Dialect::Postgres);
//! let done = complete(&req, &schema);
//! assert_eq!(done.items[0].label, "name");
//! assert_eq!(done.items[0].kind, ItemKind::Column);
//! assert_eq!(&sql[done.replace.clone()], "na");
//! ```
//!
//! [`analyze`] runs the same analysis without a schema and returns the statement, the context,
//! the qualifier and the [`Scope`]; hover and diagnostics build on it.

#![forbid(unsafe_code)]

mod context;
mod dialect_data;
mod engine;
mod lexer;
mod memory;
mod rank;
mod scope;
mod source;

pub use context::{Analysis, ContextKind, analyze};
pub use engine::{
    CompletionItem, CompletionRequest, Completions, InsertFormat, ItemKind, SchemaRequest, complete,
};
pub use memory::MemorySchema;
pub use scope::{Cte, Ident, RelationSource, Scope, ScopeRelation};
pub use source::{Loadable, Relation, SchemaSource};
