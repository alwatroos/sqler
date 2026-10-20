// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Per-tab bookkeeping of the intellisense: which loads were asked, which snapshot generation was seen.

use std::collections::HashSet;

use sqler_catalog::CacheKey;

/// What one editor tab remembers between requests.
#[derive(Debug, Default)]
pub struct TabIntel {
    asked: HashSet<CacheKey>,
    bound: Option<u64>,
    seen: Option<u64>,
    revision: u64,
}

impl TabIntel {
    /// Binds the tab to the browser generation of its connection (`None`: no catalog).
    ///
    /// Returns true when the generation changed, to or from `None` too; then the asked loads and the
    /// seen snapshot generation are forgotten, so a reconnect retries failed loads.
    pub fn bind(&mut self, browser_generation: Option<u64>) -> bool {
        if self.bound == browser_generation {
            return false;
        }
        self.bound = browser_generation;
        self.asked.clear();
        self.seen = None;
        true
    }

    /// The keys not asked before, in order; they count as asked from now on.
    ///
    /// A key that failed to load is not returned again until the next [`TabIntel::bind`].
    pub fn fresh(&mut self, keys: Vec<CacheKey>) -> Vec<CacheKey> {
        keys.into_iter()
            .filter(|key| self.asked.insert(key.clone()))
            .collect()
    }

    /// Takes `keys` back out of the asked set, so a later [`TabIntel::fresh`] returns them again.
    ///
    /// For loads that never completed (their task was dropped). A load that completed, with an
    /// error too, stays asked; that is what keeps a failing key from being requested in a loop.
    pub fn unask(&mut self, keys: &[CacheKey]) {
        for key in keys {
            self.asked.remove(key);
        }
    }

    /// Whether `generation` of the catalog snapshot is new; true once per generation.
    pub fn snapshot_changed(&mut self, generation: u64) -> bool {
        if self.seen == Some(generation) {
            return false;
        }
        self.seen = Some(generation);
        true
    }

    /// Notes an edit and returns the new revision.
    pub fn edited(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.revision
    }

    /// The number of edits so far (wrapping).
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use sqler_catalog::NodeKey;
    use sqler_driver_api::sqler_core::{ObjectKind, ObjectPath};

    use super::*;

    fn key(kind: ObjectKind) -> CacheKey {
        CacheKey::Children(NodeKey::new(ObjectPath::root(), kind))
    }

    #[test]
    fn bind_reports_changes_only() {
        let mut t = TabIntel::default();
        assert!(!t.bind(None));
        assert!(t.bind(Some(1)));
        assert!(!t.bind(Some(1)));
        assert!(t.bind(Some(2)));
        assert!(t.bind(None));
        assert!(!t.bind(None));
    }

    #[test]
    fn fresh_drops_asked_keys_until_bind() {
        let mut t = TabIntel::default();
        t.bind(Some(1));
        let a = key(ObjectKind::Table);
        let b = key(ObjectKind::View);
        assert_eq!(
            t.fresh(vec![a.clone(), b.clone(), a.clone()]),
            vec![a.clone(), b]
        );
        assert_eq!(t.fresh(vec![a.clone()]).len(), 0);
        t.bind(Some(2));
        assert_eq!(t.fresh(vec![a.clone()]), vec![a]);
    }

    #[test]
    fn unask_makes_keys_fresh_again() {
        let mut t = TabIntel::default();
        t.bind(Some(1));
        let a = key(ObjectKind::Table);
        let b = key(ObjectKind::View);
        t.fresh(vec![a.clone(), b.clone()]);
        t.unask(std::slice::from_ref(&a));
        assert_eq!(t.fresh(vec![a.clone(), b]), vec![a]);
    }

    #[test]
    fn snapshot_changed_once_per_generation() {
        let mut t = TabIntel::default();
        assert!(t.snapshot_changed(3));
        assert!(!t.snapshot_changed(3));
        assert!(t.snapshot_changed(4));
        t.bind(Some(9));
        assert!(t.snapshot_changed(4));
    }

    #[test]
    fn edits_bump_the_revision() {
        let mut t = TabIntel::default();
        assert_eq!(t.revision(), 0);
        assert_eq!(t.edited(), 1);
        assert_eq!(t.edited(), 2);
        assert_eq!(t.revision(), 2);
    }
}
