// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Cache keys, refresh scopes and path helpers.

use sqler_driver_api::sqler_core::{DbObject, ObjectKind, ObjectPath};

/// The address of one list of children: the parent path and the kind listed under it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct NodeKey {
    /// The path of the node whose children are listed.
    pub parent: ObjectPath,
    /// The kind of children listed.
    pub kind: ObjectKind,
}

impl NodeKey {
    /// Builds a key.
    #[must_use]
    pub fn new(parent: ObjectPath, kind: ObjectKind) -> Self {
        Self { parent, kind }
    }
}

/// One cached entry: a list of children or the details of one table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CacheKey {
    /// The children of a node.
    Children(NodeKey),
    /// The columns and keys of a table, view or materialised view.
    Table(DbObject),
}

impl CacheKey {
    /// The path this entry hangs under: the parent of a list, or the table's own path.
    #[must_use]
    pub fn path(&self) -> ObjectPath {
        match self {
            Self::Children(key) => key.parent.clone(),
            Self::Table(obj) => obj.child_path(),
        }
    }

    /// Whether this entry is within `path` (every set field of `path` equals the same field here).
    #[must_use]
    pub fn is_within(&self, path: &ObjectPath) -> bool {
        match self {
            Self::Children(key) => path_within(&key.parent, path),
            Self::Table(obj) => path_within(&obj.child_path(), path),
        }
    }

    /// How deep this entry sits: the number of set fields of its path.
    #[must_use]
    pub fn depth(&self) -> usize {
        path_depth(&self.path())
    }
}

impl From<NodeKey> for CacheKey {
    fn from(key: NodeKey) -> Self {
        Self::Children(key)
    }
}

/// What a refresh re-fetches.
///
/// `Subtree` and `All` only re-fetch entries that are cached when the refresh starts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefreshScope {
    /// One entry, cached or not.
    Node(CacheKey),
    /// Every cached entry within the path. The root path is the same as [`RefreshScope::All`].
    Subtree(ObjectPath),
    /// Every cached entry.
    All,
}

/// Whether `candidate` is within `scope`: every set field of `scope` matches.
///
/// Paths are not normalised: `None` and `Some("main")` are different.
pub(crate) fn path_within(candidate: &ObjectPath, scope: &ObjectPath) -> bool {
    let field = |c: &Option<String>, s: &Option<String>| s.is_none() || c == s;
    field(&candidate.database, &scope.database)
        && field(&candidate.schema, &scope.schema)
        && field(&candidate.table, &scope.table)
}

/// The number of set fields of `path`.
pub(crate) fn path_depth(path: &ObjectPath) -> usize {
    usize::from(path.database.is_some())
        + usize::from(path.schema.is_some())
        + usize::from(path.table.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(parent: &ObjectPath, name: &str) -> DbObject {
        DbObject::new(ObjectKind::Table, parent.clone(), name)
    }

    #[test]
    fn paths_and_depth() {
        let main = ObjectPath::root().in_schema("main");
        let children = CacheKey::Children(NodeKey::new(main.clone(), ObjectKind::Table));
        assert_eq!(children.path(), main);
        assert_eq!(children.depth(), 1);
        let t = CacheKey::Table(table(&main, "t"));
        assert_eq!(t.path(), main.clone().in_table("t"));
        assert_eq!(t.depth(), 2);
        let root = CacheKey::Children(NodeKey::new(ObjectPath::root(), ObjectKind::Schema));
        assert_eq!(root.depth(), 0);
        let full = ObjectPath::root()
            .in_database("d")
            .in_schema("s")
            .in_table("t");
        assert_eq!(path_depth(&full), 3);
    }

    #[test]
    fn containment() {
        let main = ObjectPath::root().in_schema("main");
        let other = ObjectPath::root().in_schema("other");
        let list = CacheKey::Children(NodeKey::new(main.clone(), ObjectKind::Table));
        let t = CacheKey::Table(table(&main, "t"));
        let idx = CacheKey::Children(NodeKey::new(main.clone().in_table("t"), ObjectKind::Index));
        let schemas = CacheKey::Children(NodeKey::new(ObjectPath::root(), ObjectKind::Schema));
        for key in [&list, &t, &idx] {
            assert!(key.is_within(&ObjectPath::root()));
            assert!(key.is_within(&main));
            assert!(!key.is_within(&other));
        }
        assert!(schemas.is_within(&ObjectPath::root()));
        assert!(!schemas.is_within(&main));
        let t_path = main.clone().in_table("t");
        assert!(!list.is_within(&t_path));
        assert!(t.is_within(&t_path));
        assert!(idx.is_within(&t_path));
        assert!(!t.is_within(&main.clone().in_table("u")));
    }

    #[test]
    fn paths_are_not_normalised() {
        let implicit = CacheKey::Children(NodeKey::new(ObjectPath::root(), ObjectKind::Table));
        let explicit = ObjectPath::root().in_schema("main");
        assert!(!implicit.is_within(&explicit));
        assert_ne!(
            NodeKey::new(ObjectPath::root(), ObjectKind::Table),
            NodeKey::new(explicit, ObjectKind::Table)
        );
    }

    #[test]
    fn node_key_converts() {
        let key = NodeKey::new(ObjectPath::root(), ObjectKind::Schema);
        assert_eq!(CacheKey::from(key.clone()), CacheKey::Children(key));
    }
}
