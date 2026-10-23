// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin drivers (ADR-0061): a [`DriverSource`](sqler_driver_api::DriverSource) that offers the drivers of plugins to
//! the registry of the application.
//!
//! A plugin declares `[[drivers]]` in its manifest. Two kinds exist:
//!
//! - `kind = "guest"` (needs `driver.guest`): the plugin implements the WIT `driver` interface, and optionally
//!   `driver-catalog`. The adapter in [`guest`] maps it onto `Session`, `QueryStream` and `Introspect`. The guest
//!   receives the host, user name and password of each connection that uses its driver, and nothing else.
//! - `kind = "host"` (needs `driver.host:<engine>`): a declarative shell around a native engine the binary links
//!   ([`HostEngines`]). The host owns connect, TLS and sockets, and the plugin's guest, if it has one, is not asked.
//!
//! A plugin driver is only reachable as a registry `Session`, so the run flow and the production gate of the
//! application apply unchanged; nothing in this module writes to a database on its own account.

mod convert;
mod error;
mod guest;
mod host_engine;
mod params;
mod source;

use crate::runtime::wit_exports::{driver as wit_driver, driver_catalog as wit_catalog};

pub use source::{HostEngines, PluginDrivers};
