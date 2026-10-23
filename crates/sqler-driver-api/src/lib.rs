// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `Driver` trait, streaming result protocol, cancellation and lazy per-node introspection trait that every database driver implements.
//!
//! A [`DriverRegistry`] maps a [`sqler_core::DriverId`] to a boxed [`Driver`]. `connect` yields a
//! boxed [`Session`]. Take a [`CancelHandle`] from the session, then `execute` a query to get a
//! [`QueryStream`] of [`QueryEvent`]s: one `Columns`, zero or more `Rows`, one `Done`, then `None`.
//! [`collect`] gathers a stream into a [`sqler_core::ResultSet`] and enforces that protocol.
//! Everything is dyn-compatible, so callers never need to know the backend.

#![forbid(unsafe_code)]

mod cancel;
mod driver;
mod error;
mod introspect;
mod registry;
mod stream;

pub use cancel::{CancelFlag, CancelHandle, Canceller};
pub use driver::{BoxFuture, Driver, Session};
pub use error::DriverError;
pub use introspect::Introspect;
pub use registry::{DriverRegistry, DriverSource, SourceLookup};
pub use sqler_core;
pub use stream::{QueryEvent, QueryStream, collect};
