// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Decides whether a result may be edited, and on what terms.

use sqler_driver_api::sqler_core::{Column, DbObject, ObjectKind, ObjectLevel, Table};
use sqler_sql::{
    Dialect, SelectList, SqlIdent, same_column, single_table_source, unwritable_reason,
};

/// Where a result comes from.
///
/// `Debug` prints the kind only: the SQL text may hold literals.
#[derive(Clone, Copy)]
pub enum EditSource<'a> {
    /// A table-data tab showing `object`.
    TableData {
        /// The object that was opened.
        object: &'a DbObject,
    },
    /// The result of a query; it is analysed by [`single_table_source`].
    QueryResult {
        /// The SQL text that produced the result.
        sql: &'a str,
    },
}

impl std::fmt::Debug for EditSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TableData { object } => f
                .debug_struct("TableData")
                .field("kind", &object.kind)
                .finish_non_exhaustive(),
            Self::QueryResult { sql } => f
                .debug_struct("QueryResult")
                .field("sql_len", &sql.len())
                .finish(),
        }
    }
}

/// Everything [`check_editable`] looks at.
///
/// `Debug` prints counts and flags: the query text is not shown.
#[derive(Clone, Copy)]
pub struct EditRequest<'a> {
    /// The connection's dialect; `None` when unknown.
    pub dialect: Option<Dialect>,
    /// Where the result comes from.
    pub source: EditSource<'a>,
    /// The catalog entry of the source table, if loaded.
    pub table: Option<&'a Table>,
    /// Unique keys as table column names; the catalog lists none yet, so the caller supplies them.
    pub unique_keys: &'a [Vec<String>],
    /// The result's columns, in order.
    pub columns: &'a [Column],
    /// The user's explicit choice to edit a result that has no usable key.
    pub allow_keyless: bool,
    /// The schema (`MySQL`: database) the server resolves *the unqualified table name as written
    /// in this query* to. Needed only for a query that names its table without a schema.
    ///
    /// The caller must resolve it per query name, on the session that ran the query, right after
    /// it ran: `PostgreSQL` `to_regclass` of the name as written (so `pg_temp` is searched first
    /// and a temporary table shadows the loaded one), `SQLite` the first of `temp`, `main` and the
    /// attached databases in attach order that holds the name, `MySQL` `DATABASE()`. A
    /// session-level constant (`current_schema()`, a fixed `"main"`) is wrong exactly when a temp
    /// table shadows the loaded one, and the writes would then go to the table named in the plan,
    /// not the one the user sees. A query with an unqualified name is read-only
    /// ([`ReadOnlyReason::TableMismatch`]) unless this equals the loaded table's schema, and
    /// whenever it is `None`. A qualified name is compared with the table's schema instead.
    pub resolved_schema: Option<&'a str>,
}

impl std::fmt::Debug for EditRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditRequest")
            .field("dialect", &self.dialect)
            .field("source", &self.source)
            .field("table_loaded", &self.table.is_some())
            .field("columns", &self.columns.len())
            .field("allow_keyless", &self.allow_keyless)
            .field("resolved_schema", &self.resolved_schema)
            .finish_non_exhaustive()
    }
}

/// How a row is found again when the change is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RowIdentity {
    /// By the key columns (result column indices) and their original values.
    Key {
        /// The result column indices of the key, in key order.
        columns: Vec<usize>,
    },
    /// By all result columns and their original values (opt-in, keyless).
    AllColumns,
}

/// Why a result is read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadOnlyReason {
    /// The connection's dialect is unknown.
    NoDialect,
    /// The tab shows something that is not a base table.
    NotATable {
        /// What it is.
        kind: ObjectKind,
    },
    /// The query is not a plain `SELECT` from one table.
    NotSingleTable,
    /// The table's columns are not loaded.
    TableNotLoaded,
    /// The loaded table is not the one the query reads.
    TableMismatch,
    /// A result column is not a column of the table.
    UnmappedColumn {
        /// The result column.
        column: String,
    },
    /// A result column is not listed by the table catalog. On `SQLite` that is a generated or
    /// hidden column (`table_info` leaves them out), which the catalog cannot describe.
    HiddenColumn {
        /// The result column.
        column: String,
    },
    /// A value cannot be written to this column without the server changing it silently.
    UnwritableType {
        /// The table column.
        column: String,
        /// Why, from [`unwritable_reason`].
        reason: &'static str,
    },
    /// Two result columns are the same table column.
    DuplicateColumn {
        /// The table column.
        column: String,
    },
    /// The table has no primary key and no usable unique key.
    NoKey,
    /// The table has a key, but the result does not contain all of its columns.
    KeyNotInResult {
        /// The key columns that are missing from the result.
        missing: Vec<String>,
    },
}

impl ReadOnlyReason {
    /// A sentence that states the reason, for the user.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoDialect => "The connection's SQL dialect is unknown.".to_owned(),
            Self::NotATable { kind } => format!(
                "Only base tables can be edited, this is a {}.",
                kind.as_str().replace('_', " ")
            ),
            Self::NotSingleTable => "The query is not a plain SELECT from one table.".to_owned(),
            Self::TableNotLoaded => "The table's columns are not loaded yet.".to_owned(),
            Self::TableMismatch => "The loaded table is not the one the query reads.".to_owned(),
            Self::UnmappedColumn { column } => {
                format!("Column {column:?} is not a column of the table.")
            }
            Self::HiddenColumn { column } => format!(
                "Column {column:?} is not in the table's column list; on SQLite that is a \
                 generated or hidden column, and a table with one cannot be edited."
            ),
            Self::UnwritableType { column, reason } => {
                format!("Column {column:?} cannot be edited: {reason}.")
            }
            Self::DuplicateColumn { column } => {
                format!("Column {column:?} appears twice in the result.")
            }
            Self::NoKey => "The table has no primary key or non-null unique key.".to_owned(),
            Self::KeyNotInResult { missing } => {
                format!("The result lacks key column(s): {}.", missing.join(", "))
            }
        }
    }

    /// Whether the user may choose to edit anyway, matching rows on all their values.
    #[must_use]
    pub fn can_opt_in(&self) -> bool {
        matches!(self, Self::NoKey | Self::KeyNotInResult { .. })
    }
}

/// The outcome of [`check_editable`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Editability {
    /// The result may be edited; the target is the only way to build [`StagedEdits`].
    ///
    /// [`StagedEdits`]: super::StagedEdits
    Editable(EditTarget),
    /// The result is read-only, for this reason.
    ReadOnly(ReadOnlyReason),
}

/// A result that passed [`check_editable`]: the table, the row identity and the column mapping.
#[derive(Debug, Clone)]
pub struct EditTarget {
    dialect: Dialect,
    table: Box<Table>,
    identity: RowIdentity,
    /// For each result column, the index of its table column.
    mapping: Vec<usize>,
}

impl EditTarget {
    /// The dialect the statements are generated for.
    #[must_use]
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// The table the changes are written to.
    #[must_use]
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// How rows are identified.
    #[must_use]
    pub fn identity(&self) -> &RowIdentity {
        &self.identity
    }

    /// The number of result columns.
    #[must_use]
    pub fn width(&self) -> usize {
        self.mapping.len()
    }

    /// The catalog name of the table column behind result column `result_col`; empty if out of range.
    #[must_use]
    pub fn table_column(&self, result_col: usize) -> &str {
        self.mapping
            .get(result_col)
            .and_then(|&i| self.table.columns.get(i))
            .map_or("", |c| c.name.as_str())
    }
}

/// Decides whether the result described by `req` may be edited.
///
/// The order of the checks is fixed and the first failure is the reason; see ADR-0030.
#[must_use]
pub fn check_editable(req: &EditRequest<'_>) -> Editability {
    match check(req) {
        Ok(target) => Editability::Editable(target),
        Err(reason) => Editability::ReadOnly(reason),
    }
}

fn check(req: &EditRequest<'_>) -> Result<EditTarget, ReadOnlyReason> {
    let dialect = req.dialect.ok_or(ReadOnlyReason::NoDialect)?;
    let mut select_list = None;
    let mut source = None;
    match req.source {
        EditSource::TableData { object } => {
            if object.kind != ObjectKind::Table {
                return Err(ReadOnlyReason::NotATable { kind: object.kind });
            }
        }
        EditSource::QueryResult { sql } => {
            let found = single_table_source(dialect, sql).ok_or(ReadOnlyReason::NotSingleTable)?;
            select_list = Some(found.columns.clone());
            source = Some(found);
        }
    }
    let table = req.table.ok_or(ReadOnlyReason::TableNotLoaded)?;
    if table.object.kind != ObjectKind::Table {
        return Err(ReadOnlyReason::TableMismatch);
    }
    match (req.source, &source) {
        (EditSource::TableData { object }, _) => {
            if *object != table.object {
                return Err(ReadOnlyReason::TableMismatch);
            }
        }
        (_, Some(found))
            if !names_agree(
                dialect,
                table,
                &found.name,
                found.schema.as_ref(),
                req.resolved_schema,
            ) =>
        {
            return Err(ReadOnlyReason::TableMismatch);
        }
        _ => {}
    }
    if let Some(SelectList::Columns(list)) = &select_list
        && list.len() != req.columns.len()
    {
        return Err(ReadOnlyReason::NotSingleTable);
    }
    let mapping = map_columns(dialect, table, req.columns)?;
    for &i in &mapping {
        let column = &table.columns[i];
        if let Some(reason) = unwritable_reason(dialect, column) {
            return Err(ReadOnlyReason::UnwritableType {
                column: column.name.clone(),
                reason,
            });
        }
    }
    match find_key(table, req.unique_keys, &mapping) {
        Ok(columns) => Ok(target(
            dialect,
            table,
            RowIdentity::Key { columns },
            mapping,
        )),
        Err(reason) if req.allow_keyless && reason.can_opt_in() => {
            Ok(target(dialect, table, RowIdentity::AllColumns, mapping))
        }
        Err(reason) => Err(reason),
    }
}

fn target(
    dialect: Dialect,
    table: &Table,
    identity: RowIdentity,
    mapping: Vec<usize>,
) -> EditTarget {
    EditTarget {
        dialect,
        table: Box::new(table.clone()),
        identity,
        mapping,
    }
}

/// The schema (`MySQL`: schema or database) the catalog table lives in.
fn table_qualifier(dialect: Dialect, table: &Table) -> Option<&str> {
    if table.object.kind.level() != ObjectLevel::Schema {
        return None;
    }
    let path = &table.object.parent;
    match dialect {
        Dialect::MySql => path.schema.as_deref().or(path.database.as_deref()),
        _ => path.schema.as_deref(),
    }
}

/// Whether the names written in the query are the catalog table's.
///
/// A qualified name must match the table's schema. An unqualified one is resolved by the server,
/// which only the caller knows: it must have said so in `resolved_schema` (resolved per name, see
/// [`EditRequest`]), and that must be the table's own schema (exact catalog names on both sides). Without that, `SELECT * FROM t` could
/// read a `t` of another schema than the one that was loaded, and the writes would go there.
fn names_agree(
    dialect: Dialect,
    table: &Table,
    name: &SqlIdent,
    schema: Option<&SqlIdent>,
    resolved_schema: Option<&str>,
) -> bool {
    if !name.matches(dialect, &table.object.name) {
        return false;
    }
    let qualifier = table_qualifier(dialect, table);
    match schema {
        Some(schema) => qualifier.is_some_and(|q| schema.matches(dialect, q)),
        None => matches!((qualifier, resolved_schema), (Some(q), Some(d)) if q == d),
    }
}

/// Maps every result column to exactly one table column.
fn map_columns(
    dialect: Dialect,
    table: &Table,
    columns: &[Column],
) -> Result<Vec<usize>, ReadOnlyReason> {
    let mut mapping: Vec<usize> = Vec::with_capacity(columns.len());
    for column in columns {
        let hits: Vec<usize> = table
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| same_column(dialect, &column.name, &c.name))
            .map(|(i, _)| i)
            .collect();
        let index = match hits.as_slice() {
            [one] => *one,
            [] if dialect == Dialect::Sqlite => {
                return Err(ReadOnlyReason::HiddenColumn {
                    column: column.name.clone(),
                });
            }
            [] => {
                return Err(ReadOnlyReason::UnmappedColumn {
                    column: column.name.clone(),
                });
            }
            many => {
                let exact: Vec<usize> = many
                    .iter()
                    .copied()
                    .filter(|&i| table.columns[i].name == column.name)
                    .collect();
                match exact.as_slice() {
                    [one] => *one,
                    _ => {
                        return Err(ReadOnlyReason::UnmappedColumn {
                            column: column.name.clone(),
                        });
                    }
                }
            }
        };
        if mapping.contains(&index) {
            return Err(ReadOnlyReason::DuplicateColumn {
                column: table.columns[index].name.clone(),
            });
        }
        mapping.push(index);
    }
    Ok(mapping)
}

/// The result column indices of the key columns `names`, or the names that are missing.
fn locate(table: &Table, mapping: &[usize], names: &[String]) -> Result<Vec<usize>, Vec<String>> {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        let at = mapping
            .iter()
            .position(|&i| table.columns.get(i).is_some_and(|c| c.name == *name));
        match at {
            Some(at) => found.push(at),
            None => missing.push(name.clone()),
        }
    }
    if missing.is_empty() {
        Ok(found)
    } else {
        Err(missing)
    }
}

fn find_key(
    table: &Table,
    unique_keys: &[Vec<String>],
    mapping: &[usize],
) -> Result<Vec<usize>, ReadOnlyReason> {
    let mut first_missing: Option<Vec<String>> = None;
    let primary = table
        .primary_key
        .as_ref()
        .map(|pk| pk.columns.as_slice())
        .filter(|c| !c.is_empty());
    if let Some(columns) = primary {
        match locate(table, mapping, columns) {
            Ok(found) => return Ok(found),
            Err(missing) => first_missing = Some(missing),
        }
    }
    for key in unique_keys.iter().filter(|k| !k.is_empty()) {
        let not_null = key
            .iter()
            .all(|name| table.columns.iter().any(|c| c.name == *name && !c.nullable));
        if !not_null {
            continue;
        }
        match locate(table, mapping, key) {
            Ok(found) => return Ok(found),
            Err(missing) => {
                first_missing.get_or_insert(missing);
            }
        }
    }
    Err(match first_missing {
        Some(missing) => ReadOnlyReason::KeyNotInResult { missing },
        None => ReadOnlyReason::NoKey,
    })
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{ColumnInfo, DataType, ObjectPath, PrimaryKey};

    use super::*;

    #[test]
    fn request_debug_does_not_show_the_query_text() {
        let req = EditRequest {
            dialect: Some(Dialect::Sqlite),
            source: EditSource::QueryResult {
                sql: "SELECT * FROM t WHERE name = 'sekrit-literal'",
            },
            table: None,
            unique_keys: &[],
            columns: &[],
            allow_keyless: false,
            resolved_schema: None,
        };
        let shown = format!("{req:?} {:?}", req.source);
        assert!(!shown.contains("sekrit"), "{shown}");
        assert!(!shown.contains("SELECT"), "{shown}");
    }

    fn object(kind: ObjectKind) -> DbObject {
        DbObject::new(kind, ObjectPath::root().in_schema("public"), "t")
    }

    fn table() -> Table {
        Table::new(object(ObjectKind::Table))
            .with_columns(vec![
                ColumnInfo::new("id", DataType::Integer).with_nullable(false),
                ColumnInfo::new("name", DataType::Text),
                ColumnInfo::new("code", DataType::Text).with_nullable(false),
            ])
            .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]))
    }

    fn cols(names: &[&str]) -> Vec<Column> {
        names
            .iter()
            .map(|n| Column::new(*n, DataType::Text))
            .collect()
    }

    fn run(
        dialect: Option<Dialect>,
        sql: &str,
        table: Option<&Table>,
        unique: &[Vec<String>],
        columns: &[Column],
        keyless: bool,
    ) -> Editability {
        check_editable(&EditRequest {
            dialect,
            source: EditSource::QueryResult { sql },
            table,
            unique_keys: unique,
            columns,
            allow_keyless: keyless,
            resolved_schema: Some("public"),
        })
    }

    fn reason(e: Editability) -> ReadOnlyReason {
        match e {
            Editability::ReadOnly(r) => r,
            other => panic!("expected read-only, got {other:?}"),
        }
    }

    fn target_of(e: Editability) -> EditTarget {
        match e {
            Editability::Editable(t) => t,
            other => panic!("expected editable, got {other:?}"),
        }
    }

    const PG: Option<Dialect> = Some(Dialect::Postgres);

    #[test]
    fn table_data_with_key_is_editable() {
        let t = table();
        let o = object(ObjectKind::Table);
        let c = cols(&["id", "name"]);
        let e = check_editable(&EditRequest {
            dialect: PG,
            source: EditSource::TableData { object: &o },
            table: Some(&t),
            unique_keys: &[],
            columns: &c,
            allow_keyless: false,
            resolved_schema: None,
        });
        let target = target_of(e);
        assert_eq!(*target.identity(), RowIdentity::Key { columns: vec![0] });
        assert_eq!(target.table_column(1), "name");
        assert_eq!(target.table_column(9), "");
        assert_eq!(target.width(), 2);
    }

    #[test]
    fn view_and_other_object_are_not_tables() {
        let t = table();
        let o = object(ObjectKind::View);
        let c = cols(&["id"]);
        let e = check_editable(&EditRequest {
            dialect: PG,
            source: EditSource::TableData { object: &o },
            table: Some(&t),
            unique_keys: &[],
            columns: &c,
            allow_keyless: true,
            resolved_schema: None,
        });
        assert_eq!(
            reason(e),
            ReadOnlyReason::NotATable {
                kind: ObjectKind::View
            }
        );
    }

    #[test]
    fn table_data_for_another_object_is_a_mismatch() {
        let t = table();
        let o = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            "u",
        );
        let c = cols(&["id"]);
        let e = check_editable(&EditRequest {
            dialect: PG,
            source: EditSource::TableData { object: &o },
            table: Some(&t),
            unique_keys: &[],
            columns: &c,
            allow_keyless: false,
            resolved_schema: None,
        });
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
    }

    #[test]
    fn no_dialect_comes_first() {
        let t = table();
        let c = cols(&["id"]);
        let e = run(None, "SELECT * FROM t", Some(&t), &[], &c, true);
        assert_eq!(reason(e), ReadOnlyReason::NoDialect);
    }

    #[test]
    fn a_t_sql_connection_is_not_refused_for_its_dialect() {
        let t = table();
        let c = cols(&["id", "name", "code"]);
        for sql in [
            "SELECT * FROM [t]",
            "SELECT TOP (10) * FROM t",
            "SELECT * FROM [public].[t]",
        ] {
            let e = run(Some(Dialect::TSql), sql, Some(&t), &[], &c, false);
            assert!(matches!(e, Editability::Editable(_)), "{sql}: {e:?}");
        }
    }

    #[test]
    fn query_shapes() {
        let t = table();
        let c = cols(&["id", "name", "code"]);
        for sql in [
            "SELECT * FROM t",
            "select id, name, code from public.t where id > 1 order by id limit 5;",
            "SELECT \"id\", \"name\", \"code\" FROM \"public\".\"t\"",
        ] {
            let e = run(PG, sql, Some(&t), &[], &c, false);
            assert!(matches!(e, Editability::Editable(_)), "{sql}");
        }
        for sql in [
            "SELECT * FROM t JOIN u ON t.id = u.id",
            "SELECT count(*) FROM t",
            "SELECT * FROM a, b",
            "SELECT * FROM t GROUP BY id",
            "DELETE FROM t",
            "SELECT 1; SELECT 2",
        ] {
            let e = run(PG, sql, Some(&t), &[], &c, true);
            assert_eq!(reason(e), ReadOnlyReason::NotSingleTable, "{sql}");
        }
    }

    #[test]
    fn select_list_must_match_result_width() {
        let t = table();
        let c = cols(&["id", "name", "code"]);
        let e = run(PG, "SELECT id, name FROM t", Some(&t), &[], &c, false);
        assert_eq!(reason(e), ReadOnlyReason::NotSingleTable);
    }

    #[test]
    fn table_must_be_loaded_and_agree() {
        let t = table();
        let c = cols(&["id"]);
        let e = run(PG, "SELECT id FROM t", None, &[], &c, false);
        assert_eq!(reason(e), ReadOnlyReason::TableNotLoaded);
        for sql in [
            "SELECT id FROM other",
            "SELECT id FROM other.t",
            "SELECT id FROM \"T\"",
        ] {
            let e = run(PG, sql, Some(&t), &[], &c, true);
            assert_eq!(reason(e), ReadOnlyReason::TableMismatch, "{sql}");
        }
        // A view in the catalog is never a target, even when the names agree.
        let view = Table::new(object(ObjectKind::View));
        let e = run(PG, "SELECT id FROM t", Some(&view), &[], &c, true);
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
    }

    fn with_resolved(sql: &str, t: &Table, resolved_schema: Option<&str>) -> Editability {
        let c = cols(&["id"]);
        check_editable(&EditRequest {
            dialect: PG,
            source: EditSource::QueryResult { sql },
            table: Some(t),
            unique_keys: &[],
            columns: &c,
            allow_keyless: false,
            resolved_schema,
        })
    }

    #[test]
    fn unqualified_name_needs_the_servers_resolution() {
        let t = table();
        // The loaded table is public.t. The server resolves `t` to the schema in resolved_schema.
        let e = with_resolved("SELECT id FROM t", &t, Some("public"));
        assert!(matches!(e, Editability::Editable(_)));
        // Another schema (search_path, pg_temp, ...): the query would read some other `t`.
        let e = with_resolved("SELECT id FROM t", &t, Some("other"));
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        let e = with_resolved("SELECT id FROM t", &t, Some("pg_temp"));
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        // Unknown resolution: read-only.
        let e = with_resolved("SELECT id FROM t", &t, None);
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        // The comparison is exact on the catalog name.
        let e = with_resolved("SELECT id FROM t", &t, Some("PUBLIC"));
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        // A table without a schema cannot be proven to be the one the server resolves.
        let bare = Table::new(DbObject::new(ObjectKind::Table, ObjectPath::root(), "t"))
            .with_columns(vec![
                ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ])
            .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]));
        let e = with_resolved("SELECT id FROM t", &bare, Some("public"));
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        // A qualified name is compared with the table's schema; resolved_schema is irrelevant.
        for default in [None, Some("other")] {
            let e = with_resolved("SELECT id FROM public.t", &t, default);
            assert!(matches!(e, Editability::Editable(_)));
            let e = with_resolved("SELECT id FROM other.t", &t, default);
            assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
        }
        // Other dialects: SQLite schema `main`, MySQL database.
        let sqlite = Table::new(DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("main"),
            "t",
        ))
        .with_columns(vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
        ])
        .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]));
        let c = cols(&["id"]);
        for (d, sql, def, ok) in [
            (Dialect::Sqlite, "SELECT id FROM t", Some("main"), true),
            (Dialect::Sqlite, "SELECT id FROM t", Some("temp"), false),
            (Dialect::Sqlite, "SELECT id FROM t", None, false),
        ] {
            let e = check_editable(&EditRequest {
                dialect: Some(d),
                source: EditSource::QueryResult { sql },
                table: Some(&sqlite),
                unique_keys: &[],
                columns: &c,
                allow_keyless: false,
                resolved_schema: def,
            });
            assert_eq!(matches!(e, Editability::Editable(_)), ok, "{d:?} {def:?}");
        }
        let shop = Table::new(DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_database("shop"),
            "t",
        ))
        .with_columns(vec![ColumnInfo::new("id", DataType::Integer)])
        .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]));
        for (def, ok) in [(Some("shop"), true), (Some("other"), false), (None, false)] {
            let e = check_editable(&EditRequest {
                dialect: Some(Dialect::MySql),
                source: EditSource::QueryResult {
                    sql: "SELECT id FROM t",
                },
                table: Some(&shop),
                unique_keys: &[],
                columns: &c,
                allow_keyless: false,
                resolved_schema: def,
            });
            assert_eq!(matches!(e, Editability::Editable(_)), ok, "{def:?}");
        }
    }

    #[test]
    fn column_mapping_rules() {
        let t = table();
        let e = run(
            PG,
            "SELECT * FROM t",
            Some(&t),
            &[],
            &cols(&["id", "x"]),
            true,
        );
        assert_eq!(
            reason(e),
            ReadOnlyReason::UnmappedColumn {
                column: "x".to_owned()
            }
        );
        // PostgreSQL is exact, SQLite is not.
        let e = run(PG, "SELECT * FROM t", Some(&t), &[], &cols(&["ID"]), true);
        assert!(matches!(reason(e), ReadOnlyReason::UnmappedColumn { .. }));
        let s = Some(Dialect::Sqlite);
        let e = run(s, "SELECT * FROM t", Some(&t), &[], &cols(&["ID"]), false);
        assert!(matches!(e, Editability::Editable(_)));
        let e = run(
            s,
            "SELECT * FROM t",
            Some(&t),
            &[],
            &cols(&["id", "ID"]),
            true,
        );
        assert_eq!(
            reason(e),
            ReadOnlyReason::DuplicateColumn {
                column: "id".to_owned()
            }
        );
    }

    #[test]
    fn sqlite_column_missing_from_the_catalog_is_reported_as_generated_or_hidden() {
        let t = table();
        let s = Some(Dialect::Sqlite);
        let e = run(
            s,
            "SELECT * FROM t",
            Some(&t),
            &[],
            &cols(&["id", "g"]),
            true,
        );
        let r = reason(e);
        assert_eq!(
            r,
            ReadOnlyReason::HiddenColumn {
                column: "g".to_owned()
            }
        );
        assert!(r.message().contains("generated"));
        assert!(!r.can_opt_in());
    }

    #[test]
    fn columns_that_cannot_be_written_safely_make_the_result_read_only() {
        let mut t = table();
        t.columns
            .push(ColumnInfo::new("c", DataType::Other).with_native_type("\"char\""));
        t.columns
            .push(ColumnInfo::new("iv", DataType::Text).with_native_type("interval year"));
        let e = run(
            PG,
            "SELECT id, c FROM t",
            Some(&t),
            &[],
            &cols(&["id", "c"]),
            false,
        );
        let r = reason(e);
        assert!(matches!(&r, ReadOnlyReason::UnwritableType { column, .. } if column == "c"));
        assert!(r.message().contains("\"c\""));
        let e = run(
            PG,
            "SELECT id, iv FROM t",
            Some(&t),
            &[],
            &cols(&["id", "iv"]),
            true,
        );
        assert!(matches!(reason(e), ReadOnlyReason::UnwritableType { .. }));
        // A result that leaves the column out is still editable.
        let e = run(
            PG,
            "SELECT id, name FROM t",
            Some(&t),
            &[],
            &cols(&["id", "name"]),
            false,
        );
        assert!(matches!(e, Editability::Editable(_)));
    }

    #[test]
    fn no_key_and_key_not_in_result() {
        let mut t = table();
        t.primary_key = None;
        let c = cols(&["id", "name"]);
        let e = run(PG, "SELECT * FROM t", Some(&t), &[], &c, false);
        let r = reason(e);
        assert_eq!(r, ReadOnlyReason::NoKey);
        assert!(r.can_opt_in());
        assert_eq!(
            r.message(),
            "The table has no primary key or non-null unique key."
        );

        let t = table();
        let c = cols(&["name"]);
        let e = run(PG, "SELECT name FROM t", Some(&t), &[], &c, false);
        let r = reason(e);
        assert_eq!(
            r,
            ReadOnlyReason::KeyNotInResult {
                missing: vec!["id".to_owned()]
            }
        );
        assert!(r.can_opt_in());
        assert!(r.message().contains("id"));
    }

    #[test]
    fn unique_key_must_be_not_null_and_present() {
        let mut t = table();
        t.primary_key = None;
        let c = cols(&["name", "code"]);
        // `name` is nullable: never a row identity.
        let e = run(
            PG,
            "SELECT name, code FROM t",
            Some(&t),
            &[vec!["name".to_owned()]],
            &c,
            false,
        );
        assert_eq!(reason(e), ReadOnlyReason::NoKey);
        let e = run(
            PG,
            "SELECT name, code FROM t",
            Some(&t),
            &[vec!["name".to_owned()], vec!["code".to_owned()]],
            &c,
            false,
        );
        let target = target_of(e);
        assert_eq!(*target.identity(), RowIdentity::Key { columns: vec![1] });
    }

    #[test]
    fn opt_in_uses_all_columns_only_for_key_reasons() {
        let mut t = table();
        t.primary_key = None;
        let c = cols(&["id", "name"]);
        let e = run(PG, "SELECT * FROM t", Some(&t), &[], &c, true);
        let target = target_of(e);
        assert_eq!(*target.identity(), RowIdentity::AllColumns);
        // Joins and views never opt in.
        let e = run(
            PG,
            "SELECT * FROM t JOIN u ON true",
            Some(&t),
            &[],
            &c,
            true,
        );
        assert_eq!(reason(e), ReadOnlyReason::NotSingleTable);
        let e = run(PG, "SELECT * FROM t", Some(&t), &[], &cols(&["zzz"]), true);
        assert!(matches!(reason(e), ReadOnlyReason::UnmappedColumn { .. }));
        // A usable key is preferred over the opt-in.
        let t = table();
        let e = run(PG, "SELECT * FROM t", Some(&t), &[], &c, true);
        assert!(matches!(*target_of(e).identity(), RowIdentity::Key { .. }));
    }

    #[test]
    fn mysql_schema_qualifier_is_the_database() {
        let t = Table::new(DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_database("shop"),
            "t",
        ))
        .with_columns(vec![ColumnInfo::new("id", DataType::Integer)])
        .with_primary_key(PrimaryKey::new(vec!["id".to_owned()]));
        let m = Some(Dialect::MySql);
        let c = cols(&["id"]);
        let e = run(m, "SELECT id FROM `shop`.`t`", Some(&t), &[], &c, false);
        assert!(matches!(e, Editability::Editable(_)));
        let e = run(m, "SELECT id FROM `other`.`t`", Some(&t), &[], &c, false);
        assert_eq!(reason(e), ReadOnlyReason::TableMismatch);
    }
}
