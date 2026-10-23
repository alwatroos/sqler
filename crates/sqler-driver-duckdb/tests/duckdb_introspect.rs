// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Introspection of an in-memory `DuckDB` database, through `dyn` objects only.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query,
};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_duckdb::{DRIVER_ID, DuckDbDriver};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

async fn exec(s: &mut dyn Session, text: &str) {
    let q = Query::new(text);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

/// A database with schema `s`: tables `parent` and `child` (PK, FK), view, index and sequence.
async fn fixture() -> Box<dyn Session> {
    let mut r = DriverRegistry::new();
    r.register(Box::new(DuckDbDriver::new())).unwrap();
    let cfg = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
    let mut s = r.connect(&cfg).await.unwrap();
    for text in [
        "create schema s",
        "create table s.parent (id integer primary key, label varchar not null default 'x')",
        "create table s.child (a integer, b integer, parent_id integer, \
         primary key (a, b), foreign key (parent_id) references s.parent (id))",
        "create table s.another (v decimal(10,2))",
        "create view s.v_parent as select id from s.parent",
        "create index idx_child_parent on s.child (parent_id)",
        "create sequence s.seq",
    ] {
        exec(&mut *s, text).await;
    }
    s
}

fn schema_path() -> ObjectPath {
    ObjectPath::root().in_schema("s")
}

async fn objects(s: &mut dyn Session, parent: &ObjectPath, kind: ObjectKind) -> Vec<DbObject> {
    match s.introspection().unwrap().list(parent, kind).await.unwrap() {
        Listing::Objects(o) => o,
        Listing::Unsupported => panic!("unsupported {kind:?}"),
    }
}

async fn names(s: &mut dyn Session, parent: &ObjectPath, kind: ObjectKind) -> Vec<String> {
    objects(s, parent, kind)
        .await
        .into_iter()
        .map(|o| o.name)
        .collect()
}

#[test]
fn supports_matrix_agrees_with_list() {
    run(async {
        let mut s = fixture().await;
        let intro = s.introspection().unwrap();
        let root = ObjectPath::root();
        for kind in ObjectKind::ALL {
            let expected = matches!(
                kind,
                ObjectKind::Database
                    | ObjectKind::Schema
                    | ObjectKind::Table
                    | ObjectKind::View
                    | ObjectKind::Index
                    | ObjectKind::Sequence
            );
            assert_eq!(intro.supports(kind), expected, "{kind:?}");
            let listing = intro.list(&root, kind).await.unwrap();
            assert_eq!(
                matches!(listing, Listing::Unsupported),
                !expected,
                "{kind:?}"
            );
        }
    });
}

#[test]
fn lists_databases_and_schemas() {
    run(async {
        let mut s = fixture().await;
        let dbs = names(&mut *s, &ObjectPath::root(), ObjectKind::Database).await;
        assert_eq!(dbs.len(), 1, "{dbs:?}");
        let schemas = names(&mut *s, &ObjectPath::root(), ObjectKind::Schema).await;
        assert!(schemas.contains(&"main".to_owned()), "{schemas:?}");
        assert!(schemas.contains(&"s".to_owned()), "{schemas:?}");
        assert!(
            !schemas.contains(&"information_schema".to_owned()),
            "{schemas:?}"
        );
        let objs = objects(&mut *s, &ObjectPath::root(), ObjectKind::Schema).await;
        assert_eq!(objs[0].parent.database.as_deref(), Some(dbs[0].as_str()));
    });
}

#[test]
fn lists_tables_views_indexes_sequences_in_name_order() {
    run(async {
        let mut s = fixture().await;
        let p = schema_path();
        assert_eq!(
            names(&mut *s, &p, ObjectKind::Table).await,
            ["another", "child", "parent"]
        );
        assert_eq!(names(&mut *s, &p, ObjectKind::View).await, ["v_parent"]);
        assert_eq!(
            names(&mut *s, &p, ObjectKind::Index).await,
            ["idx_child_parent"]
        );
        assert_eq!(names(&mut *s, &p, ObjectKind::Sequence).await, ["seq"]);
        let idx = objects(&mut *s, &p, ObjectKind::Index).await;
        assert_eq!(idx[0].parent.table.as_deref(), Some("child"));
        let narrowed = p.clone().in_table("parent");
        assert_eq!(names(&mut *s, &narrowed, ObjectKind::Index).await.len(), 0);
        let narrowed = p.in_table("child");
        assert_eq!(
            names(&mut *s, &narrowed, ObjectKind::Index).await,
            ["idx_child_parent"]
        );
        // `None` schema is the current one (`main`), which holds none of these.
        assert_eq!(
            names(&mut *s, &ObjectPath::root(), ObjectKind::Table)
                .await
                .len(),
            0
        );
    });
}

#[test]
fn unsupported_kinds() {
    run(async {
        let mut s = fixture().await;
        let intro = s.introspection().unwrap();
        for kind in [
            ObjectKind::MaterializedView,
            ObjectKind::Function,
            ObjectKind::Procedure,
            ObjectKind::Trigger,
            ObjectKind::Type,
            ObjectKind::Extension,
            ObjectKind::Role,
        ] {
            assert_eq!(
                intro.list(&schema_path(), kind).await.unwrap(),
                Listing::Unsupported,
                "{kind:?}"
            );
        }
    });
}

#[test]
fn table_columns_primary_and_foreign_keys() {
    run(async {
        let mut s = fixture().await;
        let intro = s.introspection().unwrap();

        let parent = DbObject::new(ObjectKind::Table, schema_path(), "parent");
        let t = intro.table(&parent).await.unwrap().unwrap();
        let cols: Vec<_> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["id", "label"]);
        assert_eq!(t.columns[0].data_type, DataType::Integer);
        assert_eq!(t.columns[0].native_type.as_deref(), Some("INTEGER"));
        assert!(!t.columns[0].nullable);
        assert!(!t.columns[1].nullable);
        assert!(t.columns[1].default.as_deref().unwrap().contains('x'));
        assert_eq!(t.primary_key.unwrap().columns, ["id"]);
        assert_eq!(t.foreign_keys.len(), 0);

        let child = DbObject::new(ObjectKind::Table, schema_path(), "child");
        let t = intro.table(&child).await.unwrap().unwrap();
        assert_eq!(t.primary_key.unwrap().columns, ["a", "b"]);
        assert_eq!(t.foreign_keys.len(), 1);
        let fk = &t.foreign_keys[0];
        assert_eq!(fk.columns, ["parent_id"]);
        assert_eq!(fk.referenced_table, "parent");
        assert_eq!(fk.referenced_columns, ["id"]);

        let another = DbObject::new(ObjectKind::Table, schema_path(), "another");
        let t = intro.table(&another).await.unwrap().unwrap();
        assert_eq!(t.columns[0].data_type, DataType::Decimal);
        assert_eq!(t.columns[0].native_type.as_deref(), Some("DECIMAL(10,2)"));
        assert!(t.primary_key.is_none());

        let view = DbObject::new(ObjectKind::View, schema_path(), "v_parent");
        let t = intro.table(&view).await.unwrap().unwrap();
        assert_eq!(t.columns.len(), 1);

        let missing = DbObject::new(ObjectKind::Table, schema_path(), "nope");
        assert!(intro.table(&missing).await.unwrap().is_none());
        let wrong_kind = DbObject::new(ObjectKind::Sequence, schema_path(), "seq");
        assert!(intro.table(&wrong_kind).await.unwrap().is_none());
    });
}

#[test]
fn definitions() {
    run(async {
        let mut s = fixture().await;
        let intro = s.introspection().unwrap();
        let view = DbObject::new(ObjectKind::View, schema_path(), "v_parent");
        let sql = intro.definition(&view).await.unwrap().unwrap();
        assert!(sql.to_ascii_lowercase().contains("select"), "{sql}");
        let table = DbObject::new(ObjectKind::Table, schema_path(), "parent");
        let sql = intro.definition(&table).await.unwrap().unwrap();
        assert!(sql.to_ascii_lowercase().contains("create table"), "{sql}");
        let index = DbObject::new(ObjectKind::Index, schema_path(), "idx_child_parent");
        assert!(intro.definition(&index).await.unwrap().is_some());
        let seq = DbObject::new(ObjectKind::Sequence, schema_path(), "seq");
        assert!(intro.definition(&seq).await.unwrap().is_some());
        let none = DbObject::new(ObjectKind::Function, schema_path(), "f");
        assert!(intro.definition(&none).await.unwrap().is_none());
    });
}

#[test]
fn hostile_names_are_bound_not_spliced() {
    run(async {
        let mut s = fixture().await;
        let intro = s.introspection().unwrap();
        let evil = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("s' or '1'='1"),
            "parent' or '1'='1",
        );
        assert!(intro.table(&evil).await.unwrap().is_none());
    });
}
