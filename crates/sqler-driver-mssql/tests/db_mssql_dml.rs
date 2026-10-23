// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated DML and transaction checks (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{Query, Value};

use common::{connect, fresh_schema, query, run, sql, sql_params, url};

fn count(rows: &sqler_driver_api::sqler_core::ResultSet) -> Value {
    rows.rows()[0].0[0].clone()
}

#[test]
fn create_insert_update_delete() {
    const SCHEMA: &str = "sqler_t_dml_crud";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        let rs = sql(
            &mut *s,
            &format!("create table {SCHEMA}.t (id int primary key, name nvarchar(20) not null)"),
        )
        .await;
        assert_eq!(rs.columns(), []);
        assert_eq!(rs.rows_affected(), None);

        sql_params(
            &mut *s,
            &format!("insert into {SCHEMA}.t (id, name) values (@P1, @P2), (@P3, @P4)"),
            vec![
                Value::Int(1),
                Value::Text("one".into()),
                Value::Int(2),
                Value::Text("two".into()),
            ],
        )
        .await;
        let rs = sql(&mut *s, &format!("select name from {SCHEMA}.t order by id")).await;
        assert_eq!(rs.len(), 2);

        sql_params(
            &mut *s,
            &format!("update {SCHEMA}.t set name = @P1 where id = @P2"),
            vec![Value::Text("uno".into()), Value::Int(1)],
        )
        .await;
        let rs = sql(
            &mut *s,
            &format!("select name from {SCHEMA}.t where id = 1"),
        )
        .await;
        assert_eq!(rs.rows()[0].0, [Value::Text("uno".into())]);

        sql(&mut *s, &format!("delete from {SCHEMA}.t where id = 2")).await;
        let rs = sql(&mut *s, &format!("select count(*) from {SCHEMA}.t")).await;
        assert_eq!(count(&rs), Value::Int(1));

        let err = query(
            &mut *s,
            &Query::new(format!(
                "insert into {SCHEMA}.t (id, name) values (1, N'dup')"
            )),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, DriverError::Query(ref m) if m.ends_with("(error 2627)")),
            "{err:?}"
        );
        s.close().await.unwrap();
    });
}

#[test]
fn transaction_rollback_and_commit() {
    const SCHEMA: &str = "sqler_t_dml_tx";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        sql(&mut *s, &format!("create table {SCHEMA}.t (id int)")).await;

        sql(&mut *s, "begin transaction").await;
        sql(&mut *s, &format!("insert into {SCHEMA}.t values (1)")).await;
        let rs = sql(&mut *s, &format!("select count(*) from {SCHEMA}.t")).await;
        assert_eq!(count(&rs), Value::Int(1));
        sql(&mut *s, "rollback").await;
        let rs = sql(&mut *s, &format!("select count(*) from {SCHEMA}.t")).await;
        assert_eq!(count(&rs), Value::Int(0));

        sql(&mut *s, "begin transaction").await;
        sql_params(
            &mut *s,
            &format!("insert into {SCHEMA}.t values (@P1)"),
            vec![Value::Int(2)],
        )
        .await;
        sql(&mut *s, "commit").await;
        let rs = sql(&mut *s, &format!("select count(*) from {SCHEMA}.t")).await;
        assert_eq!(count(&rs), Value::Int(1));
    });
}

#[test]
fn cancel_rolls_back_an_open_transaction() {
    const SCHEMA: &str = "sqler_t_dml_cancel";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        sql(&mut *s, &format!("create table {SCHEMA}.t (id int)")).await;
        sql(&mut *s, "begin transaction").await;
        sql(&mut *s, &format!("insert into {SCHEMA}.t values (1)")).await;

        let handle = s.cancel_handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            handle.cancel().await.unwrap();
        });
        let err = query(&mut *s, &Query::new("waitfor delay '00:01:00'"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Cancelled), "{err:?}");

        // The server drops the connection and rolls the transaction back.
        let mut fresh = connect(&url).await;
        let mut rolled_back = false;
        for _ in 0..20 {
            let rs = sql(
                &mut *fresh,
                &format!("select count(*) from {SCHEMA}.t with (readpast)"),
            )
            .await;
            if count(&rs) == Value::Int(0) {
                rolled_back = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(rolled_back, "the open transaction was not rolled back");
    });
}
