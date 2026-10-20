// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `SQLite` scripts of `sqler-sql`, generated from introspected objects and executed against
//! an in-memory database.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DbObject, DriverId, ObjectKind, ObjectPath, Query,
};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::{
    DEFAULT_ROW_LIMIT, Dialect, create_script, create_table, drop_script, insert_template,
    select_star,
};

const D: Dialect = Dialect::Sqlite;

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

/// Runs one statement and panics with the statement text if the server rejects it.
async fn exec(s: &mut dyn Session, text: &str) {
    let q = Query::new(text);
    let mut stream = s
        .execute(&q)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
    collect(&mut *stream)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{text}"));
}

fn obj(kind: ObjectKind, name: &str) -> DbObject {
    DbObject::new(kind, ObjectPath::root().in_schema("main"), name)
}

fn on_table(kind: ObjectKind, name: &str, table: &str) -> DbObject {
    DbObject::new(
        kind,
        ObjectPath::root().in_schema("main").in_table(table),
        name,
    )
}

#[test]
fn generated_scripts_run() {
    run(async {
        let mut s = memory().await;
        exec(
            &mut *s,
            "create table \"us ers\" (id integer primary key, n text)",
        )
        .await;
        exec(
            &mut *s,
            "create table \"or\"\"ders\" (id integer not null, \"user id\" integer, \
             primary key (id), foreign key (\"user id\") references \"us ers\" (id) on delete cascade)",
        )
        .await;
        exec(
            &mut *s,
            "create index \"ix o\" on \"or\"\"ders\" (\"user id\")",
        )
        .await;
        exec(
            &mut *s,
            "create view \"v w\" as select id from \"or\"\"ders\"",
        )
        .await;
        exec(
            &mut *s,
            "create trigger \"tr g\" after insert on \"or\"\"ders\" begin select 1; end",
        )
        .await;

        let table_obj = obj(ObjectKind::Table, "or\"ders");
        let intro = s.introspection().unwrap();
        let table = intro.table(&table_obj).await.unwrap().unwrap();
        assert_eq!(table.foreign_keys.len(), 1);
        let index_text = intro
            .definition(&on_table(ObjectKind::Index, "ix o", "or\"ders"))
            .await
            .unwrap()
            .unwrap();
        let view_text = intro
            .definition(&obj(ObjectKind::View, "v w"))
            .await
            .unwrap()
            .unwrap();
        let trigger_text = intro
            .definition(&on_table(ObjectKind::Trigger, "tr g", "or\"ders"))
            .await
            .unwrap()
            .unwrap();

        // Drop everything, then rebuild it from the generated scripts.
        for object in [
            on_table(ObjectKind::Trigger, "tr g", "or\"ders"),
            obj(ObjectKind::View, "v w"),
            on_table(ObjectKind::Index, "ix o", "or\"ders"),
            table_obj.clone(),
        ] {
            exec(&mut *s, &drop_script(D, &object).unwrap()).await;
        }

        let create = create_table(D, &table, &[index_text]).unwrap();
        assert!(
            create.contains("REFERENCES \"us ers\" (\"id\")"),
            "{create}"
        );
        for statement in create.split(";\n").filter(|t| !t.trim().is_empty()) {
            exec(&mut *s, statement).await;
        }
        exec(
            &mut *s,
            &create_script(D, &obj(ObjectKind::View, "v w"), Some(&view_text)).unwrap(),
        )
        .await;
        exec(
            &mut *s,
            &create_script(
                D,
                &on_table(ObjectKind::Trigger, "tr g", "or\"ders"),
                Some(&trigger_text),
            )
            .unwrap(),
        )
        .await;

        // Foreign keys are enforced from here on.
        exec(&mut *s, "pragma foreign_keys = on").await;
        let insert = insert_template(D, &table).unwrap().replace('?', "NULL");
        exec(&mut *s, &insert.replace("(NULL, NULL)", "(1, NULL)")).await;
        exec(
            &mut *s,
            &select_star(D, &table_obj, DEFAULT_ROW_LIMIT).unwrap(),
        )
        .await;
        exec(
            &mut *s,
            &select_star(D, &obj(ObjectKind::View, "v w"), DEFAULT_ROW_LIMIT).unwrap(),
        )
        .await;

        // The rebuilt table matches the original.
        let again = s
            .introspection()
            .unwrap()
            .table(&table_obj)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.columns, table.columns);
        assert_eq!(again.foreign_keys, table.foreign_keys);
        assert_eq!(again.primary_key, table.primary_key);
    });
}
