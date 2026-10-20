// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated introspection checks (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::sqler_core::{
    DataType, DbObject, Listing, ObjectKind, ObjectPath, ReferentialAction,
};
use sqler_driver_api::{DriverError, Session};

use common::{connect, fresh_schema, run, sql, url};

async fn fixture(s: &mut dyn Session, schema: &str) {
    fresh_schema(s, schema).await;
    for stmt in [
        "create table parent (id int primary key, name text not null default 'n')",
        "create table child (a int, b int, parent_id int references parent(id) \
         on delete cascade on update set null, tags text[], primary key (a, b))",
        "create view v as select id from parent",
        "create materialized view mv as select id from parent",
        "create function add(a int, b int) returns int language sql as 'select a + b'",
        "create function add(a numeric, b numeric) returns numeric language sql as 'select a + b'",
        "create procedure noop() language sql as 'select 1'",
        "create function trg() returns trigger language plpgsql as \
         'begin return new; end'",
        "create trigger parent_trg before insert on parent for each row execute function trg()",
        "create sequence seq",
        "create type mood as enum ('sad', 'happy')",
        "create domain posint as int check (value > 0)",
        "create index child_parent_idx on child (parent_id)",
    ] {
        sql(s, stmt).await;
    }
}

async fn names(s: &mut dyn Session, parent: &ObjectPath, kind: ObjectKind) -> Vec<String> {
    let intro = s.introspection().unwrap();
    assert!(intro.supports(kind));
    match intro.list(parent, kind).await.unwrap() {
        Listing::Objects(objects) => objects.into_iter().map(|o| o.name).collect(),
        Listing::Unsupported => panic!("{kind:?} unsupported"),
    }
}

async fn objects(s: &mut dyn Session, parent: &ObjectPath, kind: ObjectKind) -> Vec<DbObject> {
    match s.introspection().unwrap().list(parent, kind).await.unwrap() {
        Listing::Objects(objects) => objects,
        Listing::Unsupported => panic!("{kind:?} unsupported"),
    }
}

#[test]
fn lists_every_kind() {
    const SCHEMA: &str = "sqler_t_lists_every_kind";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fixture(&mut *s, SCHEMA).await;
        let root = ObjectPath::root();
        let in_schema = ObjectPath::root().in_schema(SCHEMA);

        let dbs = names(&mut *s, &root, ObjectKind::Database).await;
        assert!(dbs.contains(&"postgres".to_owned()));
        assert!(!dbs.iter().any(|d| d.starts_with("template")));
        let roles = names(&mut *s, &root, ObjectKind::Role).await;
        assert!(roles.contains(&"postgres".to_owned()));
        assert!(!roles.iter().any(|r| r.starts_with("pg_")));

        let schemas = names(&mut *s, &root, ObjectKind::Schema).await;
        let pos = |n: &str| schemas.iter().position(|x| x == n).unwrap();
        assert!(pos(SCHEMA) < pos("information_schema"));
        assert!(pos("public") < pos("pg_catalog"));
        assert!(
            !schemas
                .iter()
                .any(|x| x.starts_with("pg_toast") || x.starts_with("pg_temp"))
        );
        let schema_objs = objects(&mut *s, &root, ObjectKind::Schema).await;
        assert_eq!(
            schema_objs[0].parent,
            ObjectPath::root().in_database("postgres")
        );

        let exts = objects(&mut *s, &root, ObjectKind::Extension).await;
        assert!(exts.iter().any(|e| e.name == "plpgsql"));
        assert_eq!(exts[0].parent, ObjectPath::root().in_database("postgres"));

        assert_eq!(
            names(&mut *s, &in_schema, ObjectKind::Table).await,
            ["child", "parent"]
        );
        assert_eq!(names(&mut *s, &in_schema, ObjectKind::View).await, ["v"]);
        assert_eq!(
            names(&mut *s, &in_schema, ObjectKind::MaterializedView).await,
            ["mv"]
        );
        assert_eq!(
            names(&mut *s, &in_schema, ObjectKind::Sequence).await,
            ["seq"]
        );
        assert_eq!(
            names(&mut *s, &in_schema, ObjectKind::Function).await,
            [
                "add(a integer, b integer)",
                "add(a numeric, b numeric)",
                "trg()"
            ]
        );
        assert_eq!(
            names(&mut *s, &in_schema, ObjectKind::Procedure).await,
            ["noop()"]
        );
        let types = names(&mut *s, &in_schema, ObjectKind::Type).await;
        assert_eq!(types, ["mood", "posint"]);

        let triggers = objects(&mut *s, &in_schema, ObjectKind::Trigger).await;
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].name, "parent_trg");
        assert_eq!(
            triggers[0].parent,
            ObjectPath::root()
                .in_database("postgres")
                .in_schema(SCHEMA)
                .in_table("parent")
        );
        let narrowed = ObjectPath::root().in_schema(SCHEMA).in_table("child");
        assert_eq!(
            objects(&mut *s, &narrowed, ObjectKind::Trigger).await.len(),
            0
        );

        let indexes = names(&mut *s, &in_schema, ObjectKind::Index).await;
        assert_eq!(indexes, ["child_parent_idx", "child_pkey", "parent_pkey"]);
        let narrowed_idx = objects(&mut *s, &narrowed, ObjectKind::Index).await;
        assert_eq!(
            narrowed_idx
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>(),
            ["child_parent_idx", "child_pkey"]
        );
        assert_eq!(narrowed_idx[0].parent.table.as_deref(), Some("child"));

        // The default schema is the session's current one.
        let current = names(&mut *s, &root, ObjectKind::Table).await;
        assert_eq!(current, ["child", "parent"]);
    });
}

#[test]
fn describes_a_table() {
    const SCHEMA: &str = "sqler_t_describes_a_table";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fixture(&mut *s, SCHEMA).await;
        let object = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema(SCHEMA),
            "child",
        );
        let table = s
            .introspection()
            .unwrap()
            .table(&object)
            .await
            .unwrap()
            .unwrap();
        let cols: Vec<_> = table.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["a", "b", "parent_id", "tags"]);
        let a = table.column("a").unwrap();
        assert_eq!(a.data_type, DataType::Integer);
        assert_eq!(a.native_type.as_deref(), Some("integer"));
        assert!(!a.nullable);
        assert!(table.column("parent_id").unwrap().nullable);
        assert_eq!(table.column("tags").unwrap().data_type, DataType::Text);
        assert_eq!(
            table.column("tags").unwrap().native_type.as_deref(),
            Some("text[]")
        );
        let pk = table.primary_key.as_ref().unwrap();
        assert_eq!(pk.columns, ["a", "b"]);
        assert_eq!(pk.name.as_deref(), Some("child_pkey"));
        assert_eq!(table.foreign_keys.len(), 1);
        let fk = &table.foreign_keys[0];
        assert_eq!(fk.columns, ["parent_id"]);
        assert_eq!(fk.referenced_schema.as_deref(), Some(SCHEMA));
        assert_eq!(fk.referenced_table, "parent");
        assert_eq!(fk.referenced_columns, ["id"]);
        assert_eq!(fk.on_delete, ReferentialAction::Cascade);
        assert_eq!(fk.on_update, ReferentialAction::SetNull);

        let parent = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema(SCHEMA),
            "parent",
        );
        let table = s
            .introspection()
            .unwrap()
            .table(&parent)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            table.column("name").unwrap().default.as_deref(),
            Some("'n'::text")
        );

        // A view and a materialized view; other kinds and missing objects give None.
        for (kind, name) in [
            (ObjectKind::View, "v"),
            (ObjectKind::MaterializedView, "mv"),
        ] {
            let o = DbObject::new(kind, ObjectPath::root().in_schema(SCHEMA), name);
            let t = s.introspection().unwrap().table(&o).await.unwrap().unwrap();
            assert_eq!(t.columns.len(), 1);
        }
        let intro = s.introspection().unwrap();
        let wrong_kind = DbObject::new(
            ObjectKind::View,
            ObjectPath::root().in_schema(SCHEMA),
            "child",
        );
        assert!(intro.table(&wrong_kind).await.unwrap().is_none());
        let missing = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema(SCHEMA),
            "nope",
        );
        assert!(intro.table(&missing).await.unwrap().is_none());
        let seq = DbObject::new(
            ObjectKind::Sequence,
            ObjectPath::root().in_schema(SCHEMA),
            "seq",
        );
        assert!(intro.table(&seq).await.unwrap().is_none());
    });
}

#[test]
fn definitions() {
    const SCHEMA: &str = "sqler_t_definitions";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fixture(&mut *s, SCHEMA).await;
        let parent = ObjectPath::root().in_schema(SCHEMA);
        let mut def = async |kind: ObjectKind, path: &ObjectPath, name: &str| {
            let o = DbObject::new(kind, path.clone(), name);
            s.introspection().unwrap().definition(&o).await.unwrap()
        };
        let view_sql = def(ObjectKind::View, &parent, "v").await.unwrap();
        assert!(view_sql.contains("SELECT"), "{view_sql}");
        let matview_sql = def(ObjectKind::MaterializedView, &parent, "mv")
            .await
            .unwrap();
        assert!(matview_sql.contains("parent"), "{matview_sql}");
        let func_sql = def(ObjectKind::Function, &parent, "add(a numeric, b numeric)")
            .await
            .unwrap();
        assert!(
            func_sql.contains("CREATE OR REPLACE FUNCTION") && func_sql.contains("numeric"),
            "{func_sql}"
        );
        let proc_sql = def(ObjectKind::Procedure, &parent, "noop()").await.unwrap();
        assert!(
            proc_sql.contains("CREATE OR REPLACE PROCEDURE"),
            "{proc_sql}"
        );
        let trigger_sql = def(ObjectKind::Trigger, &parent, "parent_trg")
            .await
            .unwrap();
        assert!(
            trigger_sql.contains("CREATE TRIGGER parent_trg"),
            "{trigger_sql}"
        );
        let with_table = ObjectPath::root().in_schema(SCHEMA).in_table("parent");
        assert!(
            def(ObjectKind::Trigger, &with_table, "parent_trg")
                .await
                .is_some()
        );
        let other_table = ObjectPath::root().in_schema(SCHEMA).in_table("child");
        assert!(
            def(ObjectKind::Trigger, &other_table, "parent_trg")
                .await
                .is_none()
        );
        let index_sql = def(ObjectKind::Index, &parent, "child_parent_idx")
            .await
            .unwrap();
        assert!(
            index_sql.contains("CREATE INDEX child_parent_idx"),
            "{index_sql}"
        );
        // Kinds without server text, and missing objects.
        assert!(def(ObjectKind::Table, &parent, "child").await.is_none());
        assert!(def(ObjectKind::Sequence, &parent, "seq").await.is_none());
        assert!(
            def(ObjectKind::Function, &parent, "add(a integer)")
                .await
                .is_none()
        );
        assert!(def(ObjectKind::View, &parent, "missing").await.is_none());
    });
}

#[test]
fn another_database_is_unsupported() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let intro = s.introspection().unwrap();
        let other = ObjectPath::root().in_database("sqler_other_db");
        let err = intro.list(&other, ObjectKind::Schema).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::Unsupported(m)
                if m == "connected to database postgres; reconnect to browse sqler_other_db"),
            "{err:?}"
        );
        let table = DbObject::new(ObjectKind::Table, other.in_schema("public"), "t");
        assert!(matches!(
            intro.table(&table).await,
            Err(DriverError::Unsupported(_))
        ));
        // The current database by name works.
        let same = ObjectPath::root().in_database("postgres");
        assert!(intro.list(&same, ObjectKind::Schema).await.is_ok());
    });
}
