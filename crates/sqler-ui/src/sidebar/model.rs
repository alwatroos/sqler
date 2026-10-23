// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SidebarModel`: the saved connections in nested groups, the selection and per-connection
//! connect status.
//!
//! The model owns a [`ConnectionStore`] (groups, ids, order) and where to save it. The flat,
//! index-based API of the first shell (`entries`, `select`, `selected`) stays: the index is the
//! position in the store's file order.

use std::collections::{HashMap, HashSet};

use sqler_config::{
    ConfigDir, ConnectionId, ConnectionProfile, ConnectionSpec, ConnectionStore, Group, GroupId,
    StoreError, TlsMode, TlsProfile,
};
use sqler_driver_api::sqler_core::ConnectionConfig;

/// Where a connection stands in the connect flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// No connect attempt is running and the last one (if any) succeeded.
    Idle,
    /// At least one connect attempt is running.
    Connecting,
    /// The last connect attempt failed with this message.
    Failed(String),
}

/// One connection of the sidebar, in file order.
#[derive(Debug, Clone)]
pub struct SidebarEntry {
    id: ConnectionId,
    config: ConnectionConfig,
    pending: usize,
    status: ConnectionStatus,
}

impl SidebarEntry {
    /// The stable id of the connection.
    #[must_use]
    pub fn id(&self) -> &ConnectionId {
        &self.id
    }

    /// The saved connection.
    #[must_use]
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }

    /// The connect status.
    #[must_use]
    pub fn status(&self) -> &ConnectionStatus {
        &self.status
    }
}

/// What is selected in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// A connection.
    Connection(ConnectionId),
    /// A group.
    Group(GroupId),
}

/// What a sidebar row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// A group (folder).
    Group {
        /// The group id.
        id: GroupId,
        /// The group name.
        name: String,
        /// Whether its children are shown.
        expanded: bool,
        /// Whether it holds no group and no connection.
        empty: bool,
    },
    /// A connection; `index` is its position in [`SidebarModel::entries`].
    Connection {
        /// Index into `entries()`.
        index: usize,
    },
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    /// Nesting depth, 0 at the root.
    pub depth: usize,
    /// What the row is.
    pub kind: RowKind,
}

/// One line of the sidebar list: a group or connection row, or a row of a connection's object
/// tree (`row_index` indexes the tree's `rows()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarItem {
    /// A group or connection row.
    Row(SidebarRow),
    /// A visible row of the object tree of `conn`.
    Object {
        /// The connection that owns the tree.
        conn: ConnectionId,
        /// Index into the tree's `rows()`.
        row_index: usize,
    },
}

/// The range of `old` that must be replaced to get `new`, and how many items replace it.
///
/// It is the part between the common prefix and the common suffix, so a list that grew or shrank
/// in the middle keeps the measured heights (and the scroll position) of everything else.
#[must_use]
pub fn changed_range<T: PartialEq>(old: &[T], new: &[T]) -> (std::ops::Range<usize>, usize) {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (prefix..old.len() - suffix, new.len() - prefix - suffix)
}

/// The command a key runs while the sidebar has the focus, if any.
///
/// `key` is the canonical chord key (`up`, `f10`, `menu`, ...). Arrow keys and Enter take no
/// modifier; the context menu opens on Shift+F10 or the Menu key.
#[must_use]
pub fn key_command(key: &str, shift: bool, other_modifiers: bool) -> Option<&'static str> {
    if other_modifiers {
        return None;
    }
    match (key, shift) {
        ("up", false) => Some("object:select-previous"),
        ("down", false) => Some("object:select-next"),
        ("left", false) => Some("object:collapse"),
        ("right", false) => Some("object:expand"),
        ("enter", false) => Some("object:open-data"),
        ("f10", true) | ("menu", _) => Some("object:show-menu"),
        _ => None,
    }
}

/// A group as a choice in a list: its path from the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupOption {
    /// The group id.
    pub id: GroupId,
    /// Names from the root, joined with ` / `.
    pub path: String,
    /// Nesting depth, 0 at the root.
    pub depth: usize,
}

/// The sidebar state: the connection tree, the selection, a load error and where to save.
#[derive(Debug, Clone, Default)]
pub struct SidebarModel {
    store: ConnectionStore,
    entries: Vec<SidebarEntry>,
    collapsed: HashSet<GroupId>,
    selection: Option<Selection>,
    dir: Option<ConfigDir>,
    load_error: Option<String>,
    save_error: Option<String>,
}

/// A spec for a connection known only as a core config (the first-shell constructor).
fn spec_of(config: &ConnectionConfig) -> ConnectionSpec {
    let mut spec = ConnectionSpec::new(
        config.name.clone(),
        config.driver.clone(),
        config.target.clone(),
    );
    if let Some(color) = &config.color {
        spec = spec.with_color(color.clone());
    }
    if let Some(env) = config.environment {
        spec = spec.with_environment(env);
    }
    if let Some(tls) = &config.tls
        && let Some(mode) = TlsMode::from_id(tls.mode.as_str())
    {
        let mut profile = TlsProfile::new(mode);
        profile.ca_cert.clone_from(&tls.ca_cert);
        profile.client_cert.clone_from(&tls.client_cert);
        profile.client_key.clone_from(&tls.client_key);
        spec = spec.with_tls(profile);
    }
    spec
}

impl SidebarModel {
    /// Builds the model from connections in a flat list, keeping their order. Entries the store
    /// rejects (a duplicate name) are left out; nothing is saved.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // the first-shell signature; callers hand over a list
    pub fn from_connections(connections: Vec<ConnectionConfig>) -> Self {
        let mut store = ConnectionStore::new();
        for config in &connections {
            let _ = store.add_connection(spec_of(config), None);
        }
        Self::from_store(store, None)
    }

    /// Builds the model over a loaded store; changes are saved to `dir` (when given).
    #[must_use]
    pub fn from_store(store: ConnectionStore, dir: Option<ConfigDir>) -> Self {
        let mut model = Self {
            store,
            dir,
            ..Self::default()
        };
        model.refresh();
        model
    }

    /// An empty model that shows why the connection list could not be loaded. It is never
    /// saved, so the file the user may be fixing is not overwritten.
    #[must_use]
    pub fn with_load_error(message: String) -> Self {
        Self {
            load_error: Some(message),
            ..Self::default()
        }
    }

    /// Rebuilds `entries` from the store, keeping status by id, and drops a selection that no
    /// longer exists.
    fn refresh(&mut self) {
        let mut old: HashMap<ConnectionId, SidebarEntry> = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        self.entries = self
            .store
            .connections()
            .iter()
            .map(|profile| {
                let (pending, status) = old
                    .remove(profile.id())
                    .map_or((0, ConnectionStatus::Idle), |e| (e.pending, e.status));
                SidebarEntry {
                    id: profile.id().clone(),
                    config: profile.to_connection_config(),
                    pending,
                    status,
                }
            })
            .collect();
        let gone = match &self.selection {
            Some(Selection::Connection(id)) => self.store.connection(id).is_none(),
            Some(Selection::Group(id)) => self.store.group(id).is_none(),
            None => false,
        };
        if gone {
            self.selection = None;
        }
        let groups: HashSet<&GroupId> = self.store.groups().iter().map(Group::id).collect();
        self.collapsed.retain(|id| groups.contains(id));
    }

    /// Runs `f` on the store (an import adds connections and groups), then rebuilds the entries.
    pub(crate) fn import_with<R>(&mut self, f: impl FnOnce(&mut ConnectionStore) -> R) -> R {
        let result = f(&mut self.store);
        self.refresh();
        result
    }

    /// The connection store.
    #[must_use]
    pub fn store(&self) -> &ConnectionStore {
        &self.store
    }

    /// The entries in file order.
    #[must_use]
    pub fn entries(&self) -> &[SidebarEntry] {
        &self.entries
    }

    /// The position of a connection in [`entries`](Self::entries).
    #[must_use]
    pub fn index_of(&self, id: &ConnectionId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == *id)
    }

    /// The saved connection with this id.
    #[must_use]
    pub fn profile(&self, id: &ConnectionId) -> Option<&ConnectionProfile> {
        self.store.connection(id)
    }

    /// The visible rows: groups first (children indented below an expanded group), then the
    /// connections of each level.
    #[must_use]
    pub fn rows(&self) -> Vec<SidebarRow> {
        let index: HashMap<&ConnectionId, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (&e.id, i))
            .collect();
        let mut rows = Vec::new();
        self.walk(None, 0, &index, &mut rows);
        rows
    }

    fn walk(
        &self,
        parent: Option<&GroupId>,
        depth: usize,
        index: &HashMap<&ConnectionId, usize>,
        rows: &mut Vec<SidebarRow>,
    ) {
        for group in self.store.child_groups(parent) {
            let expanded = !self.collapsed.contains(group.id());
            let empty = self.store.child_groups(Some(group.id())).next().is_none()
                && self
                    .store
                    .child_connections(Some(group.id()))
                    .next()
                    .is_none();
            rows.push(SidebarRow {
                depth,
                kind: RowKind::Group {
                    id: group.id().clone(),
                    name: group.name().to_owned(),
                    expanded,
                    empty,
                },
            });
            if expanded {
                self.walk(Some(group.id()), depth + 1, index, rows);
            }
        }
        for profile in self.store.child_connections(parent) {
            if let Some(&i) = index.get(profile.id()) {
                rows.push(SidebarRow {
                    depth,
                    kind: RowKind::Connection { index: i },
                });
            }
        }
    }

    /// The list the sidebar shows: [`rows`](Self::rows), each connection row followed by the
    /// `object_rows(connection)` visible rows of its object tree.
    #[must_use]
    pub fn items(&self, object_rows: impl Fn(&ConnectionId) -> usize) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        for row in self.rows() {
            let conn = match &row.kind {
                RowKind::Connection { index } => self.entries.get(*index).map(|e| e.id.clone()),
                RowKind::Group { .. } => None,
            };
            items.push(SidebarItem::Row(row));
            if let Some(conn) = conn {
                for row_index in 0..object_rows(&conn) {
                    items.push(SidebarItem::Object {
                        conn: conn.clone(),
                        row_index,
                    });
                }
            }
        }
        items
    }

    /// Every group with its path, depth first (for pickers).
    #[must_use]
    pub fn group_options(&self) -> Vec<GroupOption> {
        fn collect(
            store: &ConnectionStore,
            parent: Option<&GroupId>,
            prefix: &str,
            depth: usize,
            out: &mut Vec<GroupOption>,
        ) {
            for group in store.child_groups(parent) {
                let path = if prefix.is_empty() {
                    group.name().to_owned()
                } else {
                    format!("{prefix} / {}", group.name())
                };
                out.push(GroupOption {
                    id: group.id().clone(),
                    path: path.clone(),
                    depth,
                });
                collect(store, Some(group.id()), &path, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        collect(&self.store, None, "", 0, &mut out);
        out
    }

    /// The selection.
    #[must_use]
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// The selected connection's index.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        match &self.selection {
            Some(Selection::Connection(id)) => self.index_of(id),
            _ => None,
        }
    }

    /// The selected connection's id.
    #[must_use]
    pub fn selected_id(&self) -> Option<&ConnectionId> {
        match &self.selection {
            Some(Selection::Connection(id)) => Some(id),
            _ => None,
        }
    }

    /// The selected group.
    #[must_use]
    pub fn selected_group(&self) -> Option<&GroupId> {
        match &self.selection {
            Some(Selection::Group(id)) => Some(id),
            _ => None,
        }
    }

    /// The error from loading the connection list, if any.
    #[must_use]
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// The error from the last save, if it failed.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Whether changes are written to disk (a config directory exists and the file loaded).
    #[must_use]
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    /// Selects a connection by index; returns `false` (and changes nothing) when out of range.
    pub fn select(&mut self, index: usize) -> bool {
        match self.entries.get(index) {
            Some(entry) => {
                self.selection = Some(Selection::Connection(entry.id.clone()));
                true
            }
            None => false,
        }
    }

    /// Selects a connection by id; returns `false` when it does not exist.
    pub fn select_connection(&mut self, id: &ConnectionId) -> bool {
        if self.store.connection(id).is_some() {
            self.selection = Some(Selection::Connection(id.clone()));
            true
        } else {
            false
        }
    }

    /// Selects a group; returns `false` when it does not exist.
    pub fn select_group(&mut self, id: &GroupId) -> bool {
        if self.store.group(id).is_some() {
            self.selection = Some(Selection::Group(id.clone()));
            true
        } else {
            false
        }
    }

    /// Clears the selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Whether the group shows its children.
    #[must_use]
    pub fn is_expanded(&self, id: &GroupId) -> bool {
        !self.collapsed.contains(id)
    }

    /// Expands a collapsed group and collapses an expanded one; `false` for an unknown group.
    pub fn toggle_group(&mut self, id: &GroupId) -> bool {
        if self.store.group(id).is_none() {
            return false;
        }
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.clone());
        }
        true
    }

    fn expand_chain(&mut self, mut group: Option<GroupId>) {
        while let Some(id) = group {
            self.collapsed.remove(&id);
            group = self.store.group(&id).and_then(|g| g.parent().cloned());
        }
    }

    /// Adds a connection at the end of `group` and selects it.
    ///
    /// # Errors
    /// The store's rejection of the spec or the group.
    pub fn add_connection(
        &mut self,
        spec: ConnectionSpec,
        group: Option<&GroupId>,
    ) -> Result<ConnectionId, StoreError> {
        let id = self.store.add_connection(spec, group)?;
        self.expand_chain(group.cloned());
        self.refresh();
        self.selection = Some(Selection::Connection(id.clone()));
        Ok(id)
    }

    /// Replaces the settings of a connection.
    ///
    /// # Errors
    /// The store's rejection of the spec.
    pub fn update_connection(
        &mut self,
        id: &ConnectionId,
        spec: ConnectionSpec,
    ) -> Result<(), StoreError> {
        self.store.update_connection(id, spec)?;
        self.refresh();
        Ok(())
    }

    /// Removes a connection and returns it (the caller deletes its keychain entries).
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`].
    pub fn delete_connection(
        &mut self,
        id: &ConnectionId,
    ) -> Result<ConnectionProfile, StoreError> {
        let profile = self.store.delete_connection(id)?;
        self.refresh();
        Ok(profile)
    }

    /// Moves a connection into a group (`None` = root) at a position among its siblings.
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`] or [`StoreError::UnknownGroup`].
    pub fn move_connection(
        &mut self,
        id: &ConnectionId,
        to: Option<&GroupId>,
        index: usize,
    ) -> Result<(), StoreError> {
        self.store.move_connection(id, to, index)?;
        self.expand_chain(to.cloned());
        self.refresh();
        Ok(())
    }

    /// Adds a group and selects it.
    ///
    /// # Errors
    /// The store's rejection of the name or the parent.
    pub fn add_group(
        &mut self,
        name: &str,
        parent: Option<&GroupId>,
    ) -> Result<GroupId, StoreError> {
        let id = self.store.add_group(name, parent)?;
        self.expand_chain(parent.cloned());
        self.refresh();
        self.selection = Some(Selection::Group(id.clone()));
        Ok(id)
    }

    /// Renames a group.
    ///
    /// # Errors
    /// The store's rejection of the name.
    pub fn rename_group(&mut self, id: &GroupId, name: &str) -> Result<(), StoreError> {
        self.store.rename_group(id, name)?;
        self.refresh();
        Ok(())
    }

    /// Removes an empty group.
    ///
    /// # Errors
    /// [`StoreError::GroupNotEmpty`] or [`StoreError::UnknownGroup`].
    pub fn delete_group(&mut self, id: &GroupId) -> Result<Group, StoreError> {
        let group = self.store.delete_group(id)?;
        self.refresh();
        Ok(group)
    }

    /// Saves the connections. Without a config directory (or after a load error) this does
    /// nothing and succeeds. A failure is kept as [`save_error`](Self::save_error).
    ///
    /// # Errors
    /// The write error as text.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        match dir.save_connection_store(&self.store) {
            Ok(()) => {
                self.save_error = None;
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                self.save_error = Some(message.clone());
                Err(message)
            }
        }
    }

    /// Marks one more connect attempt as running for `index`.
    pub(crate) fn begin_connect(&mut self, index: usize) -> bool {
        match self.entries.get_mut(index) {
            Some(entry) => {
                entry.pending += 1;
                entry.status = ConnectionStatus::Connecting;
                true
            }
            None => false,
        }
    }

    /// Records the end of one connect attempt for `index`.
    pub(crate) fn finish_connect(&mut self, index: usize, result: Result<(), String>) {
        if let Some(entry) = self.entries.get_mut(index) {
            entry.pending = entry.pending.saturating_sub(1);
            // Status is `Connecting` while any attempt is still running.
            if entry.pending == 0 {
                entry.status = match result {
                    Ok(()) => ConnectionStatus::Idle,
                    Err(msg) => ConnectionStatus::Failed(msg),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;

    fn cfg(name: &str) -> ConnectionConfig {
        ConnectionConfig::new(name, DriverId::new("sqlite").unwrap(), ":memory:")
    }

    fn model() -> SidebarModel {
        SidebarModel::from_connections(vec![cfg("b"), cfg("a")])
    }

    #[test]
    fn items_put_object_rows_under_their_connection() {
        let m = model();
        let b = m.entries()[0].id().clone();
        let items = m.items(|id| if *id == b { 3 } else { 0 });
        assert_eq!(items.len(), 2 + 3);
        assert!(matches!(
            &items[0],
            SidebarItem::Row(SidebarRow {
                kind: RowKind::Connection { index: 0 },
                ..
            })
        ));
        for (i, item) in items[1..4].iter().enumerate() {
            assert_eq!(
                item,
                &SidebarItem::Object {
                    conn: b.clone(),
                    row_index: i
                }
            );
        }
        assert!(matches!(
            &items[4],
            SidebarItem::Row(SidebarRow {
                kind: RowKind::Connection { index: 1 },
                ..
            })
        ));
    }

    #[test]
    fn items_of_five_thousand_rows_are_small() {
        let m = model();
        let b = m.entries()[0].id().clone();
        let items = m.items(|id| if *id == b { 5000 } else { 0 });
        assert_eq!(items.len(), 5002);
    }

    #[test]
    fn changed_range_keeps_the_common_ends() {
        assert_eq!(changed_range(&[1, 2, 3], &[1, 2, 3]), (3..3, 0));
        assert_eq!(changed_range(&[1, 2, 3], &[1, 9, 9, 3]), (1..2, 2));
        assert_eq!(changed_range(&[1, 4, 5, 3], &[1, 3]), (1..3, 0));
        assert_eq!(changed_range::<u8>(&[], &[1, 2]), (0..0, 2));
        assert_eq!(changed_range(&[1, 2], &[]), (0..2, 0));
        assert_eq!(changed_range(&[1, 1], &[1, 1, 1]), (2..2, 1));
    }

    #[test]
    fn sidebar_keys_map_to_object_commands() {
        let cases = [
            ("up", false, "object:select-previous"),
            ("down", false, "object:select-next"),
            ("left", false, "object:collapse"),
            ("right", false, "object:expand"),
            ("enter", false, "object:open-data"),
            ("f10", true, "object:show-menu"),
            ("menu", false, "object:show-menu"),
        ];
        for (key, shift, id) in cases {
            assert_eq!(key_command(key, shift, false), Some(id), "{key}");
        }
        assert_eq!(key_command("f10", false, false), None);
        assert_eq!(key_command("up", true, false), None);
        assert_eq!(key_command("up", false, true), None);
        assert_eq!(key_command("a", false, false), None);
    }

    #[test]
    fn keeps_file_order() {
        let m = model();
        let names: Vec<_> = m
            .entries()
            .iter()
            .map(|e| e.config().name.as_str())
            .collect();
        assert_eq!(names, ["b", "a"]);
        assert_eq!(m.selected(), None);
        assert_eq!(m.load_error(), None);
    }

    #[test]
    fn select_checks_bounds() {
        let mut m = model();
        assert!(m.select(1));
        assert_eq!(m.selected(), Some(1));
        assert!(!m.select(2));
        assert_eq!(m.selected(), Some(1));
    }

    #[test]
    fn status_transitions() {
        let mut m = model();
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
        assert!(m.begin_connect(0));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Err("boom".into()));
        assert_eq!(
            *m.entries()[0].status(),
            ConnectionStatus::Failed("boom".into())
        );
        assert!(m.begin_connect(0));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
        assert_eq!(*m.entries()[1].status(), ConnectionStatus::Idle);
    }

    #[test]
    fn pending_counter_keeps_connecting() {
        let mut m = model();
        m.begin_connect(0);
        m.begin_connect(0);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Connecting);
        m.finish_connect(0, Ok(()));
        assert_eq!(*m.entries()[0].status(), ConnectionStatus::Idle);
    }

    #[test]
    fn out_of_range_connect_is_ignored() {
        let mut m = model();
        assert!(!m.begin_connect(9));
        m.finish_connect(9, Ok(()));
    }

    #[test]
    fn load_error_is_kept() {
        let m = SidebarModel::with_load_error("bad file".into());
        assert_eq!(m.load_error(), Some("bad file"));
        assert!(m.entries().is_empty());
    }

    fn unknown_group() -> GroupId {
        ConnectionStore::new().add_group("nope", None).unwrap()
    }

    fn unknown_connection() -> ConnectionId {
        ConnectionStore::new()
            .add_connection(spec("nope"), None)
            .unwrap()
    }

    fn spec(name: &str) -> ConnectionSpec {
        ConnectionSpec::new(name, DriverId::new("sqlite").unwrap(), ":memory:")
    }

    /// root: [g1: [g2: [g3: [deep]], inner], top]
    fn tree() -> (SidebarModel, GroupId, GroupId, GroupId) {
        let mut m = SidebarModel::default();
        let g1 = m.add_group("g1", None).unwrap();
        let g2 = m.add_group("g2", Some(&g1)).unwrap();
        let g3 = m.add_group("g3", Some(&g2)).unwrap();
        m.add_connection(spec("deep"), Some(&g3)).unwrap();
        m.add_connection(spec("inner"), Some(&g1)).unwrap();
        m.add_connection(spec("top"), None).unwrap();
        (m, g1, g2, g3)
    }

    fn shape(m: &SidebarModel) -> Vec<(usize, String)> {
        m.rows()
            .into_iter()
            .map(|r| {
                let label = match r.kind {
                    RowKind::Group { name, .. } => format!("[{name}]"),
                    RowKind::Connection { index } => m.entries()[index].config().name.clone(),
                };
                (r.depth, label)
            })
            .collect()
    }

    #[test]
    fn rows_nest_three_deep_groups_first() {
        let (m, ..) = tree();
        let expected: Vec<(usize, String)> = [
            (0, "[g1]"),
            (1, "[g2]"),
            (2, "[g3]"),
            (3, "deep"),
            (1, "inner"),
            (0, "top"),
        ]
        .into_iter()
        .map(|(d, s)| (d, s.to_owned()))
        .collect();
        assert_eq!(shape(&m), expected);
    }

    #[test]
    fn collapsing_hides_children_and_expand_restores() {
        let (mut m, g1, g2, _) = tree();
        assert!(m.is_expanded(&g1));
        assert!(m.toggle_group(&g2));
        assert!(!m.is_expanded(&g2));
        let labels: Vec<String> = shape(&m).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["[g1]", "[g2]", "inner", "top"]);
        assert!(m.toggle_group(&g1));
        let labels: Vec<String> = shape(&m).into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["[g1]", "top"]);
        assert!(m.toggle_group(&g1));
        assert!(m.toggle_group(&g2));
        assert_eq!(shape(&m).len(), 6);
        let missing = unknown_group();
        assert!(!m.toggle_group(&missing));
    }

    #[test]
    fn group_rows_report_empty_and_expanded() {
        let mut m = SidebarModel::default();
        let g = m.add_group("e", None).unwrap();
        let RowKind::Group {
            empty, expanded, ..
        } = m.rows()[0].kind.clone()
        else {
            panic!("expected a group row");
        };
        assert!(empty && expanded);
        m.add_connection(spec("c"), Some(&g)).unwrap();
        let RowKind::Group { empty, .. } = m.rows()[0].kind.clone() else {
            panic!("expected a group row");
        };
        assert!(!empty);
    }

    #[test]
    fn row_index_matches_entries_in_file_order() {
        let (m, ..) = tree();
        let names: Vec<_> = m
            .entries()
            .iter()
            .map(|e| e.config().name.as_str())
            .collect();
        assert_eq!(names, ["deep", "inner", "top"]);
        for row in m.rows() {
            if let RowKind::Connection { index } = row.kind {
                assert!(index < m.entries().len());
            }
        }
    }

    #[test]
    fn selection_compat_and_groups() {
        let (mut m, g1, ..) = tree();
        // Adding selected the last added connection.
        assert_eq!(m.selected(), Some(2));
        assert!(m.select(0));
        assert_eq!(m.selected(), Some(0));
        assert_eq!(m.selected_group(), None);
        assert!(m.select_group(&g1));
        assert_eq!(m.selected(), None);
        assert_eq!(m.selected_group(), Some(&g1));
        assert!(!m.select_group(&unknown_group()));
        assert_eq!(m.selected_group(), Some(&g1));
        m.clear_selection();
        assert_eq!(m.selection(), None);
    }

    #[test]
    fn status_follows_the_id_across_edits_and_moves() {
        let (mut m, g1, ..) = tree();
        let id = m.entries()[2].id().clone();
        assert!(m.begin_connect(2));
        m.move_connection(&id, Some(&g1), 0).unwrap();
        let idx = m.index_of(&id).unwrap();
        assert_eq!(*m.entries()[idx].status(), ConnectionStatus::Connecting);
        m.update_connection(&id, spec("renamed")).unwrap();
        let idx = m.index_of(&id).unwrap();
        assert_eq!(m.entries()[idx].config().name, "renamed");
        assert_eq!(*m.entries()[idx].status(), ConnectionStatus::Connecting);
    }

    #[test]
    fn mutators_surface_store_errors() {
        let (mut m, g1, ..) = tree();
        assert_eq!(
            m.add_connection(spec("top"), None),
            Err(StoreError::DuplicateConnectionName("top".into()))
        );
        assert_eq!(
            m.add_group("g1", None),
            Err(StoreError::DuplicateGroupName("g1".into()))
        );
        assert_eq!(
            m.delete_group(&g1).map(|_| ()),
            Err(StoreError::GroupNotEmpty(g1.clone()))
        );
        assert_eq!(m.rename_group(&g1, " "), Err(StoreError::EmptyName));
        let unknown = unknown_connection();
        assert!(m.move_connection(&unknown, None, 0).is_err());
        assert!(m.delete_connection(&unknown).is_err());
    }

    #[test]
    fn delete_returns_the_profile_and_drops_the_selection() {
        let (mut m, ..) = tree();
        let id = m.selected_id().unwrap().clone();
        let profile = m.delete_connection(&id).unwrap();
        assert_eq!(profile.id(), &id);
        assert_eq!(m.selection(), None);
        assert_eq!(m.entries().len(), 2);
        assert!(m.index_of(&id).is_none());
    }

    #[test]
    fn rename_and_delete_group() {
        let mut m = SidebarModel::default();
        let g = m.add_group("a", None).unwrap();
        m.rename_group(&g, "b").unwrap();
        assert_eq!(m.group_options()[0].path, "b");
        m.delete_group(&g).unwrap();
        assert_eq!(m.rows().len(), 0);
        assert_eq!(m.selection(), None);
    }

    #[test]
    fn moving_into_a_collapsed_group_expands_it() {
        let (mut m, g1, ..) = tree();
        m.toggle_group(&g1);
        let id = m.entries()[2].id().clone();
        m.move_connection(&id, Some(&g1), 0).unwrap();
        assert!(m.is_expanded(&g1));
    }

    #[test]
    fn group_options_have_paths_and_depth() {
        let (m, ..) = tree();
        let options = m.group_options();
        let paths: Vec<_> = options.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(paths, ["g1", "g1 / g2", "g1 / g2 / g3"]);
        let depths: Vec<_> = options.iter().map(|o| o.depth).collect();
        assert_eq!(depths, [0, 1, 2]);
    }

    #[test]
    fn from_connections_keeps_color_environment_and_tls() {
        use sqler_driver_api::sqler_core::{Environment, TlsConfig, TlsMode as CoreTls};
        let config = ConnectionConfig::new("p", DriverId::new("postgres").unwrap(), "x")
            .with_color("#00ff00")
            .with_environment(Environment::Prod)
            .with_tls(TlsConfig::new(CoreTls::VerifyCa).with_ca_cert("/ca"));
        let m = SidebarModel::from_connections(vec![config.clone()]);
        assert_eq!(m.entries()[0].config(), &config);
    }

    #[test]
    fn save_without_a_directory_is_a_noop() {
        let (mut m, ..) = tree();
        assert!(!m.persistent());
        assert_eq!(m.save(), Ok(()));
        assert_eq!(m.save_error(), None);
    }

    #[test]
    fn save_writes_to_the_directory_and_reports_failure() {
        let dir = std::env::temp_dir().join(format!("sqler-sidebar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = ConfigDir::new(&dir);
        let mut m = SidebarModel::from_store(ConnectionStore::new(), Some(cfg.clone()));
        assert!(m.persistent());
        m.add_connection(spec("saved"), None).unwrap();
        assert_eq!(m.save(), Ok(()));
        let loaded = cfg.load_connection_store().unwrap();
        assert_eq!(loaded.connections()[0].spec().name, "saved");
        // A file where the directory should be makes the next save fail.
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
        assert!(m.save().is_err());
        assert!(m.save_error().is_some());
        std::fs::remove_file(&dir).unwrap();
        assert_eq!(m.save(), Ok(()));
        assert_eq!(m.save_error(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_load_error_model_is_never_saved() {
        let mut m = SidebarModel::with_load_error("bad".into());
        assert!(!m.persistent());
        assert_eq!(m.save(), Ok(()));
    }
}
