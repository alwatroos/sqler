// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The editable connection store: groups and connections with typed errors.

use sqler_core::ConnectionConfig;
use thiserror::Error;

use super::model::{
    ConnectionId, ConnectionProfile, ConnectionSpec, Group, GroupId, unique_slug, validate_spec,
};

/// Why a store operation was rejected. Never echoes a secret.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A name is empty or only whitespace.
    #[error("name must not be empty")]
    EmptyName,
    /// Another connection already has this name.
    #[error("duplicate connection name {0:?}")]
    DuplicateConnectionName(String),
    /// A sibling group already has this name.
    #[error("duplicate group name {0:?} among siblings")]
    DuplicateGroupName(String),
    /// No connection has this id.
    #[error("unknown connection {0}")]
    UnknownConnection(ConnectionId),
    /// No group has this id.
    #[error("unknown group {0}")]
    UnknownGroup(GroupId),
    /// The group still holds groups or connections.
    #[error("group {0} is not empty")]
    GroupNotEmpty(GroupId),
    /// A group cannot be moved under itself or one of its descendants.
    #[error("moving group {0} there would make it its own ancestor")]
    GroupCycle(GroupId),
    /// The colour is not `#rrggbb`.
    #[error("color {0:?} must look like #rrggbb")]
    InvalidColor(String),
    /// The target embeds a secret.
    #[error(
        "target contains a secret; secrets are not stored in config files, store the password in the keychain"
    )]
    TargetHasSecret,
    /// The TLS settings are inconsistent.
    #[error("invalid TLS settings: {0}")]
    InvalidTls(String),
    /// The SSH settings are inconsistent.
    #[error("invalid SSH settings: {0}")]
    InvalidSsh(String),
}

/// Groups and connections in user-visible order.
///
/// Sibling order is the order within the underlying lists restricted to one
/// parent. Index arguments are positions among the destination's siblings and
/// are clamped to the end.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConnectionStore {
    groups: Vec<Group>,
    connections: Vec<ConnectionProfile>,
}

/// Inserts `item` so that it becomes sibling number `index` (clamped) among
/// the elements for which `same` holds.
fn insert_sibling<T>(items: &mut Vec<T>, item: T, index: usize, same: impl Fn(&T) -> bool) {
    let pos = items
        .iter()
        .enumerate()
        .filter(|(_, x)| same(x))
        .nth(index)
        .map_or(items.len(), |(i, _)| i);
    items.insert(pos, item);
}

impl ConnectionStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a store from parts already validated by the file loader.
    pub(crate) fn from_parts(groups: Vec<Group>, connections: Vec<ConnectionProfile>) -> Self {
        Self {
            groups,
            connections,
        }
    }

    /// All groups in file order.
    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// All connections in file order.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionProfile] {
        &self.connections
    }

    /// The group with this id.
    #[must_use]
    pub fn group(&self, id: &GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id() == id)
    }

    /// The connection with this id.
    #[must_use]
    pub fn connection(&self, id: &ConnectionId) -> Option<&ConnectionProfile> {
        self.connections.iter().find(|c| c.id() == id)
    }

    /// Groups directly inside `parent` (`None` = root), in order.
    pub fn child_groups<'a>(
        &'a self,
        parent: Option<&'a GroupId>,
    ) -> impl Iterator<Item = &'a Group> + 'a {
        self.groups.iter().filter(move |g| g.parent() == parent)
    }

    /// Connections directly inside `group` (`None` = root), in order.
    pub fn child_connections<'a>(
        &'a self,
        group: Option<&'a GroupId>,
    ) -> impl Iterator<Item = &'a ConnectionProfile> + 'a {
        self.connections.iter().filter(move |c| c.group() == group)
    }

    /// Adds a connection at the end of `group` (`None` = root).
    ///
    /// # Errors
    /// [`StoreError::UnknownGroup`], a spec error ([`StoreError::EmptyName`],
    /// [`StoreError::InvalidColor`], [`StoreError::TargetHasSecret`],
    /// [`StoreError::InvalidTls`], [`StoreError::InvalidSsh`]) or
    /// [`StoreError::DuplicateConnectionName`].
    pub fn add_connection(
        &mut self,
        spec: ConnectionSpec,
        group: Option<&GroupId>,
    ) -> Result<ConnectionId, StoreError> {
        self.require_group(group)?;
        let spec = validate_spec(spec)?;
        self.check_connection_name(&spec.name, None)?;
        let id = ConnectionId::from_valid(unique_slug(&spec.name, "connection", |s| {
            self.connections.iter().any(|c| c.id().as_str() == s)
        }));
        self.connections
            .push(ConnectionProfile::new(id.clone(), group.cloned(), spec));
        Ok(id)
    }

    /// Replaces the settings of a connection; id, group and position stay.
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`], a spec error or
    /// [`StoreError::DuplicateConnectionName`].
    pub fn update_connection(
        &mut self,
        id: &ConnectionId,
        spec: ConnectionSpec,
    ) -> Result<(), StoreError> {
        let pos = self.connection_pos(id)?;
        let spec = validate_spec(spec)?;
        self.check_connection_name(&spec.name, Some(id))?;
        self.connections[pos].set_spec(spec);
        Ok(())
    }

    /// Renames a connection; the id and its keychain references stay.
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`], [`StoreError::EmptyName`] or
    /// [`StoreError::DuplicateConnectionName`].
    pub fn rename_connection(&mut self, id: &ConnectionId, name: &str) -> Result<(), StoreError> {
        let pos = self.connection_pos(id)?;
        let mut spec = self.connections[pos].spec().clone();
        name.clone_into(&mut spec.name);
        self.update_connection(id, spec)
    }

    /// Removes a connection and returns it; the caller deletes its
    /// [`ConnectionProfile::secret_refs`] from the keychain.
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`].
    pub fn delete_connection(
        &mut self,
        id: &ConnectionId,
    ) -> Result<ConnectionProfile, StoreError> {
        let pos = self.connection_pos(id)?;
        Ok(self.connections.remove(pos))
    }

    /// Moves a connection into `to` (`None` = root) as sibling number `index`.
    ///
    /// # Errors
    /// [`StoreError::UnknownConnection`] or [`StoreError::UnknownGroup`].
    pub fn move_connection(
        &mut self,
        id: &ConnectionId,
        to: Option<&GroupId>,
        index: usize,
    ) -> Result<(), StoreError> {
        let pos = self.connection_pos(id)?;
        self.require_group(to)?;
        let mut profile = self.connections.remove(pos);
        profile.set_group(to.cloned());
        insert_sibling(&mut self.connections, profile, index, |c| c.group() == to);
        Ok(())
    }

    /// Adds a group at the end of `parent` (`None` = root).
    ///
    /// # Errors
    /// [`StoreError::EmptyName`], [`StoreError::UnknownGroup`] or
    /// [`StoreError::DuplicateGroupName`].
    pub fn add_group(
        &mut self,
        name: &str,
        parent: Option<&GroupId>,
    ) -> Result<GroupId, StoreError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::EmptyName);
        }
        self.require_group(parent)?;
        self.check_group_name(name, parent, None)?;
        let id = GroupId::from_valid(unique_slug(name, "group", |s| {
            self.groups.iter().any(|g| g.id().as_str() == s)
        }));
        self.groups
            .push(Group::new(id.clone(), name.to_owned(), parent.cloned()));
        Ok(id)
    }

    /// Renames a group; the id stays.
    ///
    /// # Errors
    /// [`StoreError::UnknownGroup`], [`StoreError::EmptyName`] or
    /// [`StoreError::DuplicateGroupName`].
    pub fn rename_group(&mut self, id: &GroupId, name: &str) -> Result<(), StoreError> {
        let pos = self.group_pos(id)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::EmptyName);
        }
        let parent = self.groups[pos].parent().cloned();
        self.check_group_name(name, parent.as_ref(), Some(id))?;
        self.groups[pos].set_name(name.to_owned());
        Ok(())
    }

    /// Removes an empty group and returns it.
    ///
    /// # Errors
    /// [`StoreError::UnknownGroup`] or [`StoreError::GroupNotEmpty`].
    pub fn delete_group(&mut self, id: &GroupId) -> Result<Group, StoreError> {
        let pos = self.group_pos(id)?;
        let has_children = self.groups.iter().any(|g| g.parent() == Some(id))
            || self.connections.iter().any(|c| c.group() == Some(id));
        if has_children {
            return Err(StoreError::GroupNotEmpty(id.clone()));
        }
        Ok(self.groups.remove(pos))
    }

    /// Moves a group under `parent` (`None` = root) as sibling number `index`.
    ///
    /// # Errors
    /// [`StoreError::UnknownGroup`], [`StoreError::GroupCycle`] when `parent`
    /// is the group itself or one of its descendants, or
    /// [`StoreError::DuplicateGroupName`] at the destination.
    pub fn move_group(
        &mut self,
        id: &GroupId,
        parent: Option<&GroupId>,
        index: usize,
    ) -> Result<(), StoreError> {
        let pos = self.group_pos(id)?;
        self.require_group(parent)?;
        let mut cursor = parent;
        while let Some(p) = cursor {
            if p == id {
                return Err(StoreError::GroupCycle(id.clone()));
            }
            cursor = self.group(p).and_then(Group::parent);
        }
        let name = self.groups[pos].name().to_owned();
        self.check_group_name(&name, parent, Some(id))?;
        let mut group = self.groups.remove(pos);
        group.set_parent(parent.cloned());
        insert_sibling(&mut self.groups, group, index, |g| g.parent() == parent);
        Ok(())
    }

    /// The `sqler-core` configurations in file order.
    #[must_use]
    pub fn to_connection_configs(&self) -> Vec<ConnectionConfig> {
        self.connections
            .iter()
            .map(ConnectionProfile::to_connection_config)
            .collect()
    }

    fn connection_pos(&self, id: &ConnectionId) -> Result<usize, StoreError> {
        self.connections
            .iter()
            .position(|c| c.id() == id)
            .ok_or_else(|| StoreError::UnknownConnection(id.clone()))
    }

    fn group_pos(&self, id: &GroupId) -> Result<usize, StoreError> {
        self.groups
            .iter()
            .position(|g| g.id() == id)
            .ok_or_else(|| StoreError::UnknownGroup(id.clone()))
    }

    fn require_group(&self, id: Option<&GroupId>) -> Result<(), StoreError> {
        id.map_or(Ok(()), |id| self.group_pos(id).map(|_| ()))
    }

    fn check_connection_name(
        &self,
        name: &str,
        except: Option<&ConnectionId>,
    ) -> Result<(), StoreError> {
        let dup = self
            .connections
            .iter()
            .any(|c| Some(c.id()) != except && c.spec().name == name);
        if dup {
            Err(StoreError::DuplicateConnectionName(name.to_owned()))
        } else {
            Ok(())
        }
    }

    fn check_group_name(
        &self,
        name: &str,
        parent: Option<&GroupId>,
        except: Option<&GroupId>,
    ) -> Result<(), StoreError> {
        let dup = self
            .groups
            .iter()
            .any(|g| Some(g.id()) != except && g.parent() == parent && g.name() == name);
        if dup {
            Err(StoreError::DuplicateGroupName(name.to_owned()))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_core::DriverId;

    fn spec(name: &str) -> ConnectionSpec {
        ConnectionSpec::new(name, DriverId::new("sqlite").unwrap(), "app.db")
    }

    fn names(s: &ConnectionStore, g: Option<&GroupId>) -> Vec<String> {
        s.child_connections(g)
            .map(|c| c.spec().name.clone())
            .collect()
    }

    fn group_names(s: &ConnectionStore, g: Option<&GroupId>) -> Vec<String> {
        s.child_groups(g).map(|g| g.name().to_owned()).collect()
    }

    #[test]
    fn add_connection_and_lookup() {
        let mut s = ConnectionStore::new();
        let a = s.add_connection(spec("My DB"), None).unwrap();
        let b = s.add_connection(spec("My-DB!"), None).unwrap();
        assert_eq!(a.as_str(), "my-db");
        assert_eq!(b.as_str(), "my-db-2");
        assert_eq!(s.connection(&a).unwrap().spec().name, "My DB");
        assert_eq!(s.connections().len(), 2);
        assert_eq!(names(&s, None), ["My DB", "My-DB!"]);
    }

    #[test]
    fn add_connection_errors() {
        let mut s = ConnectionStore::new();
        s.add_connection(spec("a"), None).unwrap();
        assert_eq!(
            s.add_connection(spec("a"), None),
            Err(StoreError::DuplicateConnectionName("a".into()))
        );
        assert_eq!(
            s.add_connection(spec(" "), None),
            Err(StoreError::EmptyName)
        );
        let g = GroupId::from_valid("nope".into());
        assert_eq!(
            s.add_connection(spec("b"), Some(&g)),
            Err(StoreError::UnknownGroup(g))
        );
        assert!(matches!(
            s.add_connection(spec("c").with_color("x"), None),
            Err(StoreError::InvalidColor(_))
        ));
        let pg = DriverId::new("postgres").unwrap();
        assert_eq!(
            s.add_connection(ConnectionSpec::new("d", pg, "postgres://u:p@h/d"), None),
            Err(StoreError::TargetHasSecret)
        );
        assert_eq!(s.connections().len(), 1);
    }

    #[test]
    fn update_keeps_id_group_and_position() {
        let mut s = ConnectionStore::new();
        let g = s.add_group("G", None).unwrap();
        s.add_connection(spec("first"), Some(&g)).unwrap();
        let id = s.add_connection(spec("second"), Some(&g)).unwrap();
        s.add_connection(spec("third"), Some(&g)).unwrap();
        s.update_connection(&id, spec("renamed").with_color("#ABCDEF"))
            .unwrap();
        let p = s.connection(&id).unwrap();
        assert_eq!(p.id(), &id);
        assert_eq!(p.group(), Some(&g));
        assert_eq!(p.spec().color.as_deref(), Some("#abcdef"));
        assert_eq!(names(&s, Some(&g)), ["first", "renamed", "third"]);
        // Same name as itself is fine, as another's is not.
        s.update_connection(&id, spec("renamed")).unwrap();
        assert_eq!(
            s.update_connection(&id, spec("first")),
            Err(StoreError::DuplicateConnectionName("first".into()))
        );
        let missing = ConnectionId::from_valid("zzz".into());
        assert_eq!(
            s.update_connection(&missing, spec("x")),
            Err(StoreError::UnknownConnection(missing))
        );
    }

    #[test]
    fn rename_keeps_id_and_secret_refs() {
        let mut s = ConnectionStore::new();
        let id = s
            .add_connection(spec("old").with_password_in_keychain(true), None)
            .unwrap();
        let before = s.connection(&id).unwrap().secret_refs();
        s.rename_connection(&id, "  new name ").unwrap();
        let p = s.connection(&id).unwrap();
        assert_eq!(p.spec().name, "new name");
        assert_eq!(p.id(), &id);
        assert_eq!(p.secret_refs(), before);
        assert_eq!(s.rename_connection(&id, " "), Err(StoreError::EmptyName));
        s.add_connection(spec("other"), None).unwrap();
        assert_eq!(
            s.rename_connection(&id, "other"),
            Err(StoreError::DuplicateConnectionName("other".into()))
        );
    }

    #[test]
    fn delete_connection_returns_profile() {
        let mut s = ConnectionStore::new();
        let id = s
            .add_connection(spec("a").with_password_in_keychain(true), None)
            .unwrap();
        let gone = s.delete_connection(&id).unwrap();
        assert_eq!(gone.secret_refs().len(), 1);
        assert_eq!(s.connections().len(), 0);
        assert_eq!(
            s.delete_connection(&id),
            Err(StoreError::UnknownConnection(id))
        );
        // The id slot is free again.
        assert_eq!(s.add_connection(spec("a"), None).unwrap().as_str(), "a");
    }

    #[test]
    fn move_connection_within_and_between_groups_with_clamping() {
        let mut s = ConnectionStore::new();
        let g = s.add_group("G", None).unwrap();
        let a = s.add_connection(spec("a"), None).unwrap();
        s.add_connection(spec("b"), None).unwrap();
        s.add_connection(spec("c"), None).unwrap();
        let x = s.add_connection(spec("x"), Some(&g)).unwrap();
        s.move_connection(&a, None, 99).unwrap();
        assert_eq!(names(&s, None), ["b", "c", "a"]);
        s.move_connection(&a, None, 0).unwrap();
        assert_eq!(names(&s, None), ["a", "b", "c"]);
        s.move_connection(&a, None, 1).unwrap();
        assert_eq!(names(&s, None), ["b", "a", "c"]);
        s.move_connection(&a, Some(&g), 0).unwrap();
        assert_eq!(names(&s, Some(&g)), ["a", "x"]);
        assert_eq!(names(&s, None), ["b", "c"]);
        s.move_connection(&x, Some(&g), 50).unwrap();
        assert_eq!(names(&s, Some(&g)), ["a", "x"]);
        s.move_connection(&x, None, 1).unwrap();
        assert_eq!(names(&s, None), ["b", "x", "c"]);
        let bad = GroupId::from_valid("nope".into());
        assert_eq!(
            s.move_connection(&x, Some(&bad), 0),
            Err(StoreError::UnknownGroup(bad))
        );
        let missing = ConnectionId::from_valid("zzz".into());
        assert_eq!(
            s.move_connection(&missing, None, 0),
            Err(StoreError::UnknownConnection(missing))
        );
    }

    #[test]
    fn groups_nest_three_deep_and_list_children() {
        let mut s = ConnectionStore::new();
        let a = s.add_group("A", None).unwrap();
        let b = s.add_group("B", Some(&a)).unwrap();
        let c = s.add_group("C", Some(&b)).unwrap();
        s.add_connection(spec("leaf"), Some(&c)).unwrap();
        assert_eq!(group_names(&s, None), ["A"]);
        assert_eq!(group_names(&s, Some(&a)), ["B"]);
        assert_eq!(group_names(&s, Some(&b)), ["C"]);
        assert_eq!(names(&s, Some(&c)), ["leaf"]);
        assert_eq!(s.group(&c).unwrap().parent(), Some(&b));
        assert_eq!(s.groups().len(), 3);
    }

    #[test]
    fn group_names_unique_among_siblings_only() {
        let mut s = ConnectionStore::new();
        let a = s.add_group("A", None).unwrap();
        let b = s.add_group("B", None).unwrap();
        assert_eq!(
            s.add_group(" A ", None),
            Err(StoreError::DuplicateGroupName("A".into()))
        );
        let inner = s.add_group("A", Some(&b)).unwrap();
        assert_eq!(inner.as_str(), "a-2");
        assert_eq!(s.add_group("", None), Err(StoreError::EmptyName));
        assert_eq!(
            s.rename_group(&b, "A"),
            Err(StoreError::DuplicateGroupName("A".into()))
        );
        s.rename_group(&a, "A").unwrap();
        s.rename_group(&a, "Alpha").unwrap();
        assert_eq!(s.group(&a).unwrap().name(), "Alpha");
        assert_eq!(s.group(&a).unwrap().id().as_str(), "a");
        assert_eq!(s.rename_group(&a, " "), Err(StoreError::EmptyName));
        let missing = GroupId::from_valid("zzz".into());
        assert_eq!(
            s.rename_group(&missing, "x"),
            Err(StoreError::UnknownGroup(missing.clone()))
        );
        assert_eq!(
            s.add_group("x", Some(&missing)),
            Err(StoreError::UnknownGroup(missing))
        );
    }

    #[test]
    fn delete_group_only_when_empty() {
        let mut s = ConnectionStore::new();
        let a = s.add_group("A", None).unwrap();
        let b = s.add_group("B", Some(&a)).unwrap();
        let c = s.add_connection(spec("c"), Some(&b)).unwrap();
        assert_eq!(
            s.delete_group(&a),
            Err(StoreError::GroupNotEmpty(a.clone()))
        );
        assert_eq!(
            s.delete_group(&b),
            Err(StoreError::GroupNotEmpty(b.clone()))
        );
        s.delete_connection(&c).unwrap();
        assert_eq!(s.delete_group(&b).unwrap().name(), "B");
        assert_eq!(s.delete_group(&a).unwrap().name(), "A");
        assert_eq!(s.delete_group(&a), Err(StoreError::UnknownGroup(a)));
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn move_group_reorders_nests_and_rejects_cycles() {
        let mut s = ConnectionStore::new();
        let a = s.add_group("A", None).unwrap();
        let b = s.add_group("B", None).unwrap();
        let c = s.add_group("C", None).unwrap();
        s.move_group(&c, None, 0).unwrap();
        assert_eq!(group_names(&s, None), ["C", "A", "B"]);
        s.move_group(&c, None, 99).unwrap();
        assert_eq!(group_names(&s, None), ["A", "B", "C"]);
        s.move_group(&b, Some(&a), 0).unwrap();
        s.move_group(&c, Some(&b), 0).unwrap();
        assert_eq!(group_names(&s, Some(&b)), ["C"]);
        assert_eq!(s.group(&b).unwrap().parent(), Some(&a));
        assert_eq!(
            s.move_group(&a, Some(&a), 0),
            Err(StoreError::GroupCycle(a.clone()))
        );
        assert_eq!(
            s.move_group(&a, Some(&c), 0),
            Err(StoreError::GroupCycle(a.clone()))
        );
        s.move_group(&c, None, 0).unwrap();
        assert_eq!(group_names(&s, None), ["C", "A"]);
        // Clash at the destination.
        let d = s.add_group("A", Some(&c)).unwrap();
        assert_eq!(
            s.move_group(&d, None, 0),
            Err(StoreError::DuplicateGroupName("A".into()))
        );
        let missing = GroupId::from_valid("zzz".into());
        assert_eq!(
            s.move_group(&missing, None, 0),
            Err(StoreError::UnknownGroup(missing.clone()))
        );
        assert_eq!(
            s.move_group(&a, Some(&missing), 0),
            Err(StoreError::UnknownGroup(missing))
        );
    }

    #[test]
    fn to_connection_configs_keeps_file_order() {
        let mut s = ConnectionStore::new();
        let g = s.add_group("G", None).unwrap();
        s.add_connection(spec("one"), Some(&g)).unwrap();
        s.add_connection(spec("two"), None).unwrap();
        let cfgs = s.to_connection_configs();
        let got: Vec<_> = cfgs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(got, ["one", "two"]);
    }

    #[test]
    fn store_error_messages_never_carry_secrets() {
        let msg = StoreError::TargetHasSecret.to_string();
        assert!(msg.contains("contains a secret") && msg.contains("keychain"));
    }
}
