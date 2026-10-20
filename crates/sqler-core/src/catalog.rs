// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The object model of the object browser: kinds, addresses, objects, tables and keys.

use crate::DataType;

/// A kind of database object the object browser can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ObjectKind {
    /// A database on a server.
    Database,
    /// A schema (namespace) in a database.
    Schema,
    /// A base table.
    Table,
    /// A view.
    View,
    /// A materialised view.
    MaterializedView,
    /// A function.
    Function,
    /// A stored procedure.
    Procedure,
    /// A trigger.
    Trigger,
    /// A sequence.
    Sequence,
    /// A user-defined type.
    Type,
    /// An index.
    Index,
    /// An installed extension.
    Extension,
    /// A role or user.
    Role,
}

impl ObjectKind {
    /// Every kind, in browser order.
    pub const ALL: [ObjectKind; 13] = [
        Self::Database,
        Self::Schema,
        Self::Table,
        Self::View,
        Self::MaterializedView,
        Self::Function,
        Self::Procedure,
        Self::Trigger,
        Self::Sequence,
        Self::Type,
        Self::Index,
        Self::Extension,
        Self::Role,
    ];

    /// The kebab-case identifier, for example `materialized-view`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Schema => "schema",
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized-view",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
            Self::Sequence => "sequence",
            Self::Type => "type",
            Self::Index => "index",
            Self::Extension => "extension",
            Self::Role => "role",
        }
    }

    /// Parses an identifier produced by [`ObjectKind::as_str`] (exact match).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == id)
    }

    /// Where the parent of this kind points when listing it.
    #[must_use]
    pub fn level(self) -> ObjectLevel {
        match self {
            Self::Database | Self::Role => ObjectLevel::Server,
            Self::Schema | Self::Extension => ObjectLevel::Database,
            _ => ObjectLevel::Schema,
        }
    }
}

/// The level of the tree a `list` parent points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ObjectLevel {
    /// The server: the parent path is empty.
    Server,
    /// A database.
    Database,
    /// A schema.
    Schema,
}

/// The address of a node: database, schema and table, each optional.
///
/// `None` means "the session's current or default one" where the contract allows it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct ObjectPath {
    /// The database.
    pub database: Option<String>,
    /// The schema.
    pub schema: Option<String>,
    /// The table (or view) an index or trigger belongs to.
    pub table: Option<String>,
}

impl ObjectPath {
    /// The empty path: the server root, or the current database and schema.
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// Sets the database.
    #[must_use]
    pub fn in_database(mut self, name: impl Into<String>) -> Self {
        self.database = Some(name.into());
        self
    }

    /// Sets the schema.
    #[must_use]
    pub fn in_schema(mut self, name: impl Into<String>) -> Self {
        self.schema = Some(name.into());
        self
    }

    /// Sets the table.
    #[must_use]
    pub fn in_table(mut self, name: impl Into<String>) -> Self {
        self.table = Some(name.into());
        self
    }
}

/// One object: its kind, where it lives and its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DbObject {
    /// The kind.
    pub kind: ObjectKind,
    /// The path of the node that contains it.
    pub parent: ObjectPath,
    /// The object name.
    pub name: String,
}

impl DbObject {
    /// Builds an object.
    #[must_use]
    pub fn new(kind: ObjectKind, parent: ObjectPath, name: impl Into<String>) -> Self {
        Self {
            kind,
            parent,
            name: name.into(),
        }
    }

    /// The path of this object's children: the parent plus the object's own name at the next level.
    ///
    /// `Database` sets the database, `Schema` the schema, and `Table`, `View` and
    /// `MaterializedView` the table. Other kinds have no children and return the parent.
    #[must_use]
    pub fn child_path(&self) -> ObjectPath {
        let mut path = self.parent.clone();
        match self.kind {
            ObjectKind::Database => path.database = Some(self.name.clone()),
            ObjectKind::Schema => path.schema = Some(self.name.clone()),
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                path.table = Some(self.name.clone());
            }
            _ => {}
        }
        path
    }
}

/// The result of listing one kind under one parent.
///
/// Exactly two states, so this enum is not `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// The objects found; possibly none.
    Objects(Vec<DbObject>),
    /// The database does not have this kind.
    Unsupported,
}

/// A column of a table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Logical type.
    pub data_type: DataType,
    /// The type as the server spells it, if known.
    pub native_type: Option<String>,
    /// Whether the column accepts `NULL`.
    pub nullable: bool,
    /// The default as SQL expression text.
    pub default: Option<String>,
}

impl ColumnInfo {
    /// A nullable column without default.
    #[must_use]
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            native_type: None,
            nullable: true,
            default: None,
        }
    }

    /// Sets the server's spelling of the type.
    #[must_use]
    pub fn with_native_type(mut self, native_type: impl Into<String>) -> Self {
        self.native_type = Some(native_type.into());
        self
    }

    /// Sets nullability.
    #[must_use]
    pub fn with_nullable(mut self, nullable: bool) -> Self {
        self.nullable = nullable;
        self
    }

    /// Sets the default expression text.
    #[must_use]
    pub fn with_default(mut self, default: impl Into<String>) -> Self {
        self.default = Some(default.into());
        self
    }
}

/// A primary key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrimaryKey {
    /// Constraint name, if the server has one.
    pub name: Option<String>,
    /// Key columns in key order.
    pub columns: Vec<String>,
}

impl PrimaryKey {
    /// An unnamed primary key over `columns`.
    #[must_use]
    pub fn new(columns: Vec<String>) -> Self {
        Self {
            name: None,
            columns,
        }
    }

    /// Sets the constraint name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// What a foreign key does when the referenced row changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ReferentialAction {
    /// `NO ACTION`.
    #[default]
    NoAction,
    /// `RESTRICT`.
    Restrict,
    /// `CASCADE`.
    Cascade,
    /// `SET NULL`.
    SetNull,
    /// `SET DEFAULT`.
    SetDefault,
}

/// A foreign key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForeignKey {
    /// Constraint name, if the server has one.
    pub name: Option<String>,
    /// Local columns.
    pub columns: Vec<String>,
    /// Schema of the referenced table, if known.
    pub referenced_schema: Option<String>,
    /// Referenced table.
    pub referenced_table: String,
    /// Referenced columns; empty means the referenced table's primary key.
    pub referenced_columns: Vec<String>,
    /// Action on update.
    pub on_update: ReferentialAction,
    /// Action on delete.
    pub on_delete: ReferentialAction,
}

impl ForeignKey {
    /// An unnamed foreign key with `NO ACTION` on both events.
    #[must_use]
    pub fn new(
        columns: Vec<String>,
        referenced_table: impl Into<String>,
        referenced_columns: Vec<String>,
    ) -> Self {
        Self {
            name: None,
            columns,
            referenced_schema: None,
            referenced_table: referenced_table.into(),
            referenced_columns,
            on_update: ReferentialAction::NoAction,
            on_delete: ReferentialAction::NoAction,
        }
    }

    /// Sets the constraint name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the referenced schema.
    #[must_use]
    pub fn with_referenced_schema(mut self, schema: impl Into<String>) -> Self {
        self.referenced_schema = Some(schema.into());
        self
    }

    /// Sets the update action.
    #[must_use]
    pub fn with_on_update(mut self, action: ReferentialAction) -> Self {
        self.on_update = action;
        self
    }

    /// Sets the delete action.
    #[must_use]
    pub fn with_on_delete(mut self, action: ReferentialAction) -> Self {
        self.on_delete = action;
        self
    }
}

/// A table, view or materialised view with its column details and keys.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Table {
    /// The object itself.
    pub object: DbObject,
    /// Columns in declaration order.
    pub columns: Vec<ColumnInfo>,
    /// The primary key, if any.
    pub primary_key: Option<PrimaryKey>,
    /// Foreign keys.
    pub foreign_keys: Vec<ForeignKey>,
}

impl Table {
    /// A table without columns or keys.
    #[must_use]
    pub fn new(object: DbObject) -> Self {
        Self {
            object,
            columns: Vec::new(),
            primary_key: None,
            foreign_keys: Vec::new(),
        }
    }

    /// Sets the columns.
    #[must_use]
    pub fn with_columns(mut self, columns: Vec<ColumnInfo>) -> Self {
        self.columns = columns;
        self
    }

    /// Sets the primary key.
    #[must_use]
    pub fn with_primary_key(mut self, key: PrimaryKey) -> Self {
        self.primary_key = Some(key);
        self
    }

    /// Sets the foreign keys.
    #[must_use]
    pub fn with_foreign_keys(mut self, keys: Vec<ForeignKey>) -> Self {
        self.foreign_keys = keys;
        self
    }

    /// Finds a column by exact name.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip() {
        assert_eq!(ObjectKind::ALL.len(), 13);
        for kind in ObjectKind::ALL {
            assert_eq!(ObjectKind::from_id(kind.as_str()), Some(kind));
        }
        let ids: Vec<_> = ObjectKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            ids,
            [
                "database",
                "schema",
                "table",
                "view",
                "materialized-view",
                "function",
                "procedure",
                "trigger",
                "sequence",
                "type",
                "index",
                "extension",
                "role"
            ]
        );
        for bad in ["", "Table", "materialized_view", " table", "tables"] {
            assert_eq!(ObjectKind::from_id(bad), None);
        }
        assert!(ObjectKind::Database < ObjectKind::Role);
    }

    #[test]
    fn levels() {
        use ObjectKind as K;
        use ObjectLevel as L;
        for kind in K::ALL {
            let expected = match kind {
                K::Database | K::Role => L::Server,
                K::Schema | K::Extension => L::Database,
                _ => L::Schema,
            };
            assert_eq!(kind.level(), expected, "{kind:?}");
        }
    }

    #[test]
    fn path_builders() {
        assert_eq!(ObjectPath::root(), ObjectPath::default());
        let p = ObjectPath::root()
            .in_database("d")
            .in_schema("s")
            .in_table("t");
        assert_eq!(p.database.as_deref(), Some("d"));
        assert_eq!(p.schema.as_deref(), Some("s"));
        assert_eq!(p.table.as_deref(), Some("t"));
        let mut set = std::collections::HashSet::new();
        set.insert(p.clone());
        assert!(set.contains(&p));
    }

    #[test]
    fn child_paths() {
        let parent = ObjectPath::root().in_database("d").in_schema("s");
        for kind in ObjectKind::ALL {
            let obj = DbObject::new(kind, parent.clone(), "n");
            let child = obj.child_path();
            let mut expected = parent.clone();
            match kind {
                ObjectKind::Database => expected.database = Some("n".into()),
                ObjectKind::Schema => expected.schema = Some("n".into()),
                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                    expected.table = Some("n".into());
                }
                _ => {}
            }
            assert_eq!(child, expected, "{kind:?}");
        }
        let obj = DbObject::new(ObjectKind::Table, ObjectPath::root(), "t");
        assert_eq!((obj.kind, obj.name.as_str()), (ObjectKind::Table, "t"));
        assert_eq!(obj.child_path(), ObjectPath::root().in_table("t"));
    }

    #[test]
    fn column_builders() {
        let c = ColumnInfo::new("id", DataType::Integer);
        assert!(c.nullable);
        assert_eq!((c.native_type, c.default), (None, None));
        let c = ColumnInfo::new("id", DataType::Integer)
            .with_native_type("INTEGER")
            .with_nullable(false)
            .with_default("0");
        assert_eq!(c.native_type.as_deref(), Some("INTEGER"));
        assert!(!c.nullable);
        assert_eq!(c.default.as_deref(), Some("0"));
    }

    #[test]
    fn key_builders() {
        let pk = PrimaryKey::new(vec!["a".into(), "b".into()]);
        assert_eq!((pk.name.as_deref(), pk.columns.len()), (None, 2));
        assert_eq!(pk.with_name("pk").name.as_deref(), Some("pk"));
        let fk = ForeignKey::new(vec!["a".into()], "p", vec![]);
        assert_eq!(fk.on_update, ReferentialAction::NoAction);
        assert_eq!(fk.on_delete, ReferentialAction::default());
        assert!(fk.referenced_columns.is_empty() && fk.name.is_none());
        let fk = fk
            .with_name("fk")
            .with_referenced_schema("s")
            .with_on_update(ReferentialAction::Restrict)
            .with_on_delete(ReferentialAction::Cascade);
        assert_eq!(fk.name.as_deref(), Some("fk"));
        assert_eq!(fk.referenced_schema.as_deref(), Some("s"));
        assert_eq!(fk.referenced_table, "p");
        assert_eq!(fk.on_update, ReferentialAction::Restrict);
        assert_eq!(fk.on_delete, ReferentialAction::Cascade);
    }

    #[test]
    fn table_builders_and_column_lookup() {
        let obj = DbObject::new(ObjectKind::Table, ObjectPath::root(), "users");
        let t = Table::new(obj.clone());
        assert_eq!(t.object, obj);
        assert!(t.columns.is_empty() && t.primary_key.is_none() && t.foreign_keys.is_empty());
        assert!(t.column("id").is_none());
        let t = t
            .with_columns(vec![ColumnInfo::new("id", DataType::Integer)])
            .with_primary_key(PrimaryKey::new(vec!["id".into()]))
            .with_foreign_keys(vec![ForeignKey::new(vec!["id".into()], "o", vec![])]);
        assert_eq!(t.column("id").unwrap().data_type, DataType::Integer);
        assert!(t.column("nope").is_none());
        assert_eq!(t.primary_key.unwrap().columns, ["id"]);
        assert_eq!(t.foreign_keys.len(), 1);
    }

    #[test]
    fn listing_states() {
        assert_ne!(Listing::Unsupported, Listing::Objects(vec![]));
    }
}
