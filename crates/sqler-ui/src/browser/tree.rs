// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! [`ObjectTree`]: the lazy object tree of one connection, as a state machine.
//!
//! The tree never calls the catalog. [`ObjectTree::expand`] returns the [`CacheKey`]s to load,
//! the caller resolves them (with `Catalog::children` / `Catalog::table`) and reports each answer
//! back with [`ObjectTree::children_loaded`] / [`ObjectTree::table_loaded`]. That keeps the tree
//! headless and testable without a runtime.
//!
//! Levels: the root lists the server kinds (databases, roles). A database lists schemas and
//! extensions, a schema lists tables, views and the rest, and a table-like object has a columns
//! node plus index and trigger folders. A driver that answers `Listing::Unsupported` for databases
//! or schemas falls through to the next level at the same path, so `SQLite` shows its tables
//! directly. A folder's children come with its listing, so expanding a folder loads nothing.
//!
//! [`ObjectTree::rows`] is rebuilt when the structure changes, never per frame, and selection
//! changes do not touch it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqler_catalog::{CacheKey, CatalogError, NodeKey, RefreshScope};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DbObject, Listing, ObjectKind, ObjectLevel, ObjectPath, Table,
};

/// The identity of a tree node. Ids are never reused within one tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeNodeId(u32);

/// What a tree node stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TreeNodeKind {
    /// The connection itself.
    Root,
    /// A folder of one kind of children, for example "Tables".
    Folder(NodeKey),
    /// A database object.
    Object(DbObject),
    /// The columns of a table-like object.
    Columns(DbObject),
    /// One column of a table-like object.
    Column {
        /// The table, view or materialized view.
        table: DbObject,
        /// The column.
        column: ColumnInfo,
    },
}

/// Whether the children of a node are known.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadState {
    /// Not asked for yet.
    NotLoaded,
    /// Loads are in flight.
    Loading,
    /// Every load has answered.
    Loaded,
    /// A load failed; expanding again retries.
    Failed(String),
}

/// One visible row of the tree (the root itself is not a row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The node.
    pub id: TreeNodeId,
    /// Indent level; the children of the root are at depth 0.
    pub depth: usize,
    /// The text: the object name, `Tables (5000)` for a folder, `name  type` for a column.
    pub label: String,
    /// The kind, for object rows.
    pub kind: Option<ObjectKind>,
    /// Whether the node can have children.
    pub expandable: bool,
    /// Whether the children are shown.
    pub expanded: bool,
    /// Whether the children are known.
    pub state: LoadState,
    /// Refresh progress `(done, total)`, while a refresh runs below this node.
    pub progress: Option<(usize, usize)>,
}

struct Node {
    kind: TreeNodeKind,
    parent: Option<TreeNodeId>,
    children: Vec<TreeNodeId>,
    expanded: bool,
    state: LoadState,
    /// Every key this node asked for (base keys and fall-through keys).
    keys: Vec<CacheKey>,
    /// Keys asked for and not answered yet.
    pending: HashSet<CacheKey>,
    errors: Vec<(CacheKey, String)>,
    progress: Option<(usize, usize)>,
    label: String,
}

/// The lazy object tree of one connection.
pub struct ObjectTree {
    nodes: HashMap<TreeNodeId, Node>,
    next_id: u32,
    root: TreeNodeId,
    owners: HashMap<CacheKey, TreeNodeId>,
    selected: Option<TreeNodeId>,
    rows: Vec<TreeRow>,
}

impl std::fmt::Debug for ObjectTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectTree")
            .field("nodes", &self.nodes.len())
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Default for ObjectTree {
    fn default() -> Self {
        Self::new()
    }
}

const SERVER_KINDS: [ObjectKind; 2] = [ObjectKind::Database, ObjectKind::Role];
const DATABASE_KINDS: [ObjectKind; 2] = [ObjectKind::Schema, ObjectKind::Extension];
const SCHEMA_KINDS: [ObjectKind; 7] = [
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::MaterializedView,
    ObjectKind::Function,
    ObjectKind::Procedure,
    ObjectKind::Sequence,
    ObjectKind::Type,
];

fn level_keys(level: ObjectLevel, path: &ObjectPath) -> Vec<CacheKey> {
    let kinds: &[ObjectKind] = match level {
        ObjectLevel::Server => &SERVER_KINDS,
        ObjectLevel::Database => &DATABASE_KINDS,
        _ => &SCHEMA_KINDS,
    };
    kinds
        .iter()
        .map(|kind| CacheKey::Children(NodeKey::new(path.clone(), *kind)))
        .collect()
}

/// The level whose kinds are listed at the same path when `kind` is unsupported.
fn fall_through(kind: ObjectKind) -> Option<ObjectLevel> {
    match kind {
        ObjectKind::Database => Some(ObjectLevel::Database),
        ObjectKind::Schema => Some(ObjectLevel::Schema),
        _ => None,
    }
}

pub(super) fn is_table_like(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
    )
}

/// The keys a node asks for when it is expanded for the first time.
fn base_keys(kind: &TreeNodeKind) -> Vec<CacheKey> {
    match kind {
        TreeNodeKind::Root => level_keys(ObjectLevel::Server, &ObjectPath::root()),
        TreeNodeKind::Object(obj) => match obj.kind {
            ObjectKind::Database => level_keys(ObjectLevel::Database, &obj.child_path()),
            ObjectKind::Schema => level_keys(ObjectLevel::Schema, &obj.child_path()),
            kind if is_table_like(kind) => {
                let path = obj.child_path();
                vec![
                    CacheKey::Table(obj.clone()),
                    CacheKey::Children(NodeKey::new(path.clone(), ObjectKind::Index)),
                    CacheKey::Children(NodeKey::new(path, ObjectKind::Trigger)),
                ]
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn plural(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Database => "Databases",
        ObjectKind::Schema => "Schemas",
        ObjectKind::Table => "Tables",
        ObjectKind::View => "Views",
        ObjectKind::MaterializedView => "Materialized Views",
        ObjectKind::Function => "Functions",
        ObjectKind::Procedure => "Procedures",
        ObjectKind::Trigger => "Triggers",
        ObjectKind::Sequence => "Sequences",
        ObjectKind::Type => "Types",
        ObjectKind::Index => "Indexes",
        ObjectKind::Extension => "Extensions",
        ObjectKind::Role => "Roles",
        _ => "Objects",
    }
}

fn folder_label(kind: ObjectKind, count: usize) -> String {
    format!("{} ({count})", plural(kind))
}

fn column_label(column: &ColumnInfo) -> String {
    let ty = column
        .native_type
        .clone()
        .unwrap_or_else(|| format!("{:?}", column.data_type).to_lowercase());
    format!("{}  {ty}", column.name)
}

/// Where a child sorts among its siblings: columns first, then folders in `ObjectKind::ALL` order.
fn child_rank(kind: &TreeNodeKind) -> usize {
    match kind {
        TreeNodeKind::Columns(_) => 0,
        TreeNodeKind::Folder(key) => {
            1 + ObjectKind::ALL
                .iter()
                .position(|k| *k == key.kind)
                .unwrap_or(ObjectKind::ALL.len())
        }
        _ => ObjectKind::ALL.len() + 2,
    }
}

impl ObjectTree {
    /// An empty tree: a collapsed root that has not loaded anything.
    #[must_use]
    pub fn new() -> Self {
        let root = TreeNodeId(0);
        let mut nodes = HashMap::new();
        nodes.insert(
            root,
            Node {
                kind: TreeNodeKind::Root,
                parent: None,
                children: Vec::new(),
                expanded: false,
                state: LoadState::NotLoaded,
                keys: Vec::new(),
                pending: HashSet::new(),
                errors: Vec::new(),
                progress: None,
                label: String::new(),
            },
        );
        Self {
            nodes,
            next_id: 1,
            root,
            owners: HashMap::new(),
            selected: None,
            rows: Vec::new(),
        }
    }

    /// The root node (the connection).
    #[must_use]
    pub fn root(&self) -> TreeNodeId {
        self.root
    }

    /// The visible rows, in display order. Empty while the root is collapsed.
    #[must_use]
    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    /// What `id` stands for.
    #[must_use]
    pub fn kind(&self, id: TreeNodeId) -> Option<&TreeNodeKind> {
        self.nodes.get(&id).map(|n| &n.kind)
    }

    /// The database object behind `id`, for object nodes.
    #[must_use]
    pub fn object(&self, id: TreeNodeId) -> Option<&DbObject> {
        match self.kind(id)? {
            TreeNodeKind::Object(obj) => Some(obj),
            _ => None,
        }
    }

    /// The load state of `id`.
    #[must_use]
    pub fn state(&self, id: TreeNodeId) -> Option<&LoadState> {
        self.nodes.get(&id).map(|n| &n.state)
    }

    /// Whether the children of `id` are shown.
    #[must_use]
    pub fn is_expanded(&self, id: TreeNodeId) -> bool {
        self.nodes.get(&id).is_some_and(|n| n.expanded)
    }

    /// The refresh progress shown on `id`.
    #[must_use]
    pub fn progress(&self, id: TreeNodeId) -> Option<(usize, usize)> {
        self.nodes.get(&id).and_then(|n| n.progress)
    }

    /// The parent of `id`.
    #[must_use]
    pub fn parent(&self, id: TreeNodeId) -> Option<TreeNodeId> {
        self.nodes.get(&id).and_then(|n| n.parent)
    }

    /// The selected node, if any.
    #[must_use]
    pub fn selected(&self) -> Option<TreeNodeId> {
        self.selected
    }

    /// The row position of `id`, if it is visible.
    #[must_use]
    pub fn row_index(&self, id: TreeNodeId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    /// Selects `id`; `false` when there is no such node.
    pub fn select(&mut self, id: TreeNodeId) -> bool {
        let exists = self.nodes.contains_key(&id);
        if exists {
            self.selected = Some(id);
        }
        exists
    }

    /// Selects the next visible row (the first one when nothing is selected).
    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let target = match self.selected.and_then(|id| self.row_index(id)) {
            Some(i) => (i + 1).min(last),
            None => 0,
        };
        self.selected = self.rows.get(target).map(|r| r.id);
    }

    /// Selects the previous visible row; from the first row it selects the root.
    pub fn select_previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        if self.selected == Some(self.root) {
            return;
        }
        self.selected = match self.selected.and_then(|id| self.row_index(id)) {
            Some(0) => Some(self.root),
            Some(i) => self.rows.get(i - 1).map(|r| r.id),
            None => self.rows.last().map(|r| r.id),
        };
    }

    /// Shows the children of `id` and returns the keys to load for it.
    ///
    /// Nothing is returned when the node is loading or loaded, or has nothing to load. A failed
    /// node asks again.
    pub fn expand(&mut self, id: TreeNodeId) -> Vec<CacheKey> {
        let Some(node) = self.nodes.get(&id) else {
            return Vec::new();
        };
        let keys = base_keys(&node.kind);
        let can_expand = !keys.is_empty()
            || (matches!(
                node.kind,
                TreeNodeKind::Folder(_) | TreeNodeKind::Columns(_)
            ) && !node.children.is_empty());
        if !can_expand {
            return Vec::new();
        }
        let load =
            matches!(node.state, LoadState::NotLoaded | LoadState::Failed(_)) && !keys.is_empty();
        if let Some(node) = self.nodes.get_mut(&id) {
            node.expanded = true;
        }
        let mut out = Vec::new();
        if load {
            for key in &keys {
                self.own(id, key.clone());
            }
            if let Some(node) = self.nodes.get_mut(&id) {
                node.errors.clear();
                node.pending = keys.iter().cloned().collect();
                node.state = LoadState::Loading;
            }
            out = keys;
        }
        self.rebuild();
        out
    }

    /// Hides the children of `id`. A selection inside moves to `id`.
    pub fn collapse(&mut self, id: TreeNodeId) {
        let Some(node) = self.nodes.get_mut(&id) else {
            return;
        };
        if !node.expanded {
            return;
        }
        node.expanded = false;
        if let Some(sel) = self.selected
            && sel != id
            && self.is_descendant(sel, id)
        {
            self.selected = Some(id);
        }
        self.rebuild();
    }

    fn is_descendant(&self, mut id: TreeNodeId, ancestor: TreeNodeId) -> bool {
        while let Some(parent) = self.parent(id) {
            if parent == ancestor {
                return true;
            }
            id = parent;
        }
        false
    }

    /// Applies the answer for one listing and returns more keys to load (the fall-through).
    ///
    /// Answers for keys no node owns (stale) are ignored, and an answer that was not pending
    /// (a duplicate or a reload) only reconciles the children.
    pub fn children_loaded(
        &mut self,
        key: &NodeKey,
        result: Result<Arc<Listing>, CatalogError>,
    ) -> Vec<CacheKey> {
        let ck = CacheKey::Children(key.clone());
        let Some(&owner) = self.owners.get(&ck) else {
            return Vec::new();
        };
        let first = self.take_pending(owner, &ck);
        let mut follow = Vec::new();
        match result {
            Ok(listing) => {
                self.set_error(owner, &ck, None);
                match &*listing {
                    Listing::Objects(objects) => self.apply_objects(owner, key, objects),
                    Listing::Unsupported => {
                        self.remove_folder(owner, key);
                        if let Some(level) = fall_through(key.kind) {
                            for next in level_keys(level, &key.parent) {
                                if first || !self.owners.contains_key(&next) {
                                    self.own(owner, next.clone());
                                    if let Some(node) = self.nodes.get_mut(&owner) {
                                        node.pending.insert(next.clone());
                                    }
                                    follow.push(next);
                                }
                            }
                        }
                    }
                }
            }
            Err(err) => self.set_error(owner, &ck, Some(err.to_string())),
        }
        self.settle(owner);
        self.rebuild();
        follow
    }

    /// Applies the answer for one table: its columns node, or a failure.
    pub fn table_loaded(
        &mut self,
        obj: &DbObject,
        result: Result<Option<Arc<Table>>, CatalogError>,
    ) {
        let ck = CacheKey::Table(obj.clone());
        let Some(&owner) = self.owners.get(&ck) else {
            return;
        };
        self.take_pending(owner, &ck);
        match result {
            Ok(table) => {
                self.set_error(owner, &ck, None);
                match table {
                    Some(table) => self.apply_columns(owner, obj, &table),
                    None => self.remove_columns(owner, obj),
                }
            }
            Err(err) => self.set_error(owner, &ck, Some(err.to_string())),
        }
        self.settle(owner);
        self.rebuild();
    }

    /// What a refresh of `id` re-fetches.
    #[must_use]
    pub fn refresh_scope(&self, id: TreeNodeId) -> Option<RefreshScope> {
        let node = self.nodes.get(&id)?;
        Some(match &node.kind {
            TreeNodeKind::Root => RefreshScope::All,
            TreeNodeKind::Folder(key) => RefreshScope::Node(CacheKey::Children(key.clone())),
            TreeNodeKind::Object(obj) => {
                if matches!(obj.kind, ObjectKind::Database | ObjectKind::Schema)
                    || is_table_like(obj.kind)
                {
                    RefreshScope::Subtree(obj.child_path())
                } else {
                    match &self.nodes.get(&node.parent?)?.kind {
                        TreeNodeKind::Folder(key) => {
                            RefreshScope::Node(CacheKey::Children(key.clone()))
                        }
                        _ => return None,
                    }
                }
            }
            TreeNodeKind::Columns(obj) | TreeNodeKind::Column { table: obj, .. } => {
                RefreshScope::Node(CacheKey::Table(obj.clone()))
            }
        })
    }

    /// Every key the tree holds, parents before children, to load again after a refresh.
    #[must_use]
    pub fn reload_keys(&self) -> Vec<CacheKey> {
        let mut out = Vec::new();
        let mut level = vec![self.root];
        while !level.is_empty() {
            let mut next = Vec::new();
            for id in level {
                if let Some(node) = self.nodes.get(&id) {
                    out.extend(node.keys.iter().cloned());
                    next.extend(node.children.iter().copied());
                }
            }
            level = next;
        }
        out
    }

    /// Shows (or clears) the refresh progress on `id`.
    pub fn set_progress(&mut self, id: TreeNodeId, progress: Option<(usize, usize)>) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.progress = progress;
        }
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.progress = progress;
        }
    }

    fn own(&mut self, id: TreeNodeId, key: CacheKey) {
        self.owners.insert(key.clone(), id);
        if let Some(node) = self.nodes.get_mut(&id)
            && !node.keys.contains(&key)
        {
            node.keys.push(key);
        }
    }

    fn take_pending(&mut self, id: TreeNodeId, key: &CacheKey) -> bool {
        self.nodes
            .get_mut(&id)
            .is_some_and(|n| n.pending.remove(key))
    }

    fn set_error(&mut self, id: TreeNodeId, key: &CacheKey, message: Option<String>) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.errors.retain(|(k, _)| k != key);
            if let Some(message) = message {
                node.errors.push((key.clone(), message));
            }
        }
    }

    fn settle(&mut self, id: TreeNodeId) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.state = if !node.pending.is_empty() {
                LoadState::Loading
            } else if let Some((_, message)) = node.errors.first() {
                LoadState::Failed(message.clone())
            } else {
                LoadState::Loaded
            };
        }
    }

    fn alloc(&mut self, kind: TreeNodeKind, parent: TreeNodeId, label: String) -> TreeNodeId {
        let id = TreeNodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(
            id,
            Node {
                kind,
                parent: Some(parent),
                children: Vec::new(),
                expanded: false,
                state: LoadState::NotLoaded,
                keys: Vec::new(),
                pending: HashSet::new(),
                errors: Vec::new(),
                progress: None,
                label,
            },
        );
        id
    }

    fn find_child(&self, parent: TreeNodeId, wanted: &TreeNodeKind) -> Option<TreeNodeId> {
        let node = self.nodes.get(&parent)?;
        node.children
            .iter()
            .copied()
            .find(|c| self.nodes.get(c).is_some_and(|n| &n.kind == wanted))
    }

    fn add_child(&mut self, parent: TreeNodeId, kind: TreeNodeKind, label: String) -> TreeNodeId {
        let id = self.alloc(kind, parent, label);
        if let Some(node) = self.nodes.get_mut(&parent) {
            node.children.push(id);
        }
        id
    }

    fn sort_children(&mut self, parent: TreeNodeId) {
        let Some(node) = self.nodes.get(&parent) else {
            return;
        };
        let mut children = node.children.clone();
        children.sort_by_key(|c| {
            self.nodes
                .get(c)
                .map_or(usize::MAX, |n| child_rank(&n.kind))
        });
        if let Some(node) = self.nodes.get_mut(&parent) {
            node.children = children;
        }
    }

    fn apply_objects(&mut self, owner: TreeNodeId, key: &NodeKey, objects: &[DbObject]) {
        let wanted = TreeNodeKind::Folder(key.clone());
        let folder = if let Some(folder) = self.find_child(owner, &wanted) {
            folder
        } else {
            let folder = self.add_child(owner, wanted, folder_label(key.kind, objects.len()));
            if let Some(node) = self.nodes.get_mut(&folder) {
                node.state = LoadState::Loaded;
            }
            self.sort_children(owner);
            folder
        };
        let old = self
            .nodes
            .get_mut(&folder)
            .map(|n| std::mem::take(&mut n.children))
            .unwrap_or_default();
        let mut by_object: HashMap<DbObject, TreeNodeId> = HashMap::with_capacity(old.len());
        for id in old {
            if let Some(TreeNodeKind::Object(obj)) = self.nodes.get(&id).map(|n| &n.kind) {
                by_object.insert(obj.clone(), id);
            }
        }
        let mut seen: HashSet<&DbObject> = HashSet::with_capacity(objects.len());
        let mut children = Vec::with_capacity(objects.len());
        for obj in objects {
            if !seen.insert(obj) {
                continue;
            }
            let id = match by_object.remove(obj) {
                Some(id) => id,
                None => self.alloc(TreeNodeKind::Object(obj.clone()), folder, obj.name.clone()),
            };
            children.push(id);
        }
        let count = children.len();
        if let Some(node) = self.nodes.get_mut(&folder) {
            node.children = children;
            node.label = folder_label(key.kind, count);
        }
        for (_, gone) in by_object {
            self.remove_subtree(gone, folder);
        }
    }

    fn remove_folder(&mut self, owner: TreeNodeId, key: &NodeKey) {
        if let Some(folder) = self.find_child(owner, &TreeNodeKind::Folder(key.clone())) {
            self.detach(owner, folder);
        }
    }

    fn apply_columns(&mut self, owner: TreeNodeId, obj: &DbObject, table: &Table) {
        let wanted = TreeNodeKind::Columns(obj.clone());
        let columns = if let Some(id) = self.find_child(owner, &wanted) {
            id
        } else {
            let id = self.add_child(owner, wanted, String::new());
            self.sort_children(owner);
            id
        };
        let old = self
            .nodes
            .get_mut(&columns)
            .map(|n| std::mem::take(&mut n.children))
            .unwrap_or_default();
        let mut by_name: HashMap<String, TreeNodeId> = HashMap::new();
        for id in old {
            if let Some(TreeNodeKind::Column { column, .. }) = self.nodes.get(&id).map(|n| &n.kind)
            {
                by_name.insert(column.name.clone(), id);
            }
        }
        let mut children = Vec::with_capacity(table.columns.len());
        for column in &table.columns {
            let kind = TreeNodeKind::Column {
                table: obj.clone(),
                column: column.clone(),
            };
            let label = column_label(column);
            match by_name.remove(&column.name) {
                Some(id) => {
                    if let Some(node) = self.nodes.get_mut(&id) {
                        node.kind = kind;
                        node.label = label;
                    }
                    children.push(id);
                }
                None => children.push(self.alloc(kind, columns, label)),
            }
        }
        let count = children.len();
        if let Some(node) = self.nodes.get_mut(&columns) {
            node.children = children;
            node.label = format!("Columns ({count})");
            node.state = LoadState::Loaded;
        }
        for (_, gone) in by_name {
            self.remove_subtree(gone, columns);
        }
    }

    fn remove_columns(&mut self, owner: TreeNodeId, obj: &DbObject) {
        if let Some(columns) = self.find_child(owner, &TreeNodeKind::Columns(obj.clone())) {
            self.detach(owner, columns);
        }
    }

    /// Removes `child` from `parent` and drops its subtree.
    fn detach(&mut self, parent: TreeNodeId, child: TreeNodeId) {
        if let Some(node) = self.nodes.get_mut(&parent) {
            node.children.retain(|c| *c != child);
        }
        self.remove_subtree(child, parent);
    }

    /// Drops `id` and everything below it; a selection inside moves to `fallback`.
    fn remove_subtree(&mut self, id: TreeNodeId, fallback: TreeNodeId) {
        let mut stack = vec![id];
        while let Some(next) = stack.pop() {
            let Some(node) = self.nodes.remove(&next) else {
                continue;
            };
            for key in &node.keys {
                if self.owners.get(key) == Some(&next) {
                    self.owners.remove(key);
                }
            }
            if self.selected == Some(next) {
                self.selected = Some(fallback);
            }
            stack.extend(node.children);
        }
    }

    fn is_expandable(node: &Node) -> bool {
        match &node.kind {
            TreeNodeKind::Root => true,
            TreeNodeKind::Folder(_) | TreeNodeKind::Columns(_) => !node.children.is_empty(),
            TreeNodeKind::Object(obj) => {
                matches!(obj.kind, ObjectKind::Database | ObjectKind::Schema)
                    || is_table_like(obj.kind)
            }
            TreeNodeKind::Column { .. } => false,
        }
    }

    fn rebuild(&mut self) {
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();
        self.push_children(self.root, 0, &mut rows);
        self.rows = rows;
    }

    fn push_children(&self, id: TreeNodeId, depth: usize, out: &mut Vec<TreeRow>) {
        let Some(node) = self.nodes.get(&id) else {
            return;
        };
        if !node.expanded {
            return;
        }
        for child in &node.children {
            let Some(c) = self.nodes.get(child) else {
                continue;
            };
            let expandable = Self::is_expandable(c);
            out.push(TreeRow {
                id: *child,
                depth,
                label: c.label.clone(),
                kind: match &c.kind {
                    TreeNodeKind::Object(obj) => Some(obj.kind),
                    _ => None,
                },
                expandable,
                expanded: expandable && c.expanded,
                state: c.state.clone(),
                progress: c.progress,
            });
            self.push_children(*child, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_obj(name: &str) -> DbObject {
        DbObject::new(ObjectKind::Schema, ObjectPath::root(), name)
    }

    fn table_in(path: &ObjectPath, name: &str) -> DbObject {
        DbObject::new(ObjectKind::Table, path.clone(), name)
    }

    #[allow(clippy::unnecessary_wraps)] // shaped like the catalog's answer
    fn objects(list: Vec<DbObject>) -> Result<Arc<Listing>, CatalogError> {
        Ok(Arc::new(Listing::Objects(list)))
    }

    #[allow(clippy::unnecessary_wraps)] // shaped like the catalog's answer
    fn unsupported() -> Result<Arc<Listing>, CatalogError> {
        Ok(Arc::new(Listing::Unsupported))
    }

    fn nk(path: &ObjectPath, kind: ObjectKind) -> NodeKey {
        NodeKey::new(path.clone(), kind)
    }

    fn ck(path: &ObjectPath, kind: ObjectKind) -> CacheKey {
        CacheKey::Children(nk(path, kind))
    }

    fn labels(tree: &ObjectTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.label))
            .collect()
    }

    fn row_id(tree: &ObjectTree, label: &str) -> TreeNodeId {
        tree.rows()
            .iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("no row {label}: {:?}", labels(tree)))
            .id
    }

    /// A Postgres-like tree: root -> database `app` -> schema `public` with tables.
    fn postgres_tree(tables: &[&str]) -> (ObjectTree, ObjectPath) {
        let mut tree = ObjectTree::new();
        let root = tree.root();
        let keys = tree.expand(root);
        assert_eq!(
            keys,
            vec![
                ck(&ObjectPath::root(), ObjectKind::Database),
                ck(&ObjectPath::root(), ObjectKind::Role)
            ]
        );
        let db = DbObject::new(ObjectKind::Database, ObjectPath::root(), "app");
        assert_eq!(
            tree.children_loaded(
                &nk(&ObjectPath::root(), ObjectKind::Database),
                objects(vec![db.clone()])
            )
            .len(),
            0
        );
        assert_eq!(
            tree.children_loaded(&nk(&ObjectPath::root(), ObjectKind::Role), unsupported())
                .len(),
            0
        );
        let db_row = row_id(&tree, "Databases (1)");
        tree.expand(db_row);
        let db_id = row_id(&tree, "app");
        let keys = tree.expand(db_id);
        let db_path = db.child_path();
        assert_eq!(
            keys,
            vec![
                ck(&db_path, ObjectKind::Schema),
                ck(&db_path, ObjectKind::Extension)
            ]
        );
        let public = DbObject::new(ObjectKind::Schema, db_path.clone(), "public");
        tree.children_loaded(
            &nk(&db_path, ObjectKind::Schema),
            objects(vec![public.clone()]),
        );
        tree.children_loaded(&nk(&db_path, ObjectKind::Extension), unsupported());
        let schemas = row_id(&tree, "Schemas (1)");
        tree.expand(schemas);
        let schema_id = row_id(&tree, "public");
        let keys = tree.expand(schema_id);
        let sp = public.child_path();
        assert_eq!(keys.len(), 7);
        assert_eq!(keys[0], ck(&sp, ObjectKind::Table));
        let list: Vec<DbObject> = tables.iter().map(|t| table_in(&sp, t)).collect();
        tree.children_loaded(&nk(&sp, ObjectKind::Table), objects(list));
        for kind in &SCHEMA_KINDS[1..] {
            tree.children_loaded(&nk(&sp, *kind), unsupported());
        }
        (tree, sp)
    }

    #[test]
    fn new_tree_is_collapsed_and_empty() {
        let tree = ObjectTree::new();
        assert_eq!(tree.rows().len(), 0);
        assert_eq!(tree.state(tree.root()), Some(&LoadState::NotLoaded));
        assert_eq!(tree.selected(), None);
        assert!(!tree.is_expanded(tree.root()));
    }

    #[test]
    fn root_expand_loads_server_kinds_and_shows_loading() {
        let mut tree = ObjectTree::new();
        let root = tree.root();
        let keys = tree.expand(root);
        assert_eq!(keys.len(), 2);
        assert_eq!(tree.state(root), Some(&LoadState::Loading));
        assert!(tree.expand(root).is_empty(), "loading: no second load");
        let path = ObjectPath::root();
        tree.children_loaded(&nk(&path, ObjectKind::Database), objects(vec![]));
        assert_eq!(tree.state(root), Some(&LoadState::Loading));
        tree.children_loaded(&nk(&path, ObjectKind::Role), unsupported());
        assert_eq!(tree.state(root), Some(&LoadState::Loaded));
        assert_eq!(labels(&tree), vec!["Databases (0)"]);
        assert!(!tree.rows()[0].expandable, "an empty folder cannot expand");
    }

    #[test]
    fn postgres_levels_and_folder_order() {
        let (mut tree, sp) = postgres_tree(&["a", "b"]);
        assert_eq!(
            labels(&tree),
            vec![
                "Databases (1)",
                "  app",
                "    Schemas (1)",
                "      public",
                "        Tables (2)"
            ]
        );
        let folder = row_id(&tree, "Tables (2)");
        assert!(tree.expand(folder).is_empty(), "folders load nothing");
        assert_eq!(labels(&tree).len(), 7);
        let a = row_id(&tree, "a");
        let keys = tree.expand(a);
        let a_obj = table_in(&sp, "a");
        let ap = a_obj.child_path();
        assert_eq!(
            keys,
            vec![
                CacheKey::Table(a_obj.clone()),
                ck(&ap, ObjectKind::Index),
                ck(&ap, ObjectKind::Trigger)
            ]
        );
        tree.children_loaded(
            &nk(&ap, ObjectKind::Trigger),
            objects(vec![DbObject::new(ObjectKind::Trigger, ap.clone(), "trg")]),
        );
        tree.children_loaded(
            &nk(&ap, ObjectKind::Index),
            objects(vec![DbObject::new(ObjectKind::Index, ap.clone(), "ix")]),
        );
        let table = Table::new(a_obj.clone());
        let mut table = table;
        table.columns = vec![
            ColumnInfo::new("id", sqler_driver_api::sqler_core::DataType::Integer),
            ColumnInfo::new("name", sqler_driver_api::sqler_core::DataType::Text)
                .with_native_type("varchar(20)"),
        ];
        tree.table_loaded(&a_obj, Ok(Some(Arc::new(table))));
        assert_eq!(tree.state(a), Some(&LoadState::Loaded));
        let l = labels(&tree);
        let at = l.iter().position(|s| s.trim() == "a").unwrap();
        assert_eq!(l[at + 1].trim(), "Columns (2)");
        // Folders follow `ObjectKind::ALL`: triggers come before indexes.
        assert_eq!(l[at + 2].trim(), "Triggers (1)");
        assert_eq!(l[at + 3].trim(), "Indexes (1)");
        let cols = row_id(&tree, "Columns (2)");
        tree.expand(cols);
        let l = labels(&tree);
        assert!(l.iter().any(|s| s.trim() == "id  integer"));
        assert!(l.iter().any(|s| s.trim() == "name  varchar(20)"));
    }

    #[test]
    fn sqlite_like_falls_through_to_schemas_then_tables() {
        let mut tree = ObjectTree::new();
        let root = tree.root();
        tree.expand(root);
        let p = ObjectPath::root();
        let follow = tree.children_loaded(&nk(&p, ObjectKind::Database), unsupported());
        assert_eq!(
            follow,
            vec![ck(&p, ObjectKind::Schema), ck(&p, ObjectKind::Extension)]
        );
        assert_eq!(tree.state(root), Some(&LoadState::Loading));
        assert_eq!(
            tree.children_loaded(&nk(&p, ObjectKind::Role), unsupported())
                .len(),
            0
        );
        let follow = tree.children_loaded(&nk(&p, ObjectKind::Schema), unsupported());
        assert_eq!(follow.len(), 7);
        assert_eq!(follow[0], ck(&p, ObjectKind::Table));
        tree.children_loaded(&nk(&p, ObjectKind::Extension), unsupported());
        assert_eq!(tree.state(root), Some(&LoadState::Loading));
        tree.children_loaded(&nk(&p, ObjectKind::Table), objects(vec![table_in(&p, "t")]));
        for kind in &SCHEMA_KINDS[1..] {
            tree.children_loaded(&nk(&p, *kind), unsupported());
        }
        assert_eq!(tree.state(root), Some(&LoadState::Loaded));
        assert_eq!(labels(&tree), vec!["Tables (1)"]);
    }

    #[test]
    fn sqlite_like_with_a_schema_list() {
        let mut tree = ObjectTree::new();
        let root = tree.root();
        tree.expand(root);
        let p = ObjectPath::root();
        tree.children_loaded(&nk(&p, ObjectKind::Database), unsupported());
        tree.children_loaded(&nk(&p, ObjectKind::Role), unsupported());
        tree.children_loaded(
            &nk(&p, ObjectKind::Schema),
            objects(vec![schema_obj("main")]),
        );
        tree.children_loaded(&nk(&p, ObjectKind::Extension), unsupported());
        assert_eq!(labels(&tree), vec!["Schemas (1)"]);
        tree.expand(row_id(&tree, "Schemas (1)"));
        let keys = tree.expand(row_id(&tree, "main"));
        assert_eq!(keys.len(), 7);
        assert_eq!(
            keys[0],
            ck(&schema_obj("main").child_path(), ObjectKind::Table)
        );
    }

    #[test]
    fn unsupported_kinds_add_no_folder() {
        let (tree, _) = postgres_tree(&["a"]);
        let l = labels(&tree);
        assert!(!l.iter().any(|s| s.contains("Views")));
        assert!(!l.iter().any(|s| s.contains("Extensions")));
    }

    #[test]
    fn error_marks_only_that_node_and_expanding_retries() {
        let mut tree = ObjectTree::new();
        let root = tree.root();
        tree.expand(root);
        let p = ObjectPath::root();
        tree.children_loaded(&nk(&p, ObjectKind::Database), Err(CatalogError::Closed));
        tree.children_loaded(&nk(&p, ObjectKind::Role), unsupported());
        assert_eq!(
            tree.state(root),
            Some(&LoadState::Failed("catalog worker is closed".into()))
        );
        let keys = tree.expand(root);
        assert_eq!(keys.len(), 2, "retry asks again");
        assert_eq!(tree.state(root), Some(&LoadState::Loading));
        tree.children_loaded(&nk(&p, ObjectKind::Database), objects(vec![]));
        tree.children_loaded(&nk(&p, ObjectKind::Role), unsupported());
        assert_eq!(tree.state(root), Some(&LoadState::Loaded));
    }

    #[test]
    fn a_failed_table_does_not_fail_its_siblings() {
        let (mut tree, sp) = postgres_tree(&["a", "b"]);
        let a_obj = table_in(&sp, "a");
        tree.expand(row_id(&tree, "Tables (2)"));
        let a = row_id(&tree, "a");
        let b = row_id(&tree, "b");
        tree.expand(a);
        tree.expand(b);
        tree.table_loaded(&a_obj, Err(CatalogError::Driver("boom".into())));
        let ap = a_obj.child_path();
        tree.children_loaded(&nk(&ap, ObjectKind::Index), unsupported());
        tree.children_loaded(&nk(&ap, ObjectKind::Trigger), unsupported());
        assert_eq!(
            tree.state(a),
            Some(&LoadState::Failed("introspection failed: boom".into()))
        );
        assert_eq!(tree.state(b), Some(&LoadState::Loading));
    }

    #[test]
    fn stale_and_duplicate_results_are_ignored() {
        let (mut tree, sp) = postgres_tree(&["a", "b"]);
        let before = labels(&tree);
        // A key nobody owns.
        let other = ObjectPath::root().in_schema("elsewhere");
        assert_eq!(
            tree.children_loaded(
                &nk(&other, ObjectKind::Table),
                objects(vec![table_in(&other, "x")])
            )
            .len(),
            0
        );
        tree.table_loaded(&table_in(&other, "x"), Ok(None));
        // A duplicate answer changes nothing.
        tree.children_loaded(
            &nk(&sp, ObjectKind::Table),
            objects(vec![table_in(&sp, "a"), table_in(&sp, "b")]),
        );
        assert_eq!(labels(&tree), before);
        let schema = tree
            .owners
            .get(&ck(&sp, ObjectKind::Table))
            .copied()
            .unwrap();
        assert_eq!(tree.state(schema), Some(&LoadState::Loaded));
    }

    #[test]
    fn duplicate_objects_in_a_listing_show_once() {
        let (tree, _) = postgres_tree(&["a", "a", "b"]);
        assert!(labels(&tree).contains(&"        Tables (2)".to_owned()));
    }

    #[test]
    fn reload_keeps_expanded_and_selected_by_identity() {
        let (mut tree, sp) = postgres_tree(&["a", "b", "c"]);
        tree.expand(row_id(&tree, "Tables (3)"));
        let b = row_id(&tree, "b");
        tree.select(b);
        let keys = tree.reload_keys();
        assert!(keys.contains(&ck(&sp, ObjectKind::Table)));
        assert_eq!(keys[0], ck(&ObjectPath::root(), ObjectKind::Database));
        // `a` vanished, `d` appeared; the folder stays expanded and `b` stays selected.
        tree.children_loaded(
            &nk(&sp, ObjectKind::Table),
            objects(vec![
                table_in(&sp, "b"),
                table_in(&sp, "c"),
                table_in(&sp, "d"),
            ]),
        );
        assert_eq!(row_id(&tree, "b"), b);
        assert_eq!(tree.selected(), Some(b));
        let l = labels(&tree);
        assert!(l.contains(&"          d".to_owned()));
        assert!(!l.iter().any(|s| s.trim() == "a"));
        assert!(l.contains(&"        Tables (3)".to_owned()));
    }

    #[test]
    fn a_vanished_selection_moves_to_the_parent() {
        let (mut tree, sp) = postgres_tree(&["a", "b"]);
        let folder = row_id(&tree, "Tables (2)");
        tree.expand(folder);
        tree.select(row_id(&tree, "a"));
        tree.children_loaded(
            &nk(&sp, ObjectKind::Table),
            objects(vec![table_in(&sp, "b")]),
        );
        assert_eq!(tree.selected(), Some(folder));
    }

    #[test]
    fn reload_of_an_expanded_table_keeps_its_columns_open() {
        let (mut tree, sp) = postgres_tree(&["a"]);
        tree.expand(row_id(&tree, "Tables (1)"));
        let a_obj = table_in(&sp, "a");
        tree.expand(row_id(&tree, "a"));
        let mut t = Table::new(a_obj.clone());
        t.columns = vec![ColumnInfo::new(
            "id",
            sqler_driver_api::sqler_core::DataType::Integer,
        )];
        tree.table_loaded(&a_obj, Ok(Some(Arc::new(t.clone()))));
        let cols = row_id(&tree, "Columns (1)");
        tree.expand(cols);
        t.columns.push(ColumnInfo::new(
            "n",
            sqler_driver_api::sqler_core::DataType::Text,
        ));
        tree.table_loaded(&a_obj, Ok(Some(Arc::new(t))));
        assert_eq!(row_id(&tree, "Columns (2)"), cols);
        assert!(labels(&tree).iter().any(|s| s.trim() == "n  text"));
        tree.table_loaded(&a_obj, Ok(None));
        assert!(!labels(&tree).iter().any(|s| s.contains("Columns")));
    }

    #[test]
    fn collapse_hides_children_and_moves_selection() {
        let (mut tree, _) = postgres_tree(&["a"]);
        tree.expand(row_id(&tree, "Tables (1)"));
        let schema = row_id(&tree, "public");
        tree.select(row_id(&tree, "a"));
        tree.collapse(schema);
        assert_eq!(tree.selected(), Some(schema));
        assert_eq!(tree.rows().last().unwrap().label, "public");
        assert!(!tree.rows().last().unwrap().expanded);
        tree.expand(schema);
        assert!(
            labels(&tree).len() > 4,
            "expanding again shows the cached children"
        );
    }

    #[test]
    fn selection_moves_over_rows_and_the_root() {
        let (mut tree, _) = postgres_tree(&["a"]);
        let root = tree.root();
        tree.select_next();
        assert_eq!(tree.selected(), Some(tree.rows()[0].id));
        tree.select_next();
        assert_eq!(tree.selected(), Some(tree.rows()[1].id));
        tree.select_previous();
        tree.select_previous();
        assert_eq!(tree.selected(), Some(root));
        tree.select_previous();
        assert_eq!(tree.selected(), Some(root));
        tree.select_next();
        assert_eq!(tree.selected(), Some(tree.rows()[0].id));
        let last = tree.rows().last().unwrap().id;
        for _ in 0..20 {
            tree.select_next();
        }
        assert_eq!(tree.selected(), Some(last));
        assert!(!tree.select(TreeNodeId(9999)));
        assert_eq!(tree.selected(), Some(last));
    }

    #[test]
    fn select_previous_with_no_selection_picks_the_last_row() {
        let (mut tree, _) = postgres_tree(&["a"]);
        tree.select_previous();
        assert_eq!(tree.selected(), tree.rows().last().map(|r| r.id));
    }

    #[test]
    fn refresh_scopes() {
        let (mut tree, sp) = postgres_tree(&["a"]);
        assert_eq!(tree.refresh_scope(tree.root()), Some(RefreshScope::All));
        let folder = row_id(&tree, "Tables (1)");
        assert_eq!(
            tree.refresh_scope(folder),
            Some(RefreshScope::Node(ck(&sp, ObjectKind::Table)))
        );
        let schema = row_id(&tree, "public");
        let schema_path = tree.object(schema).unwrap().child_path();
        assert_eq!(
            tree.refresh_scope(schema),
            Some(RefreshScope::Subtree(schema_path))
        );
        tree.expand(folder);
        let a = row_id(&tree, "a");
        let a_obj = table_in(&sp, "a");
        assert_eq!(
            tree.refresh_scope(a),
            Some(RefreshScope::Subtree(a_obj.child_path()))
        );
        tree.expand(a);
        let ap = a_obj.child_path();
        let trg = DbObject::new(ObjectKind::Trigger, ap.clone(), "trg");
        tree.children_loaded(&nk(&ap, ObjectKind::Trigger), objects(vec![trg]));
        let mut t = Table::new(a_obj.clone());
        t.columns = vec![ColumnInfo::new(
            "id",
            sqler_driver_api::sqler_core::DataType::Integer,
        )];
        tree.table_loaded(&a_obj, Ok(Some(Arc::new(t))));
        let triggers = row_id(&tree, "Triggers (1)");
        tree.expand(triggers);
        assert_eq!(
            tree.refresh_scope(row_id(&tree, "trg")),
            Some(RefreshScope::Node(ck(&ap, ObjectKind::Trigger)))
        );
        let cols = row_id(&tree, "Columns (1)");
        assert_eq!(
            tree.refresh_scope(cols),
            Some(RefreshScope::Node(CacheKey::Table(a_obj.clone())))
        );
        tree.expand(cols);
        assert_eq!(
            tree.refresh_scope(row_id(&tree, "id  integer")),
            Some(RefreshScope::Node(CacheKey::Table(a_obj)))
        );
        assert_eq!(tree.refresh_scope(TreeNodeId(777)), None);
    }

    #[test]
    fn progress_shows_on_the_row() {
        let (mut tree, _) = postgres_tree(&["a"]);
        let schema = row_id(&tree, "public");
        tree.set_progress(schema, Some((1, 4)));
        assert_eq!(
            tree.rows()
                .iter()
                .find(|r| r.id == schema)
                .unwrap()
                .progress,
            Some((1, 4))
        );
        tree.set_progress(schema, None);
        assert_eq!(tree.progress(schema), None);
    }

    #[test]
    fn leaf_objects_do_not_expand() {
        let (mut tree, sp) = postgres_tree(&["a"]);
        tree.expand(row_id(&tree, "Tables (1)"));
        let f = DbObject::new(ObjectKind::Function, sp.clone(), "f");
        tree.children_loaded(&nk(&sp, ObjectKind::Function), objects(vec![f]));
        tree.expand(row_id(&tree, "Functions (1)"));
        let f_id = row_id(&tree, "f");
        assert_eq!(tree.expand(f_id).len(), 0);
        assert!(!tree.is_expanded(f_id));
        assert!(
            !tree
                .rows()
                .iter()
                .find(|r| r.id == f_id)
                .unwrap()
                .expandable
        );
        assert_eq!(
            tree.rows().iter().find(|r| r.id == f_id).unwrap().kind,
            Some(ObjectKind::Function)
        );
    }

    #[test]
    fn five_thousand_tables_expand_and_select() {
        let names: Vec<String> = (0..5000).map(|i| format!("t{i:04}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (mut tree, _) = postgres_tree(&refs);
        let folder = row_id(&tree, "Tables (5000)");
        tree.expand(folder);
        assert_eq!(tree.rows().len(), 5 + 5000);
        for _ in 0..3 {
            tree.select_next();
        }
        assert!(tree.selected().is_some());
    }

    #[test]
    fn ids_are_never_reused() {
        let (mut tree, sp) = postgres_tree(&["a"]);
        tree.expand(row_id(&tree, "Tables (1)"));
        let a = row_id(&tree, "a");
        tree.children_loaded(&nk(&sp, ObjectKind::Table), objects(vec![]));
        tree.children_loaded(
            &nk(&sp, ObjectKind::Table),
            objects(vec![table_in(&sp, "a")]),
        );
        assert_ne!(row_id(&tree, "a"), a);
        assert_eq!(tree.kind(a), None);
    }
}
