// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

// The generated host traits are async; these implementations just have nothing to await yet.
#![allow(clippy::unused_async_trait_impl)]

//! Host interfaces that are linked but not yet bound to the app: they answer by the gate of ADR-0055. A missing
//! capability gives `capability-denied`; a granted one gives `failed` ("not available in this host"). The item that
//! implements an interface deletes its file here and its `mod` line.

use sqler_plugin_api::{CapabilityKind, CapabilitySet, Gate, HostInterface, gate};

use super::bindings::sqler::plugin::types::PluginError;
use super::convert::plugin_error;
use crate::error::ErrorCode;

use crate::query::NOT_AVAILABLE;

/// The error for a call into `interface`: the API gate decides first (a missing capability is `capability-denied`);
/// a call that passes it is `failed`. An unknown gate kind fails closed. No interface is gated this way since `query`
/// and `catalog` became real (P5), so only the tests call it today.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn gated(effective: &CapabilitySet, interface: HostInterface) -> PluginError {
    let allowed = match gate(interface) {
        Gate::None => true,
        Gate::Requires(kind) => effective.allows(kind),
        // Which class a statement has is the query item's business; here either capability gets past the gate.
        Gate::Query => {
            effective.allows(CapabilityKind::DbRead) || effective.allows(CapabilityKind::DbWrite)
        }
        _ => false,
    };
    if allowed {
        plugin_error(ErrorCode::Failed, NOT_AVAILABLE)
    } else {
        plugin_error(
            ErrorCode::CapabilityDenied,
            format!("`{interface:?}` is not granted to this plugin"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_plugin_api::Capability;

    #[test]
    fn missing_capability_is_denied_and_granted_is_unavailable() {
        let none = CapabilitySet::new();
        let e = gated(&none, HostInterface::Catalog);
        assert_eq!(ErrorCode::from(e.code), ErrorCode::CapabilityDenied);
        let mut some = CapabilitySet::new();
        some.insert(Capability::DbRead);
        let e = gated(&some, HostInterface::Catalog);
        assert_eq!(ErrorCode::from(e.code), ErrorCode::Failed);
        assert_eq!(e.message, NOT_AVAILABLE);
    }

    #[test]
    fn either_kind_satisfies_a_query_gate() {
        let mut w = CapabilitySet::new();
        w.insert(Capability::DbWrite);
        let e = gated(&w, HostInterface::Query);
        assert_eq!(ErrorCode::from(e.code), ErrorCode::Failed);
    }
}
