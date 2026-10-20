// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks that the bound DML of `sqler_sql::dml` (staged grid edits, ADR-0030) finds
//! the row it was staged for on `MySQL` 8.4 and `MariaDB` 11.8 (ADR-0009). Without
//! `SQLER_TEST_MYSQL_URL` the tests skip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Flavour, run, start};
use sqler_driver_api::sqler_core::{
    DbObject, ObjectKind, ObjectPath, Query, ResultSet, Row, Table, Value,
};
use sqler_driver_api::{DriverError, Session, collect};
use sqler_sql::{Dialect, update};

async fn query(s: &mut dyn Session, text: &str) -> Result<ResultSet, DriverError> {
    let q = Query::new(text);
    let mut stream = s.execute(&q).await?;
    collect(&mut *stream).await
}

async fn exec(s: &mut dyn Session, text: &str) -> ResultSet {
    query(s, text).await.unwrap()
}

/// The keyless filter the grid builds: every column of `row`, by the table's column names.
fn filter<'a>(table: &'a Table, row: &'a Row) -> Vec<(&'a str, &'a Value)> {
    table
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .zip(&row.0)
        .collect()
}

/// A keyless edit of a row with a JSON column finds its row: `MySQL` compares a text parameter with
/// a `JSON` column as a JSON string, which never equals a document, so the generator casts the
/// parameter to `JSON`. `MariaDB`'s `JSON` is a text column and needs no cast.
#[test]
fn keyless_match_on_a_json_column_finds_the_row() {
    run(async {
        let Some(server) = start().await else { return };
        let mut s = common::connect(&server.config()).await;
        exec(&mut *s, "drop database if exists sqler_t_dml").await;
        exec(&mut *s, "create database sqler_t_dml").await;
        exec(
            &mut *s,
            "create table sqler_t_dml.k (name varchar(20), note varchar(20), j json, n int)",
        )
        .await;
        exec(
            &mut *s,
            "insert into sqler_t_dml.k values ('solo', 'x', '{\"a\": 1, \"b\": [1, 2]}', null), \
             ('solo', 'y', '{\"a\": 2}', 5)",
        )
        .await;
        let object = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("sqler_t_dml"),
            "k",
        );
        let table = s
            .introspection()
            .unwrap()
            .table(&object)
            .await
            .unwrap()
            .unwrap();
        let rs = exec(
            &mut *s,
            "select name, note, j, n from sqler_t_dml.k order by note",
        )
        .await;
        let first = rs.rows()[0].clone();
        match server.flavour {
            Flavour::MySql => assert!(matches!(first.0[2], Value::Json(_)), "{first:?}"),
            Flavour::MariaDb => assert!(matches!(first.0[2], Value::Bytes(_)), "{first:?}"),
        }
        let new = Value::Text("changed".into());
        let q = update(
            Dialect::MySql,
            &table,
            &[("note", &new)],
            &filter(&table, &first),
        )
        .unwrap();
        let done = {
            let mut stream = s.execute(&q).await.unwrap();
            collect(&mut *stream).await.unwrap()
        };
        assert_eq!(done.rows_affected(), Some(1), "{}", q.sql);
        let rs = exec(&mut *s, "select note from sqler_t_dml.k order by note").await;
        assert_eq!(
            rs.rows().iter().map(|r| r.0[0].clone()).collect::<Vec<_>>(),
            [Value::Text("changed".into()), Value::Text("y".into())]
        );
        exec(&mut *s, "drop database sqler_t_dml").await;
        s.close().await.unwrap();
    });
}
