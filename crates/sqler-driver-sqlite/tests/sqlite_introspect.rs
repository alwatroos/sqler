// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Introspection of an in-memory `SQLite` database, through `dyn` objects only.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath, Query,
    ReferentialAction,
};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

async fn memory() -> Box<dyn Session> {
    let mut r = DriverRegistry::new();
    r.register(Box::new(SqliteDriver::new())).unwrap();
    let cfg = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
    r.connect(&cfg).await.unwrap()
}

async fn exec(s: &mut dyn Session, text: &str) {
    let q = Query::new(text);
    let mut stream = s.execute(&q).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

async fn names(s: &mut dyn Session, parent: &ObjectPath, kind: ObjectKind) -> Vec<String> {
    let intro = s.introspection().unwrap();
    match intro.list(parent, kind).await.unwrap() {
        Listing::Objects(o) => o.into_iter().map(|o| o.name).collect(),
        Listing::Unsupported => panic!("unsupported {kind:?}"),
    }
}

fn main_path() -> ObjectPath {
    ObjectPath::root().in_schema("main")
}

fn obj(kind: ObjectKind, name: &str) -> DbObject {
    DbObject::new(kind, main_path(), name)
}

#[test]
fn supports_matrix_agrees_with_list() {
    run(async {
        let mut s = memory().await;
        let intro = s.introspection().unwrap();
        let root = ObjectPath::root();
        for kind in ObjectKind::ALL {
            let expected = matches!(
                kind,
                ObjectKind::Schema
                    | ObjectKind::Table
                    | ObjectKind::View
                    | ObjectKind::Index
                    | ObjectKind::Trigger
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
fn schemas_include_main_and_attached() {
    run(async {
        let mut s = memory().await;
        let root = ObjectPath::root();
        let before = names(&mut *s, &root, ObjectKind::Schema).await;
        assert_eq!(before[0], "main");
        exec(&mut *s, "attach ':memory:' as aux").await;
        let after = names(&mut *s, &root, ObjectKind::Schema).await;
        assert!(after.contains(&"main".to_owned()));
        assert!(after.contains(&"aux".to_owned()));
        exec(&mut *s, "create table aux.only_aux (a integer)").await;
        let aux = ObjectPath::root().in_schema("aux");
        assert_eq!(names(&mut *s, &aux, ObjectKind::Table).await, ["only_aux"]);
        assert_eq!(
            names(&mut *s, &main_path(), ObjectKind::Table).await,
            Vec::<String>::new()
        );
    });
}

#[test]
fn internal_objects_are_hidden() {
    run(async {
        let mut s = memory().await;
        exec(
            &mut *s,
            "create table b (id integer primary key autoincrement, u text unique)",
        )
        .await;
        exec(&mut *s, "create table a (x integer)").await;
        exec(&mut *s, "insert into b (u) values ('x')").await;
        assert_eq!(
            names(&mut *s, &main_path(), ObjectKind::Table).await,
            ["a", "b"]
        );
        assert_eq!(
            names(&mut *s, &main_path(), ObjectKind::Index).await,
            Vec::<String>::new()
        );
    });
}

#[test]
fn views_indexes_and_triggers() {
    run(async {
        let mut s = memory().await;
        exec(&mut *s, "create table t1 (a integer, b text)").await;
        exec(&mut *s, "create table t2 (a integer)").await;
        exec(&mut *s, "create view v as select a from t1").await;
        exec(&mut *s, "create index i1 on t1 (a)").await;
        exec(&mut *s, "create index i2 on t2 (a)").await;
        exec(
            &mut *s,
            "create trigger tr1 after insert on t1 begin select 1; end",
        )
        .await;
        exec(
            &mut *s,
            "create trigger tr2 after insert on t2 begin select 1; end",
        )
        .await;
        assert_eq!(names(&mut *s, &main_path(), ObjectKind::View).await, ["v"]);
        assert_eq!(
            names(&mut *s, &main_path(), ObjectKind::Index).await,
            ["i1", "i2"]
        );
        let t1 = main_path().in_table("t1");
        assert_eq!(names(&mut *s, &t1, ObjectKind::Index).await, ["i1"]);
        assert_eq!(names(&mut *s, &t1, ObjectKind::Trigger).await, ["tr1"]);
        let intro = s.introspection().unwrap();
        let Listing::Objects(objs) = intro.list(&t1, ObjectKind::Index).await.unwrap() else {
            panic!("unsupported");
        };
        assert_eq!(objs[0].parent.table.as_deref(), Some("t1"));
        assert_eq!(objs[0].parent.schema.as_deref(), Some("main"));
        let Listing::Objects(all) = intro.list(&main_path(), ObjectKind::Trigger).await.unwrap()
        else {
            panic!("unsupported");
        };
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].parent.table.as_deref(), Some("t2"));
    });
}

#[test]
fn table_details() {
    run(async {
        let mut s = memory().await;
        exec(&mut *s, "create table p (id integer primary key, k text)").await;
        exec(
            &mut *s,
            "create table c (a integer not null, b varchar(10) default 'x', c, \
             d decimal(5,2), pid integer references p on delete cascade on update set null, \
             primary key (b, a))",
        )
        .await;
        exec(
            &mut *s,
            "create table m (x integer, y integer, z integer, \
             foreign key (x, y) references c (a, b) on delete restrict)",
        )
        .await;
        exec(&mut *s, "create view v as select a, b from c").await;
        let intro = s.introspection().unwrap();

        let t = intro
            .table(&obj(ObjectKind::Table, "c"))
            .await
            .unwrap()
            .unwrap();
        let cols: Vec<_> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["a", "b", "c", "d", "pid"]);
        let col_a = t.column("a").unwrap();
        assert!(!col_a.nullable);
        assert_eq!(col_a.data_type, DataType::Integer);
        assert_eq!(col_a.native_type.as_deref(), Some("INTEGER"));
        let col_b = t.column("b").unwrap();
        assert!(col_b.nullable);
        assert_eq!(col_b.default.as_deref(), Some("'x'"));
        assert_eq!(col_b.data_type, DataType::Text);
        let col_c = t.column("c").unwrap();
        assert_eq!(col_c.native_type, None);
        assert_eq!(col_c.data_type, DataType::Other);
        assert_eq!(t.column("d").unwrap().data_type, DataType::Decimal);
        let pk = t.primary_key.as_ref().unwrap();
        assert_eq!(pk.name, None);
        assert_eq!(pk.columns, ["b", "a"]);
        assert_eq!(t.foreign_keys.len(), 1);
        let fk = &t.foreign_keys[0];
        assert_eq!(fk.columns, ["pid"]);
        assert_eq!(fk.referenced_table, "p");
        assert_eq!(fk.referenced_schema.as_deref(), Some("main"));
        assert_eq!(fk.referenced_columns, Vec::<String>::new());
        assert_eq!(fk.on_delete, ReferentialAction::Cascade);
        assert_eq!(fk.on_update, ReferentialAction::SetNull);
        assert_eq!(fk.name, None);

        let child = intro
            .table(&obj(ObjectKind::Table, "m"))
            .await
            .unwrap()
            .unwrap();
        assert!(child.primary_key.is_none());
        let fk = &child.foreign_keys[0];
        assert_eq!(fk.columns, ["x", "y"]);
        assert_eq!(fk.referenced_columns, ["a", "b"]);
        assert_eq!(fk.on_delete, ReferentialAction::Restrict);
        assert_eq!(fk.on_update, ReferentialAction::NoAction);

        let view = intro
            .table(&obj(ObjectKind::View, "v"))
            .await
            .unwrap()
            .unwrap();
        let cols: Vec<_> = view.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, ["a", "b"]);
        assert!(view.primary_key.is_none());

        assert!(
            intro
                .table(&obj(ObjectKind::Table, "nope"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            intro
                .table(&obj(ObjectKind::Table, "v"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            intro
                .table(&obj(ObjectKind::Index, "c"))
                .await
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn definitions() {
    run(async {
        let mut s = memory().await;
        exec(&mut *s, "create table t (a integer unique)").await;
        exec(&mut *s, "create view v as select a from t").await;
        exec(&mut *s, "create index i on t (a)").await;
        exec(
            &mut *s,
            "create trigger tr after insert on t begin select 1; end",
        )
        .await;
        let intro = s.introspection().unwrap();
        for (kind, name, prefix) in [
            (ObjectKind::Table, "t", "CREATE TABLE"),
            (ObjectKind::View, "v", "CREATE VIEW"),
            (ObjectKind::Index, "i", "CREATE INDEX"),
            (ObjectKind::Trigger, "tr", "CREATE TRIGGER"),
        ] {
            let text = intro.definition(&obj(kind, name)).await.unwrap().unwrap();
            assert!(text.to_ascii_uppercase().starts_with(prefix), "{text}");
        }
        assert!(
            intro
                .definition(&obj(ObjectKind::Function, "f"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            intro
                .definition(&obj(ObjectKind::Table, "missing"))
                .await
                .unwrap()
                .is_none()
        );
        // The implicit unique index has no CREATE text.
        assert!(
            intro
                .definition(&obj(ObjectKind::Index, "sqlite_autoindex_t_1"))
                .await
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn awkward_table_name() {
    run(async {
        let mut s = memory().await;
        exec(
            &mut *s,
            "create table \"we\"\"ird t\" (a integer, \"b c\" text)",
        )
        .await;
        assert_eq!(
            names(&mut *s, &main_path(), ObjectKind::Table).await,
            ["we\"ird t"]
        );
        let intro = s.introspection().unwrap();
        let o = obj(ObjectKind::Table, "we\"ird t");
        let t = intro.table(&o).await.unwrap().unwrap();
        assert_eq!(t.columns.len(), 2);
        assert!(t.column("b c").is_some());
        assert!(intro.definition(&o).await.unwrap().is_some());
    });
}
