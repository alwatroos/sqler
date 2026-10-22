// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! [`NetEgress`]: outgoing HTTP for plugins with `net.http:<pattern>` capabilities (feature `net`).
//!
//! The pieces: [`check_host`] normalises the host and matches it against the grant, [`is_forbidden`] lists the
//! addresses the sandbox never reaches, [`VettedResolver`] vets **every** address a name resolves to and hands the
//! vetted list to the connector (one resolution per request, so DNS rebinding cannot swap the address), and
//! [`NetEgressFactory`] builds the per-plugin [`Egress`](crate::Egress) on one shared reqwest client (rustls on ring,
//! no proxy, no redirects).
//!
//! "Refuses any non-public address" has one exception: the exact name `localhost`, answered `127.0.0.1` / `::1` without
//! a lookup, is reachable when (and only when) a `net.http` pattern names it. Every other name is refused if any of its
//! addresses is forbidden. The runtime additionally bounds concurrent requests and buffered bytes per plugin and per
//! host (see `EGRESS_MAX_*` in the crate root).

mod addr;
mod egress;
mod host;
mod resolver;

pub use addr::is_forbidden;
pub use egress::{NetEgress, NetEgressFactory, NetTimeouts};
pub use host::{Target, check_host};
pub use resolver::{LookupFuture, Resolver, SystemResolver, VettedResolver, vet};
