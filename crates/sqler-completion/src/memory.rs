// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! An in-memory [`SchemaSource`] for tests, benches and examples.

use std::collections::HashMap;

use sqler_core::{ColumnInfo, ForeignKey, ObjectKind};

use crate::source::{Loadable, Relation, SchemaSource};

#[derive(Debug, Clone)]
struct TableData {
    name: String,
    kind: ObjectKind,
    columns: Option<Vec<ColumnInfo>>,
    foreign_keys: Vec<ForeignKey>,
}

#[derive(Debug, Clone, Default)]
struct SchemaData {
    name: String,
    tables: Vec<TableData>,
    index: HashMap<String, usize>,
    relations_loaded: bool,
}

/// A schema held in memory.
///
/// Names are stored exactly as given. A table added with `None` columns models "columns not
/// loaded yet".
#[derive(Debug, Clone, Default)]
pub struct MemorySchema {
    search_path: Vec<String>,
    schemas: Vec<SchemaData>,
    index: HashMap<String, usize>,
}

impl MemorySchema {
    /// An empty schema source whose unqualified names resolve in `search_path`, in order.
    #[must_use]
    pub fn new<S: Into<String>>(search_path: impl IntoIterator<Item = S>) -> Self {
        let mut me = Self::default();
        for s in search_path {
            let s: String = s.into();
            me.search_path.push(s.clone());
            me.schema_index(&s);
        }
        me
    }

    fn schema_index(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.schemas.len();
        self.schemas.push(SchemaData {
            name: name.to_owned(),
            relations_loaded: true,
            ..SchemaData::default()
        });
        self.index.insert(name.to_owned(), i);
        i
    }

    /// Adds an empty schema (no-op if it exists).
    pub fn add_schema(&mut self, name: &str) -> &mut Self {
        self.schema_index(name);
        self
    }

    /// Adds a table or view. `columns` of `None` means the columns are not loaded.
    pub fn add_table(
        &mut self,
        schema: &str,
        name: &str,
        kind: ObjectKind,
        columns: Option<Vec<ColumnInfo>>,
    ) -> &mut Self {
        let s = self.schema_index(schema);
        let data = &mut self.schemas[s];
        let table = TableData {
            name: name.to_owned(),
            kind,
            columns,
            foreign_keys: Vec::new(),
        };
        if let Some(&t) = data.index.get(name) {
            data.tables[t] = table;
        } else {
            data.index.insert(name.to_owned(), data.tables.len());
            data.tables.push(table);
        }
        self
    }

    /// Adds a foreign key to a table. Does nothing when the table does not exist.
    pub fn add_foreign_key(&mut self, schema: &str, table: &str, fk: ForeignKey) -> &mut Self {
        if let Some(&s) = self.index.get(schema)
            && let Some(&t) = self.schemas[s].index.get(table)
        {
            self.schemas[s].tables[t].foreign_keys.push(fk);
        }
        self
    }

    /// Marks the relation list of `schema` as not loaded (the schema is created if missing).
    pub fn mark_relations_not_loaded(&mut self, schema: &str) -> &mut Self {
        let s = self.schema_index(schema);
        self.schemas[s].relations_loaded = false;
        self
    }

    fn schema(&self, name: &str) -> Option<&SchemaData> {
        self.index.get(name).map(|&i| &self.schemas[i])
    }

    fn search_schemas(&self) -> impl Iterator<Item = &SchemaData> {
        self.search_path.iter().filter_map(|s| self.schema(s))
    }
}

fn relations_of(s: &SchemaData) -> impl Iterator<Item = Relation<'_>> {
    s.tables
        .iter()
        .map(|t| Relation::new(Some(s.name.as_str()), t.name.as_str(), t.kind))
}

impl SchemaSource for MemorySchema {
    fn schemas(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.schemas.iter().map(|s| s.name.as_str()))
    }

    fn search_path(&self) -> &[String] {
        &self.search_path
    }

    fn relations(
        &self,
        schema: Option<&str>,
    ) -> Loadable<Box<dyn Iterator<Item = Relation<'_>> + '_>> {
        if let Some(name) = schema {
            return match self.schema(name) {
                Some(s) if !s.relations_loaded => Loadable::NotLoaded,
                Some(s) => Loadable::Loaded(Box::new(relations_of(s))),
                None => Loadable::Loaded(Box::new(std::iter::empty())),
            };
        }
        if self.search_schemas().any(|s| !s.relations_loaded) {
            return Loadable::NotLoaded;
        }
        Loadable::Loaded(Box::new(self.search_schemas().flat_map(relations_of)))
    }

    fn columns(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ColumnInfo]> {
        fn find<'s>(s: &'s SchemaData, relation: &str) -> Option<&'s TableData> {
            s.index.get(relation).map(|&t| &s.tables[t])
        }
        let table = match schema {
            Some(name) => self.schema(name).and_then(|s| find(s, relation)),
            None => self.search_schemas().find_map(|s| find(s, relation)),
        };
        match table.and_then(|t| t.columns.as_deref()) {
            Some(cols) => Loadable::Loaded(cols),
            None => Loadable::NotLoaded,
        }
    }

    fn foreign_keys(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ForeignKey]> {
        fn find<'s>(s: &'s SchemaData, relation: &str) -> Option<&'s TableData> {
            s.index.get(relation).map(|&t| &s.tables[t])
        }
        let table = match schema {
            Some(name) => self.schema(name).and_then(|s| find(s, relation)),
            None => self.search_schemas().find_map(|s| find(s, relation)),
        };
        match table {
            Some(t) => Loadable::Loaded(&t.foreign_keys),
            None => Loadable::NotLoaded,
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_core::DataType;

    use super::*;

    fn col(name: &str) -> ColumnInfo {
        ColumnInfo::new(name, DataType::Integer)
    }

    #[test]
    fn lookup_and_search_path() {
        let mut m = MemorySchema::new(["a", "b"]);
        m.add_table("a", "t1", ObjectKind::Table, Some(vec![col("x")]));
        m.add_table("b", "t1", ObjectKind::Table, Some(vec![col("y")]));
        m.add_table("b", "t2", ObjectKind::View, None);
        m.add_table("c", "t3", ObjectKind::Table, Some(vec![]));
        assert_eq!(m.schemas().count(), 3);
        let Loadable::Loaded(cols) = m.columns(None, "t1") else {
            panic!("loaded")
        };
        assert_eq!(cols[0].name, "x", "first schema in the search path wins");
        assert_eq!(m.columns(Some("b"), "t2"), Loadable::NotLoaded);
        assert_eq!(m.columns(Some("b"), "nope"), Loadable::NotLoaded);
        let Loadable::Loaded(rels) = m.relations(None) else {
            panic!("loaded")
        };
        assert_eq!(rels.count(), 3);
        let Loadable::Loaded(rels) = m.relations(Some("c")) else {
            panic!("loaded")
        };
        assert_eq!(rels.count(), 1);
    }

    #[test]
    fn relations_not_loaded() {
        let mut m = MemorySchema::new(["a"]);
        m.add_table("a", "t", ObjectKind::Table, None);
        m.mark_relations_not_loaded("a");
        assert!(matches!(m.relations(None), Loadable::NotLoaded));
        assert!(matches!(m.relations(Some("a")), Loadable::NotLoaded));
        assert!(matches!(m.relations(Some("zz")), Loadable::Loaded(_)));
    }

    #[test]
    fn replacing_a_table_keeps_order() {
        let mut m = MemorySchema::new(["a"]);
        m.add_table("a", "t", ObjectKind::Table, None);
        m.add_table("a", "u", ObjectKind::Table, None);
        m.add_table("a", "t", ObjectKind::View, Some(vec![col("c")]));
        let Loadable::Loaded(rels) = m.relations(Some("a")) else {
            panic!("loaded")
        };
        let v: Vec<_> = rels.map(|r| (r.name, r.kind)).collect();
        assert_eq!(v, [("t", ObjectKind::View), ("u", ObjectKind::Table)]);
    }

    #[test]
    fn foreign_keys_are_stored_per_table() {
        let mut m = MemorySchema::new(["a"]);
        m.add_table("a", "orders", ObjectKind::Table, Some(vec![col("user_id")]));
        let fk = ForeignKey::new(vec!["user_id".to_owned()], "users", vec!["id".to_owned()]);
        m.add_foreign_key("a", "orders", fk.clone());
        m.add_foreign_key("a", "missing", fk.clone());
        m.add_foreign_key("zz", "orders", fk.clone());
        assert_eq!(
            m.foreign_keys(Some("a"), "orders"),
            Loadable::Loaded(&[fk][..])
        );
        assert!(matches!(m.foreign_keys(None, "orders"), Loadable::Loaded(f) if f.len() == 1));
        assert_eq!(m.foreign_keys(Some("a"), "missing"), Loadable::NotLoaded);
        assert_eq!(m.foreign_keys(Some("zz"), "orders"), Loadable::NotLoaded);
        m.add_table("a", "plain", ObjectKind::Table, None);
        assert_eq!(
            m.foreign_keys(Some("a"), "plain"),
            Loadable::Loaded(&[][..])
        );
    }
}
