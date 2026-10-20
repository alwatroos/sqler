// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Domain types for `SQLer`: connection, object model, query, result set, value and data type. Pure: no I/O, no async, no UI.
//!
//! All growable types are `#[non_exhaustive]`; use the provided constructors.

#![forbid(unsafe_code)]

mod catalog;
mod connection;
mod error;
mod query;
mod result;
mod value;

pub use catalog::{
    ColumnInfo, DbObject, ForeignKey, Listing, ObjectKind, ObjectLevel, ObjectPath, PrimaryKey,
    ReferentialAction, Table,
};
pub use connection::{ConnectionConfig, DriverId, Environment, TlsConfig, TlsMode};
pub use error::CoreError;
pub use query::Query;
pub use result::{Column, ResultSet, Row};
pub use value::{DataType, Date, Decimal, Time, Timestamp, TimestampTz, Value};
