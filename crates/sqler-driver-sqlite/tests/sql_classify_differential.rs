// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Differential check of the `sqler-sql` classifier against a real engine: every statement the
//! classifier calls `Read` must leave an in-memory `SQLite` database (schema, data and the
//! change counter) exactly as it was. Statements that are not `Read` are not run.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::{Dialect, StatementClass, classify, split};

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

/// Runs one statement; `false` when the engine rejects it.
async fn try_exec(s: &mut dyn Session, text: &str) -> bool {
    let q = Query::new(text);
    match s.execute(&q).await {
        Ok(mut stream) => collect(&mut *stream).await.is_ok(),
        Err(_) => false,
    }
}

/// Schema, data and the change counter, as one text.
async fn fingerprint(s: &mut dyn Session) -> String {
    let q = Query::new(
        "select (select ifnull(group_concat(type || name || ifnull(sql, ''), '|'), '') \
                   from sqlite_master) \
              || '#' || (select ifnull(group_concat(a || ':' || ifnull(b, ''), ','), '') from t) \
              || '#' || (select ifnull(group_concat(x, ','), '') from u) \
              || '#' || total_changes()",
    );
    let mut stream = s.execute(&q).await.unwrap();
    let rs = collect(&mut *stream).await.unwrap();
    match rs.rows()[0].get(0) {
        Some(Value::Text(t)) => t.clone(),
        other => panic!("unexpected fingerprint {other:?}"),
    }
}

/// Statements of every flavour, hostile ones included. Anything that is `Read` here is run.
const CORPUS: &[&str] = &[
    "select 1",
    "select * from t",
    "select a, count(*) from t group by a order by a limit 1",
    "with c as (select a from t) select * from c",
    "with c as (select 1) insert into t values (9, 'x')",
    "with c as (select 1) delete from t",
    "values (1), (2)",
    "table t",
    "select * from (select * from t) join u on 1",
    "select x from u where x in (select a from t)",
    "select 1 from t where exists (select 1 from u)",
    "select 1 union all select 2",
    "select * from v",
    "select [a] from t",
    "select \"a\" from \"t\"",
    "select 'a;b'; drop table t",
    "select 1; delete from t",
    "select 1 -- ; drop table t",
    "select 1 /* ; */ ; update t set a = 2",
    "select [a]] ; drop table t; --]",
    "select 1 --\u{3000}x from (select 1 as \u{3000}x) t; drop table t",
    "select\u{a0}1; drop table t",
    "select 1 --\u{a0}x\n; drop table t",
    "select '\\'; drop table t; -- '",
    "select 1 where 'a' = '\\'; delete from t; --'",
    "select load_extension('x')",
    "select randomblob(4)",
    "select abs(-1), upper('a'), length('abc'), coalesce(null, 1)",
    "select total_changes()",
    "select last_insert_rowid()",
    "select sqlite_version()",
    "select replace('abc', 'b', 'c')",
    "replace into t values (1, 'z')",
    "insert into t values (7, 'n')",
    "insert or replace into t values (1, 'y')",
    "update t set b = 'q'",
    "delete from t",
    "drop table u",
    "drop view v",
    "create table w (a)",
    "create index ix on t (a)",
    "alter table t add column c",
    "alter table t rename to t2",
    "pragma table_info(t)",
    "pragma table_list",
    "pragma index_list(t)",
    "pragma user_version = 5",
    "pragma journal_mode = wal",
    "pragma foreign_keys = on",
    "pragma writable_schema = on",
    "pragma schema.table_info(t)",
    "explain select * from t",
    "explain query plan select * from t",
    "explain delete from t",
    "vacuum",
    "analyze",
    "reindex",
    "attach ':memory:' as other",
    "begin",
    "savepoint s",
    "rollback",
    "select * from pragma_table_info('t')",
    "select * from json_each('[1,2]')",
    "select 1 where 1 = 1 and 2 = 2 or 3 = 3",
    "select a from t where b like 'x%' escape '!'",
    "select cast(a as text), typeof(a) from t",
    "select case when a > 1 then 'x' else 'y' end from t",
    "select * from t limit 1 offset 1",
    "select 1 where 1 = ?1",
    "select 1 /* unterminated",
    "select 'unterminated",
    "select 1 do",
    "select next value for s",
    "select filter(1)",
    "select partition(1)",
    "select * from partition(1)",
    "select * from t join join(1)",
    "select * from like(1)",
    "select * from t, lateral partition(1)",
    "select main.cast(1)",
    "select main.row(1)",
    "select main.load_extension('x')",
    "select * from pragma_optimize",
    "SELECT * FROM PRAGMA_OPTIMIZE",
    "select * from main.pragma_optimize",
    "with c as (select * from pragma_optimize) select * from c",
    "select * from main.\"pragma_optimize\"",
    "select * from sqlite_dbpage",
    "select * from 'pragma_optimize'",
    "select * from main.'pragma_optimize'",
    "select * from 'sqlite_dbpage'",
    "select * from [pragma_optimize]",
    "select * from main.[pragma_optimize]",
    "select * from `pragma_optimize`",
    "select cast(1)",
    "select cast(1 as text), extract(1)",
    "explain vacuum",
    "explain query plan vacuum",
    "explain query plan attach ':memory:' as o",
    "explain analyze reindex",
    "explain create table w (a)",
    "set x = 1",
    "commit /*! ; drop table t */",
    "end",
    "release s",
];

#[test]
fn read_statements_leave_the_database_unchanged() {
    run(async {
        let mut ran_read = 0usize;
        let mut ran_non_read = 0usize;
        for text in CORPUS {
            // A fresh database per statement, so that one entry cannot mask another.
            let mut s = memory().await;
            for setup in [
                "create table t (a integer primary key, b text)",
                "create table u (x integer)",
                "create view v as select a from t",
                "insert into t values (1, 'one'), (2, 'two')",
                "insert into u values (10), (20)",
            ] {
                assert!(try_exec(&mut *s, setup).await, "{setup}");
            }
            let before = fingerprint(&mut *s).await;
            let class = classify(D, text);
            // A script may hold several statements; the whole text must be `Read`.
            let all_read = class == StatementClass::Read
                && split(D, text)
                    .iter()
                    .all(|seg| classify(D, &text[seg.span.clone()]) != StatementClass::Write);
            if all_read {
                ran_read += 1;
                // The engine may reject it (a missing function, a bad argument): no change then.
                let _ = try_exec(&mut *s, text).await;
                let after = fingerprint(&mut *s).await;
                assert_eq!(
                    before, after,
                    "classified Read but changed the database: {text:?}"
                );
            } else {
                ran_non_read += 1;
            }
        }
        assert!(ran_read >= 25, "only {ran_read} read statements ran");
        assert!(
            ran_non_read >= 25,
            "only {ran_non_read} non-read statements"
        );
    });
}

/// The writers in the corpus must never be classified `Read`.
#[test]
fn known_writers_are_not_read() {
    for text in [
        "with c as (select 1) insert into t values (9, 'x')",
        "select 'a;b'; drop table t",
        "select 1; delete from t",
        "select 1 /* ; */ ; update t set a = 2",
        "select [a]] ; drop table t; --]",
        "select\u{a0}1; drop table t",
        "select '\\'; drop table t; -- '",
        "select load_extension('x')",
        "replace into t values (1, 'z')",
        "pragma user_version = 5",
        "explain delete from t",
        "select filter(1)",
        "select * from partition(1)",
        "select * from t join join(1)",
        "select * from like(1)",
        "select main.cast(1)",
        "select main.load_extension('x')",
        "select * from pragma_optimize",
        "SELECT * FROM PRAGMA_OPTIMIZE",
        "select * from main.pragma_optimize",
        "with c as (select * from pragma_optimize) select * from c",
        "select * from main.\"pragma_optimize\"",
        "select * from sqlite_dbpage",
        "select * from 'pragma_optimize'",
        "select * from main.'pragma_optimize'",
        "select * from 'sqlite_dbpage'",
        "select * from [pragma_optimize]",
        "select * from main.[pragma_optimize]",
        "select * from `pragma_optimize`",
        "select * from 'pragma_optimize'",
        "select * from main.'pragma_optimize'",
        "select * from 'sqlite_dbpage'",
        "select * from [pragma_optimize]",
        "select * from main.[pragma_optimize]",
        "select * from `pragma_optimize`",
        "explain vacuum",
        "explain query plan attach ':memory:' as o",
    ] {
        assert_ne!(classify(D, text), StatementClass::Read, "{text:?}");
    }
}
