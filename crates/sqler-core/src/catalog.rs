// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use crate::Column;

/// What kind of relation a [`Table`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TableKind {
    /// A base table.
    Table,
    /// A view.
    View,
    /// Anything else.
    Other,
}

/// A table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Table {
    /// Table name.
    pub name: String,
    /// Table kind.
    pub kind: TableKind,
    /// Columns; may be empty when only names were listed.
    pub columns: Vec<Column>,
}

impl Table {
    /// Creates a table without columns.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: TableKind) -> Self {
        Self {
            name: name.into(),
            kind,
            columns: Vec::new(),
        }
    }
}

/// A schema and its tables.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Schema {
    /// Schema name.
    pub name: String,
    /// Tables in the schema.
    pub tables: Vec<Table>,
}

impl Schema {
    /// Creates an empty schema.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tables: Vec::new(),
        }
    }

    /// Finds a table by exact name.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }
}

/// The schemas of one database.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Catalog {
    /// Schemas in the database.
    pub schemas: Vec<Schema>,
}

impl Catalog {
    /// Finds a schema by exact name.
    #[must_use]
    pub fn schema(&self, name: &str) -> Option<&Schema> {
        self.schemas.iter().find(|s| s.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataType;

    #[test]
    fn lookup_hit_and_miss() {
        let mut t = Table::new("users", TableKind::Table);
        t.columns.push(Column::new("id", DataType::Integer));
        let mut s = Schema::new("main");
        s.tables.push(t);
        let mut c = Catalog::default();
        assert!(c.schema("main").is_none());
        c.schemas.push(s);

        let s = c.schema("main").unwrap();
        assert_eq!(s.table("users").unwrap().kind, TableKind::Table);
        assert_eq!(s.table("users").unwrap().columns.len(), 1);
        assert!(s.table("nope").is_none());
        assert!(c.schema("other").is_none());
    }

    #[test]
    fn constructors_are_empty() {
        let t = Table::new("v", TableKind::View);
        assert_eq!(t.columns.len(), 0);
        assert_eq!(Schema::new("s").tables.len(), 0);
    }
}
