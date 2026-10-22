// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Grants: who decides what a plugin may do, and the intersection with what it declared.

use sqler_plugin_api::{CapabilitySet, GrantKey};

/// Answers which capabilities the user approved for one exact plugin build (id, version, manifest hash and component
/// digest, see [`GrantKey`]). The grants store (P6) implements it; the host only consumes it.
///
/// This is declared here with P6's exact shape until P6 is on `main`, then it becomes a re-export.
pub trait GrantSource: Send + Sync {
    /// The approved set for `key`, or `None` when nothing was approved (default deny).
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet>;
}

/// A [`GrantSource`] that approves nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAll;

impl GrantSource for DenyAll {
    fn approved(&self, _key: &GrantKey) -> Option<CapabilitySet> {
        None
    }
}

/// What a plugin may actually do: declared ∩ approved. Approving something the manifest does not declare grants
/// nothing; no approval grants nothing.
#[must_use]
pub fn effective(declared: &CapabilitySet, approved: Option<&CapabilitySet>) -> CapabilitySet {
    approved.map_or_else(CapabilitySet::new, |a| declared.intersection(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_plugin_api::Capability;

    fn set(items: &[Capability]) -> CapabilitySet {
        let mut s = CapabilitySet::new();
        for c in items {
            s.insert(c.clone());
        }
        s
    }

    #[test]
    fn intersection_of_declared_and_approved() {
        let declared = set(&[Capability::DbRead, Capability::UiPanel]);
        let approved = set(&[Capability::DbRead, Capability::DbWrite]);
        assert_eq!(
            effective(&declared, Some(&approved)),
            set(&[Capability::DbRead])
        );
    }

    #[test]
    fn nothing_approved_means_nothing_effective() {
        let declared = set(&[Capability::DbRead]);
        assert!(effective(&declared, None).is_empty());
        assert!(effective(&declared, Some(&CapabilitySet::new())).is_empty());
    }

    #[test]
    fn approved_but_undeclared_gives_nothing() {
        let approved = set(&[Capability::DbWrite]);
        assert!(effective(&CapabilitySet::new(), Some(&approved)).is_empty());
    }
}
