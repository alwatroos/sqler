// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Saved-query types, validation and the in-memory store.

use std::fmt;

use thiserror::Error;

use crate::ConnectionId;

/// Version written to, and the highest read from, `queries.toml`.
pub const SAVED_QUERIES_FORMAT_VERSION: u32 = 1;

/// Stable identifier of a saved query, `q-<n>`; never changed after creation.
/// Follows the connection id grammar (`[a-z0-9-]+`, at most 64 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SavedQueryId(String);

impl SavedQueryId {
    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps an id already checked with [`id_problem`].
    pub(crate) fn from_valid(id: String) -> Self {
        debug_assert!(id_problem(&id).is_none());
        Self(id)
    }
}

impl fmt::Display for SavedQueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why `s` is not a valid saved-query id, `None` when it is valid.
pub(crate) fn id_problem(s: &str) -> Option<&'static str> {
    if let Err(reason) = crate::secrets::validate_id(s) {
        return Some(reason);
    }
    if !crate::history::valid_connection_id(s) {
        return Some("id may only contain lowercase letters, digits and '-'");
    }
    None
}

/// A saved query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedQuery {
    id: SavedQueryId,
    name: String,
    folder: Option<String>,
    connection: Option<ConnectionId>,
    tags: Vec<String>,
    sql: String,
}

impl SavedQuery {
    /// Builds a query from parts the loader has already normalised.
    pub(crate) fn from_parts(
        id: SavedQueryId,
        name: String,
        folder: Option<String>,
        connection: Option<ConnectionId>,
        tags: Vec<String>,
        sql: String,
    ) -> Self {
        Self {
            id,
            name,
            folder,
            connection,
            tags,
            sql,
        }
    }

    /// The stable id.
    #[must_use]
    pub fn id(&self) -> &SavedQueryId {
        &self.id
    }

    /// The display name, unique within its folder.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The folder path (`a/b`), `None` for the top level.
    #[must_use]
    pub fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }

    /// The connection it is bound to, if any. The id may refer to a connection
    /// that no longer exists.
    #[must_use]
    pub fn connection(&self) -> Option<&ConnectionId> {
        self.connection.as_ref()
    }

    /// The tags, in order, without duplicates.
    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    /// The SQL text.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.sql
    }
}

/// The editable fields of a saved query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedQueryDraft {
    /// Display name; trimmed, non-empty, unique within the folder.
    pub name: String,
    /// The SQL text.
    pub sql: String,
    /// Folder path of `/`-separated segments; `None` or `Some("")` is the top level.
    pub folder: Option<String>,
    /// Optional connection binding.
    pub connection: Option<ConnectionId>,
    /// Tags; trimmed, non-empty, deduplicated.
    pub tags: Vec<String>,
}

impl SavedQueryDraft {
    /// A draft with a name and SQL, at the top level, unbound and untagged.
    #[must_use]
    pub fn new(name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            sql: sql.into(),
            folder: None,
            connection: None,
            tags: Vec::new(),
        }
    }
}

/// Why a saved-query operation was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SavedQueryError {
    /// The name is empty or only whitespace.
    #[error("name must not be empty")]
    EmptyName,
    /// Another query in the same folder has this name.
    #[error("a saved query named {0:?} already exists in this folder")]
    DuplicateName(String),
    /// No saved query has this id.
    #[error("unknown saved query {0}")]
    UnknownQuery(SavedQueryId),
    /// A folder path has an empty segment.
    #[error("invalid folder {0:?}: segments must not be empty")]
    InvalidFolder(String),
    /// A tag is empty or only whitespace.
    #[error("tags must not be empty")]
    EmptyTag,
}

pub(crate) fn normalize_name(name: &str) -> Result<String, SavedQueryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SavedQueryError::EmptyName);
    }
    Ok(name.to_owned())
}

pub(crate) fn normalize_folder(folder: Option<&str>) -> Result<Option<String>, SavedQueryError> {
    let Some(raw) = folder else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let segments: Vec<&str> = raw.split('/').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(SavedQueryError::InvalidFolder(raw.to_owned()));
    }
    Ok(Some(segments.join("/")))
}

pub(crate) fn normalize_tags(tags: &[String]) -> Result<Vec<String>, SavedQueryError> {
    let mut out: Vec<String> = Vec::with_capacity(tags.len());
    for t in tags {
        let t = t.trim();
        if t.is_empty() {
            return Err(SavedQueryError::EmptyTag);
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    Ok(out)
}

/// The saved queries, in list order. A plain value: change it, then
/// [`ConfigDir::save_saved_queries`](crate::ConfigDir::save_saved_queries).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedQueries {
    queries: Vec<SavedQuery>,
}

impl SavedQueries {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// All queries, in list order (file order; new ones are appended).
    #[must_use]
    pub fn queries(&self) -> &[SavedQuery] {
        &self.queries
    }

    /// The query with this id.
    #[must_use]
    pub fn get(&self, id: &SavedQueryId) -> Option<&SavedQuery> {
        self.queries.iter().find(|q| q.id == *id)
    }

    /// The folders in use, sorted and without duplicates.
    #[must_use]
    pub fn folders(&self) -> Vec<&str> {
        let mut f: Vec<&str> = self.queries.iter().filter_map(SavedQuery::folder).collect();
        f.sort_unstable();
        f.dedup();
        f
    }

    fn position(&self, id: &SavedQueryId) -> Result<usize, SavedQueryError> {
        self.queries
            .iter()
            .position(|q| q.id == *id)
            .ok_or_else(|| SavedQueryError::UnknownQuery(id.clone()))
    }

    /// Fails when another query (not `except`) has `name` in `folder`.
    pub(crate) fn check_unique(
        &self,
        name: &str,
        folder: Option<&str>,
        except: Option<&SavedQueryId>,
    ) -> Result<(), SavedQueryError> {
        let clash = self
            .queries
            .iter()
            .any(|q| q.name == name && q.folder() == folder && Some(&q.id) != except);
        if clash {
            Err(SavedQueryError::DuplicateName(name.to_owned()))
        } else {
            Ok(())
        }
    }

    /// The next free `q-<n>`: one more than the highest existing `n`.
    fn next_id(&self) -> SavedQueryId {
        let max = self
            .queries
            .iter()
            .filter_map(|q| q.id.as_str().strip_prefix("q-"))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .filter_map(|n| n.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let n = match max.checked_add(1) {
            Some(n) => n,
            // Hand-edited file at the top of the range: take the lowest free number.
            None => (1..=u64::MAX)
                .find(|n| {
                    let id = format!("q-{n}");
                    !self.queries.iter().any(|q| q.id.as_str() == id)
                })
                .unwrap_or(1),
        };
        SavedQueryId(format!("q-{n}"))
    }

    /// Appends a new query and returns its id.
    ///
    /// # Errors
    /// [`SavedQueryError::EmptyName`], [`SavedQueryError::DuplicateName`],
    /// [`SavedQueryError::InvalidFolder`] or [`SavedQueryError::EmptyTag`].
    pub fn create(&mut self, draft: SavedQueryDraft) -> Result<SavedQueryId, SavedQueryError> {
        let name = normalize_name(&draft.name)?;
        let folder = normalize_folder(draft.folder.as_deref())?;
        let tags = normalize_tags(&draft.tags)?;
        self.check_unique(&name, folder.as_deref(), None)?;
        let id = self.next_id();
        self.queries.push(SavedQuery {
            id: id.clone(),
            name,
            folder,
            connection: draft.connection,
            tags,
            sql: draft.sql,
        });
        Ok(id)
    }

    /// Replaces every editable field of a query; its id and list position stay.
    ///
    /// # Errors
    /// [`SavedQueryError::UnknownQuery`] and the errors of
    /// [`create`](Self::create). Nothing changes on error.
    pub fn update(
        &mut self,
        id: &SavedQueryId,
        draft: SavedQueryDraft,
    ) -> Result<(), SavedQueryError> {
        let i = self.position(id)?;
        let name = normalize_name(&draft.name)?;
        let folder = normalize_folder(draft.folder.as_deref())?;
        let tags = normalize_tags(&draft.tags)?;
        self.check_unique(&name, folder.as_deref(), Some(id))?;
        let q = &mut self.queries[i];
        q.name = name;
        q.folder = folder;
        q.connection = draft.connection;
        q.tags = tags;
        q.sql = draft.sql;
        Ok(())
    }

    /// Renames a query.
    ///
    /// # Errors
    /// [`SavedQueryError::UnknownQuery`], [`SavedQueryError::EmptyName`] or
    /// [`SavedQueryError::DuplicateName`].
    pub fn rename(&mut self, id: &SavedQueryId, name: &str) -> Result<(), SavedQueryError> {
        let i = self.position(id)?;
        let name = normalize_name(name)?;
        self.check_unique(&name, self.queries[i].folder(), Some(id))?;
        self.queries[i].name = name;
        Ok(())
    }

    /// Replaces the SQL text of a query.
    ///
    /// # Errors
    /// [`SavedQueryError::UnknownQuery`].
    pub fn set_sql(
        &mut self,
        id: &SavedQueryId,
        sql: impl Into<String>,
    ) -> Result<(), SavedQueryError> {
        let i = self.position(id)?;
        self.queries[i].sql = sql.into();
        Ok(())
    }

    /// Removes a query and returns it.
    ///
    /// # Errors
    /// [`SavedQueryError::UnknownQuery`].
    pub fn delete(&mut self, id: &SavedQueryId) -> Result<SavedQuery, SavedQueryError> {
        let i = self.position(id)?;
        Ok(self.queries.remove(i))
    }

    /// Appends an already validated query (used by the file loader).
    pub(crate) fn push_loaded(&mut self, query: SavedQuery) {
        self.queries.push(query);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str) -> SavedQueryDraft {
        SavedQueryDraft::new(name, "select 1")
    }

    fn loaded(id: &str) -> SavedQuery {
        SavedQuery::from_parts(
            SavedQueryId::from_valid(id.into()),
            id.into(),
            None,
            None,
            vec![],
            String::new(),
        )
    }

    #[test]
    fn create_assigns_sequential_ids_and_appends() {
        let mut s = SavedQueries::new();
        let a = s.create(draft("a")).unwrap();
        let b = s.create(draft("b")).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("q-1", "q-2"));
        let names: Vec<_> = s.queries().iter().map(SavedQuery::name).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn ids_continue_after_the_highest_and_ignore_other_shapes() {
        let mut s = SavedQueries::new();
        for id in ["q-7", "q-x", "other", "q-3"] {
            s.push_loaded(loaded(id));
        }
        let n = s.create(draft("n")).unwrap();
        assert_eq!(n.as_str(), "q-8");
        // Deleting the newest frees its number again.
        s.delete(&n).unwrap();
        assert_eq!(s.create(draft("m")).unwrap().as_str(), "q-8");
    }

    #[test]
    fn name_is_trimmed_and_must_be_non_empty_and_unique_per_folder() {
        let mut s = SavedQueries::new();
        assert_eq!(s.create(draft("  ")), Err(SavedQueryError::EmptyName));
        s.create(draft(" a ")).unwrap();
        assert_eq!(s.queries()[0].name(), "a");
        assert_eq!(
            s.create(draft("a")),
            Err(SavedQueryError::DuplicateName("a".into()))
        );
        let mut in_folder = draft("a");
        in_folder.folder = Some("x".into());
        s.create(in_folder.clone()).unwrap();
        assert!(s.create(in_folder).is_err());
    }

    #[test]
    fn folder_normalisation() {
        assert_eq!(normalize_folder(None), Ok(None));
        assert_eq!(normalize_folder(Some("")), Ok(None));
        assert_eq!(normalize_folder(Some("  ")), Ok(None));
        assert_eq!(
            normalize_folder(Some(" a / b ")),
            Ok(Some("a/b".to_owned()))
        );
        for bad in ["/a", "a/", "a//b", "a/ /b"] {
            assert_eq!(
                normalize_folder(Some(bad)),
                Err(SavedQueryError::InvalidFolder(bad.into()))
            );
        }
    }

    #[test]
    fn tags_are_trimmed_deduplicated_in_order() {
        let t = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            normalize_tags(&t(&[" b", "a", "b ", "a"])),
            Ok(t(&["b", "a"]))
        );
        assert_eq!(
            normalize_tags(&t(&["a", " "])),
            Err(SavedQueryError::EmptyTag)
        );
    }

    #[test]
    fn update_replaces_fields_and_keeps_position() {
        let mut s = SavedQueries::new();
        let a = s.create(draft("a")).unwrap();
        s.create(draft("b")).unwrap();
        let mut d = draft("renamed");
        d.sql = "select 2".into();
        d.folder = Some("f / g".into());
        d.tags = vec!["t".into(), "t".into()];
        d.connection = Some(ConnectionId::from_valid("billing".into()));
        s.update(&a, d).unwrap();
        let q = s.get(&a).unwrap();
        assert_eq!(q.name(), "renamed");
        assert_eq!(q.sql(), "select 2");
        assert_eq!(q.folder(), Some("f/g"));
        assert_eq!(q.tags(), ["t"]);
        assert_eq!(q.connection().map(ConnectionId::as_str), Some("billing"));
        assert_eq!(s.queries()[0].id(), &a);
    }

    #[test]
    fn update_to_own_name_is_allowed_and_a_clash_changes_nothing() {
        let mut s = SavedQueries::new();
        let a = s.create(draft("a")).unwrap();
        s.create(draft("b")).unwrap();
        s.update(&a, draft("a")).unwrap();
        let before = s.clone();
        assert_eq!(
            s.update(&a, draft("b")),
            Err(SavedQueryError::DuplicateName("b".into()))
        );
        assert_eq!(s.update(&a, draft("")), Err(SavedQueryError::EmptyName));
        assert_eq!(s, before);
    }

    #[test]
    fn rename_set_sql_delete_and_unknown_ids() {
        let mut s = SavedQueries::new();
        let a = s.create(draft("a")).unwrap();
        let b = s.create(draft("b")).unwrap();
        s.rename(&a, " c ").unwrap();
        assert_eq!(s.get(&a).unwrap().name(), "c");
        assert_eq!(
            s.rename(&a, "b"),
            Err(SavedQueryError::DuplicateName("b".into()))
        );
        assert_eq!(s.rename(&a, ""), Err(SavedQueryError::EmptyName));
        s.set_sql(&b, String::from("select 3")).unwrap();
        assert_eq!(s.get(&b).unwrap().sql(), "select 3");
        let gone = s.delete(&a).unwrap();
        assert_eq!(gone.name(), "c");
        assert_eq!(s.queries().len(), 1);
        let unknown = SavedQueryError::UnknownQuery(a.clone());
        assert_eq!(s.delete(&a), Err(unknown.clone()));
        assert_eq!(s.rename(&a, "x"), Err(unknown.clone()));
        assert_eq!(s.set_sql(&a, "x"), Err(unknown.clone()));
        assert_eq!(s.update(&a, draft("x")), Err(unknown));
    }

    #[test]
    fn folders_are_sorted_and_deduplicated() {
        let mut s = SavedQueries::new();
        for (n, f) in [
            ("1", Some("b")),
            ("2", Some("a/x")),
            ("3", None),
            ("4", Some("b")),
        ] {
            let mut d = draft(n);
            d.folder = f.map(str::to_owned);
            s.create(d).unwrap();
        }
        // "1" and "4" share folder "b" but have different names.
        assert_eq!(s.folders(), ["a/x", "b"]);
    }

    #[test]
    fn id_grammar() {
        assert_eq!(id_problem("q-1"), None);
        assert!(id_problem("Q-1").is_some());
        assert!(id_problem("-q").is_some());
        assert!(id_problem("").is_some());
    }

    #[test]
    fn id_at_u64_max_does_not_produce_a_duplicate() {
        let mut s = SavedQueries::new();
        s.push_loaded(loaded("q-18446744073709551615"));
        s.push_loaded(loaded("q-1"));
        let id = s.create(draft("new")).unwrap();
        assert_eq!(id.as_str(), "q-2");
    }
}
