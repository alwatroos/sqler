// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Per-tab headless editor state, owned by the workspace.

use std::collections::HashMap;

use sqler_sql::Dialect;

use crate::tabs::TabId;

use super::EditorBuffer;
use super::occurrence::{self, OccurrenceChain, OccurrenceStep};

/// The headless state of one editor tab.
#[derive(Debug, Clone)]
pub struct EditorDoc {
    dialect: Option<Dialect>,
    seed: Option<String>,
    chain: Option<OccurrenceChain>,
}

impl EditorDoc {
    /// A doc for a connection of `dialect` (`None` if the driver has no SQL dialect), with optional start text.
    #[must_use]
    pub fn new(dialect: Option<Dialect>, seed: Option<String>) -> Self {
        Self {
            dialect,
            seed,
            chain: None,
        }
    }

    /// The SQL dialect of the tab's connection.
    #[must_use]
    pub fn dialect(&self) -> Option<Dialect> {
        self.dialect
    }

    /// The start text that has not been handed to the view yet.
    #[must_use]
    pub fn seed(&self) -> Option<&str> {
        self.seed.as_deref()
    }

    /// Hands the start text over (once).
    pub fn take_seed(&mut self) -> Option<String> {
        self.seed.take()
    }

    /// The running "add next occurrence" chain.
    #[must_use]
    pub fn chain(&self) -> Option<&OccurrenceChain> {
        self.chain.as_ref()
    }

    /// One "add next occurrence" step for `buffer`.
    pub fn add_next_occurrence(&mut self, buffer: &EditorBuffer) -> OccurrenceStep {
        occurrence::step(&mut self.chain, &buffer.text, &buffer.selection)
    }
}

/// The docs of all editor tabs.
#[derive(Debug, Default)]
pub struct EditorDocs {
    docs: HashMap<TabId, EditorDoc>,
}

impl EditorDocs {
    /// No docs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an editor tab.
    pub fn open(&mut self, tab: TabId, dialect: Option<Dialect>, seed: Option<String>) {
        self.docs.insert(tab, EditorDoc::new(dialect, seed));
    }

    /// The doc of `tab`, if it is an editor tab.
    #[must_use]
    pub fn get(&self, tab: TabId) -> Option<&EditorDoc> {
        self.docs.get(&tab)
    }

    /// The doc of `tab`, mutable.
    pub fn get_mut(&mut self, tab: TabId) -> Option<&mut EditorDoc> {
        self.docs.get_mut(&tab)
    }

    /// Forgets `tab`; returns its doc.
    pub fn remove(&mut self, tab: TabId) -> Option<EditorDoc> {
        self.docs.remove(&tab)
    }

    /// Whether `tab` is an editor tab.
    #[must_use]
    pub fn contains(&self, tab: TabId) -> bool {
        self.docs.contains_key(&tab)
    }

    /// Takes the start text of `tab` (once).
    pub fn take_seed(&mut self, tab: TabId) -> Option<String> {
        self.docs.get_mut(&tab)?.take_seed()
    }

    /// How many editor tabs are open.
    #[must_use]
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether no editor tab is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::{TabConnection, TabKind, TabStrip};
    use sqler_driver_api::sqler_core::DriverId;

    fn tab_id() -> TabId {
        let mut strip = TabStrip::default();
        let conn = TabConnection {
            name: "c".to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        strip.open(conn, TabKind::Query)
    }

    #[test]
    fn open_get_remove() {
        let tab = tab_id();
        let mut docs = EditorDocs::new();
        assert!(!docs.contains(tab));
        docs.open(tab, Some(Dialect::Sqlite), Some("select 1".into()));
        assert!(docs.contains(tab));
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs.get(tab).and_then(EditorDoc::dialect),
            Some(Dialect::Sqlite)
        );
        assert_eq!(docs.get(tab).and_then(EditorDoc::seed), Some("select 1"));
        assert!(docs.remove(tab).is_some());
        assert!(docs.is_empty());
        assert!(docs.get(tab).is_none());
    }

    #[test]
    fn the_seed_is_taken_once() {
        let tab = tab_id();
        let mut docs = EditorDocs::new();
        docs.open(tab, None, Some("select 1".into()));
        assert_eq!(docs.take_seed(tab).as_deref(), Some("select 1"));
        assert_eq!(docs.take_seed(tab), None);
        assert_eq!(docs.get(tab).and_then(EditorDoc::dialect), None);
    }

    #[test]
    fn occurrence_chain_lives_in_the_doc() {
        let tab = tab_id();
        let mut docs = EditorDocs::new();
        docs.open(tab, None, None);
        let mut b = EditorBuffer::new("ab ab", 2);
        b.selection = 0..2;
        let doc = docs.get_mut(tab).unwrap();
        assert_eq!(doc.add_next_occurrence(&b), OccurrenceStep::Add(3..5));
        assert_eq!(doc.chain().map(OccurrenceChain::len), Some(2));
    }
}
