// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Introspection of the `MySQL` driver against the server of `SQLER_TEST_MYSQL_URL` (`MySQL` 8.4
//! or `MariaDB` 11.8). Without the variable it skips (ADR-0009).

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use common::{Server, connect, run, start};
use sqler_driver_api::sqler_core::{
    DataType, DbObject, Listing, ObjectKind, ObjectPath, Query, ReferentialAction,
};
use sqler_driver_api::{Introspect, Session, collect};

async fn exec(session: &mut dyn Session, text: &str) {
    let query = Query::new(text);
    let mut stream = session.execute(&query).await.unwrap();
    collect(&mut *stream).await.unwrap();
}

fn objects(listing: Listing) -> Vec<DbObject> {
    match listing {
        Listing::Objects(objects) => objects,
        Listing::Unsupported => panic!("unsupported"),
    }
}

fn names(listing: Listing) -> Vec<String> {
    objects(listing).into_iter().map(|o| o.name).collect()
}

#[test]
fn scenarios_on_server() {
    run(async {
        if let Some(server) = start().await {
            scenarios(&server).await;
        }
    });
}

async fn fixtures(session: &mut dyn Session) {
    for stmt in [
        "drop database if exists sqler_t_intro",
        "drop database if exists sqler_t_intro2",
        "create database sqler_t_intro",
        "create database sqler_t_intro2",
        "use sqler_t_intro",
        "create table sqler_t_intro.parent (id int primary key, label varchar(20) not null default 'x')",
        "create table sqler_t_intro.child (id int auto_increment primary key, parent_id int not null, \
         name varchar(20) default null, flag tinyint(1) default 0, \
         constraint fk_child_parent foreign key (parent_id) references parent (id) \
         on delete cascade on update restrict)",
        "create index idx_child_name on sqler_t_intro.child (name)",
        "create view sqler_t_intro.v_child as select id, name from sqler_t_intro.child",
        "create function sqler_t_intro.f_one() returns int deterministic return 1",
        "create procedure sqler_t_intro.p_noop() select 1",
        "create trigger sqler_t_intro.trg_child before insert on sqler_t_intro.child for each row \
         set new.name = upper(new.name)",
        "create table sqler_t_intro2.solo (a int)",
        "create function sqler_t_intro2.f_two() returns int deterministic return 2",
    ] {
        exec(session, stmt).await;
    }
}

async fn scenarios(server: &Server) {
    let mut session = connect(&server.config()).await;
    fixtures(&mut *session).await;
    let intro = session.introspection().expect("introspection");
    listings(intro).await;
    tables(intro).await;
    definitions(intro).await;
    drop(session);
}

fn app() -> ObjectPath {
    ObjectPath::root().in_schema("sqler_t_intro")
}

async fn listings(intro: &mut dyn Introspect) {
    for kind in [
        ObjectKind::Schema,
        ObjectKind::Table,
        ObjectKind::View,
        ObjectKind::Function,
        ObjectKind::Procedure,
        ObjectKind::Trigger,
        ObjectKind::Index,
        ObjectKind::Role,
    ] {
        assert!(intro.supports(kind), "{kind:?}");
    }
    for kind in [
        ObjectKind::Database,
        ObjectKind::MaterializedView,
        ObjectKind::Sequence,
        ObjectKind::Type,
        ObjectKind::Extension,
    ] {
        assert!(!intro.supports(kind), "{kind:?}");
        let listing = intro.list(&ObjectPath::root(), kind).await.unwrap();
        assert_eq!(listing, Listing::Unsupported, "{kind:?}");
    }

    // Schemas: the databases, in name order, at the root.
    let schemas = objects(
        intro
            .list(&ObjectPath::root(), ObjectKind::Schema)
            .await
            .unwrap(),
    );
    let schema_names: Vec<_> = schemas.iter().map(|o| o.name.as_str()).collect();
    for expected in [
        "sqler_t_intro",
        "sqler_t_intro2",
        "mysql",
        "information_schema",
    ] {
        assert!(
            schema_names.contains(&expected),
            "{expected} in {schema_names:?}"
        );
    }
    let mut sorted: Vec<_> = schema_names.iter().map(|n| n.to_lowercase()).collect();
    sorted.sort();
    assert_eq!(
        schema_names
            .iter()
            .map(|n| n.to_lowercase())
            .collect::<Vec<_>>(),
        sorted
    );
    assert!(schemas.iter().all(|o| o.parent == ObjectPath::root()));

    // Tables and views, by explicit schema and by the current database.
    let tables = objects(intro.list(&app(), ObjectKind::Table).await.unwrap());
    assert_eq!(
        tables.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        ["child", "parent"]
    );
    assert!(tables.iter().all(|o| o.parent == app()));
    assert_eq!(
        names(
            intro
                .list(&ObjectPath::root(), ObjectKind::Table)
                .await
                .unwrap()
        ),
        ["child", "parent"]
    );
    assert_eq!(
        names(
            intro
                .list(
                    &ObjectPath::root().in_schema("sqler_t_intro2"),
                    ObjectKind::Table
                )
                .await
                .unwrap()
        ),
        ["solo"]
    );
    assert_eq!(
        names(intro.list(&app(), ObjectKind::View).await.unwrap()),
        ["v_child"]
    );
    assert_eq!(
        names(
            intro
                .list(&ObjectPath::root().in_schema("nope"), ObjectKind::Table)
                .await
                .unwrap()
        ),
        Vec::<String>::new()
    );

    // Routines.
    assert_eq!(
        names(intro.list(&app(), ObjectKind::Function).await.unwrap()),
        ["f_one"]
    );
    assert_eq!(
        names(
            intro
                .list(
                    &ObjectPath::root().in_schema("sqler_t_intro2"),
                    ObjectKind::Function
                )
                .await
                .unwrap()
        ),
        ["f_two"]
    );
    assert_eq!(
        names(intro.list(&app(), ObjectKind::Procedure).await.unwrap()),
        ["p_noop"]
    );

    // Triggers and indexes carry their table, and a table narrows them.
    let triggers = objects(intro.list(&app(), ObjectKind::Trigger).await.unwrap());
    assert_eq!(triggers.len(), 1);
    assert_eq!(triggers[0].name, "trg_child");
    assert_eq!(triggers[0].parent, app().in_table("child"));
    assert_eq!(
        names(
            intro
                .list(&app().in_table("child"), ObjectKind::Trigger)
                .await
                .unwrap()
        ),
        ["trg_child"]
    );
    assert_eq!(
        names(
            intro
                .list(&app().in_table("parent"), ObjectKind::Trigger)
                .await
                .unwrap()
        ),
        Vec::<String>::new()
    );

    let indexes = objects(intro.list(&app(), ObjectKind::Index).await.unwrap());
    let pairs: Vec<_> = indexes
        .iter()
        .map(|o| (o.name.to_lowercase(), o.parent.table.clone().unwrap()))
        .collect();
    let mut sorted = pairs.clone();
    sorted.sort();
    assert_eq!(pairs, sorted);
    assert!(pairs.contains(&("idx_child_name".to_owned(), "child".to_owned())));
    assert!(pairs.contains(&("primary".to_owned(), "child".to_owned())));
    assert!(pairs.contains(&("primary".to_owned(), "parent".to_owned())));
    let narrowed = names(
        intro
            .list(&app().in_table("parent"), ObjectKind::Index)
            .await
            .unwrap(),
    );
    assert_eq!(narrowed, ["PRIMARY"]);

    // Accounts as user@host, or the current account.
    let roles = objects(
        intro
            .list(&ObjectPath::root(), ObjectKind::Role)
            .await
            .unwrap(),
    );
    assert!(
        roles.iter().any(|o| o.name.starts_with("root@")),
        "{roles:?}"
    );
    assert!(roles.iter().all(|o| o.parent == ObjectPath::root()));
}

async fn tables(intro: &mut dyn Introspect) {
    let child = DbObject::new(ObjectKind::Table, app(), "child");
    let table = intro.table(&child).await.unwrap().unwrap();
    let cols: Vec<_> = table
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type))
        .collect();
    assert_eq!(
        cols,
        [
            ("id", DataType::Integer),
            ("parent_id", DataType::Integer),
            ("name", DataType::Text),
            ("flag", DataType::Boolean),
        ]
    );
    assert!(
        table.columns[0]
            .native_type
            .as_deref()
            .unwrap()
            .starts_with("int")
    );
    assert_eq!(table.columns[2].native_type.as_deref(), Some("varchar(20)"));
    let nullable: Vec<_> = table.columns.iter().map(|c| c.nullable).collect();
    assert_eq!(nullable, [false, false, true, true]);
    assert_eq!(table.columns[3].default.as_deref(), Some("0"));
    assert_eq!(table.primary_key.as_ref().unwrap().columns, ["id"]);
    assert_eq!(table.foreign_keys.len(), 1);
    let fk = &table.foreign_keys[0];
    assert_eq!(fk.name.as_deref(), Some("fk_child_parent"));
    assert_eq!(fk.columns, ["parent_id"]);
    assert_eq!(fk.referenced_schema.as_deref(), Some("sqler_t_intro"));
    assert_eq!(fk.referenced_table, "parent");
    assert_eq!(fk.referenced_columns, ["id"]);
    assert_eq!(fk.on_delete, ReferentialAction::Cascade);
    assert_eq!(fk.on_update, ReferentialAction::Restrict);

    // The current database is used when the path has no schema.
    let parent = DbObject::new(ObjectKind::Table, ObjectPath::root(), "parent");
    let table = intro.table(&parent).await.unwrap().unwrap();
    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.foreign_keys.len(), 0);

    let view = DbObject::new(ObjectKind::View, app(), "v_child");
    let table = intro.table(&view).await.unwrap().unwrap();
    let cols: Vec<_> = table.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, ["id", "name"]);
    assert!(table.primary_key.is_none());

    for missing in [
        DbObject::new(ObjectKind::Table, app(), "nope"),
        DbObject::new(ObjectKind::Table, ObjectPath::root().in_schema("nodb"), "t"),
        DbObject::new(ObjectKind::Function, app(), "f_one"),
    ] {
        assert!(
            intro.table(&missing).await.unwrap().is_none(),
            "{missing:?}"
        );
    }
}

async fn definitions(intro: &mut dyn Introspect) {
    let cases = [
        (
            DbObject::new(ObjectKind::Schema, ObjectPath::root(), "sqler_t_intro"),
            "CREATE DATABASE",
        ),
        (
            DbObject::new(ObjectKind::Table, app(), "child"),
            "fk_child_parent",
        ),
        (DbObject::new(ObjectKind::View, app(), "v_child"), "v_child"),
        (DbObject::new(ObjectKind::Function, app(), "f_one"), "f_one"),
        (
            DbObject::new(ObjectKind::Procedure, app(), "p_noop"),
            "p_noop",
        ),
        (
            DbObject::new(ObjectKind::Trigger, app().in_table("child"), "trg_child"),
            "trg_child",
        ),
    ];
    for (object, needle) in cases {
        let text = intro.definition(&object).await.unwrap();
        let text = text.unwrap_or_else(|| panic!("no definition for {object:?}"));
        assert!(text.contains(needle), "{object:?}: {text}");
    }
    for none in [
        DbObject::new(ObjectKind::Index, app().in_table("child"), "idx_child_name"),
        DbObject::new(ObjectKind::Role, ObjectPath::root(), "root@%"),
        DbObject::new(ObjectKind::Table, app(), "nope"),
        DbObject::new(ObjectKind::Function, app(), "nope"),
        DbObject::new(ObjectKind::Procedure, app(), "nope"),
        DbObject::new(ObjectKind::Trigger, app(), "nope"),
        DbObject::new(ObjectKind::Table, ObjectPath::root().in_schema("nodb"), "t"),
        DbObject::new(ObjectKind::Schema, ObjectPath::root(), "nodb"),
        DbObject::new(ObjectKind::Sequence, app(), "s"),
    ] {
        assert!(intro.definition(&none).await.unwrap().is_none(), "{none:?}");
    }
}
