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

/// Runs `text` plain and with `params` and returns both rows-affected figures.
async fn affected(
    s: &mut dyn sqler_driver_api::Session,
    text: &str,
    params: Vec<Value>,
) -> (Option<u64>, sqler_driver_api::sqler_core::ResultSet) {
    let rs = sql_params(s, text, params).await;
    (rs.rows_affected(), rs)
}

#[test]
#[allow(clippy::too_many_lines)]
fn rows_affected_per_statement() {
    const SCHEMA: &str = "sqler_t_dml_count";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        let t = format!("{SCHEMA}.t");
        let reset = format!(
            "delete from {t}; insert into {t} (id, name) values (1, N'a'), (2, N'b'), (3, N'c')"
        );
        let create = sql(
            &mut *s,
            &format!("create table {t} (id int primary key, name nvarchar(20))"),
        )
        .await;
        assert_eq!(create.rows_affected(), None);

        // Plain form, then the same statements with `@P` parameters.
        for with_params in [false, true] {
            sql(&mut *s, &reset).await;
            let (p0, p1, p3) = if with_params {
                (
                    vec![Value::Int(99)],
                    vec![Value::Int(1)],
                    vec![Value::Int(0)],
                )
            } else {
                (vec![], vec![], vec![])
            };
            let id = |n: i64, ph: &str| {
                if with_params {
                    ph.to_owned()
                } else {
                    n.to_string()
                }
            };

            let upd = |cond: String| format!("update {t} set name = N'x' where {cond}");
            let (n, _) =
                affected(&mut *s, &upd(format!("id = {}", id(99, "@P1"))), p0.clone()).await;
            assert_eq!(n, Some(0), "update 0, params {with_params}");
            let (n, _) =
                affected(&mut *s, &upd(format!("id = {}", id(1, "@P1"))), p1.clone()).await;
            assert_eq!(n, Some(1), "update 1, params {with_params}");
            let (n, _) =
                affected(&mut *s, &upd(format!("id > {}", id(0, "@P1"))), p3.clone()).await;
            assert_eq!(n, Some(3), "update 3, params {with_params}");

            let ins =
                |a: &str, b: &str| format!("insert into {t} (id, name) values ({a}, N'n'){b}");
            let (n, _) = if with_params {
                affected(&mut *s, &ins("@P1", ""), vec![Value::Int(10)]).await
            } else {
                affected(&mut *s, &ins("10", ""), vec![]).await
            };
            assert_eq!(n, Some(1), "insert 1, params {with_params}");
            let (n, _) = if with_params {
                affected(
                    &mut *s,
                    &ins("@P1", ", (@P2, N'n')"),
                    vec![Value::Int(11), Value::Int(12)],
                )
                .await
            } else {
                affected(&mut *s, &ins("11", ", (12, N'n')"), vec![]).await
            };
            assert_eq!(n, Some(2), "insert 2, params {with_params}");

            let del = |cond: String| format!("delete from {t} where {cond}");
            let (n, _) =
                affected(&mut *s, &del(format!("id = {}", id(99, "@P1"))), p0.clone()).await;
            assert_eq!(n, Some(0), "delete 0, params {with_params}");
            let (n, _) = affected(
                &mut *s,
                &del(format!("id = {}", id(10, "@P1"))),
                if with_params {
                    vec![Value::Int(10)]
                } else {
                    vec![]
                },
            )
            .await;
            assert_eq!(n, Some(1), "delete 1, params {with_params}");
            let (n, _) =
                affected(&mut *s, &del(format!("id > {}", id(0, "@P1"))), p3.clone()).await;
            assert_eq!(n, Some(5), "delete n, params {with_params}");
        }

        // OUTPUT streams its rows and still reports the count.
        sql(&mut *s, &reset).await;
        let (n, rs) = affected(
            &mut *s,
            &format!("update {t} set name = N'y' output inserted.id where id <= @P1"),
            vec![Value::Int(2)],
        )
        .await;
        assert_eq!(n, Some(2));
        assert_eq!(rs.len(), 2);
        let (n, rs) = affected(
            &mut *s,
            &format!("update {t} set name = N'z' output inserted.id where id <= 2"),
            vec![],
        )
        .await;
        assert_eq!((n, rs.len()), (Some(2), 2));

        // A select stays unknown, with and without parameters.
        let (n, rs) = affected(
            &mut *s,
            &format!("select id from {t} where id >= @P1"),
            vec![Value::Int(1)],
        )
        .await;
        assert_eq!((n, rs.len()), (None, 3));
        let rs = sql(&mut *s, &format!("select id from {t}")).await;
        assert_eq!((rs.rows_affected(), rs.len()), (None, 3));
        s.close().await.unwrap();
    });
}

#[test]
fn rows_affected_unknown_cases() {
    const SCHEMA: &str = "sqler_t_dml_nocount";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        let t = format!("{SCHEMA}.t");
        let audit = format!("{SCHEMA}.audit");
        sql(
            &mut *s,
            &format!("create table {t} (id int primary key, v int)"),
        )
        .await;
        sql(&mut *s, &format!("create table {audit} (id int)")).await;
        sql(&mut *s, &format!("insert into {t} values (1, 0), (2, 0)")).await;
        let update = format!("update {t} set v = v + 1 where id = 1");

        // (a) Session NOCOUNT: unknown, never a guess; off again, known.
        sql(&mut *s, "set nocount on").await;
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), None);
        sql(&mut *s, "set nocount off").await;
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), Some(1));

        // (b) A trigger that counts its own statement adds a second count.
        sql(
            &mut *s,
            &format!(
                "create trigger {SCHEMA}.trg on {t} after update as \
                 begin insert into {audit} select id from inserted end"
            ),
        )
        .await;
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), None);
        sql(
            &mut *s,
            &format!(
                "alter trigger {SCHEMA}.trg on {t} after update as \
                 begin set nocount on; insert into {audit} select id from inserted end"
            ),
        )
        .await;
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), Some(1));
        s.close().await.unwrap();
    });
}

/// A DML text that also holds a session-state statement without a `;` keeps the plain-batch
/// route: the state survives, and the row count is unknown (`None`).
#[test]
fn dml_text_with_session_state_keeps_the_batch_route() {
    const SCHEMA: &str = "sqler_t_dml_state";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        let t = format!("{SCHEMA}.t");
        sql(
            &mut *s,
            &format!("create table {t} (id int primary key, v int)"),
        )
        .await;
        sql(&mut *s, &format!("insert into {t} values (1, 0)")).await;
        let update = format!("update {t} set v = v + 1 where id = 1");

        // `SET NOCOUNT ON` after the insert, no `;`: it lasts, so the next count is unknown.
        let rs = sql(
            &mut *s,
            &format!("insert into {t} values (2, 0)\nset nocount on"),
        )
        .await;
        assert_eq!(rs.rows_affected(), None);
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), None);
        sql(&mut *s, "set nocount off").await;
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), Some(1));

        // `USE` after an update: the database switch lasts.
        let home = sql(&mut *s, "select db_name()").await.rows()[0].0[0].clone();
        let rs = sql(&mut *s, &format!("{update}\nuse tempdb")).await;
        assert_eq!(rs.rows_affected(), None);
        assert_eq!(
            sql(&mut *s, "select db_name()").await.rows()[0].0[0],
            Value::Text("tempdb".into())
        );
        let Value::Text(home) = home else {
            panic!("db_name() is text")
        };
        sql(&mut *s, &format!("use [{home}]")).await;

        // `SELECT ... INTO #tmp` after an update: the temp table lasts.
        let rs = sql(&mut *s, &format!("{update}\nselect * into #keep from {t}")).await;
        assert_eq!(rs.rows_affected(), None);
        let rs = sql(&mut *s, "select count(*) from #keep").await;
        assert_eq!(count(&rs), Value::Int(2));

        // A plain single statement is still counted.
        assert_eq!(sql(&mut *s, &update).await.rows_affected(), Some(1));
        s.close().await.unwrap();
    });
}

#[test]
fn dates_bind_the_same_under_dmy() {
    const SCHEMA: &str = "sqler_t_dml_dmy";
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, SCHEMA).await;
        sql(
            &mut *s,
            &format!(
                "create table {SCHEMA}.t (d date, dt datetime, sdt smalldatetime, dt2 datetime2(3), \
                 dto datetimeoffset(3))"
            ),
        )
        .await;
        // The values come from the server, so they are the driver's own date and timestamp types.
        let src = sql(
            &mut *s,
            "select cast('2024-03-05' as date), cast('2024-03-05T10:20:00' as datetime), \
             cast('2024-03-05T10:20:00' as smalldatetime), cast('2024-03-05T10:20:00.123' as datetime2(3)), \
             cast('2024-03-05T10:20:00.123+02:00' as datetimeoffset(3))",
        )
        .await;
        let params = src.rows()[0].0.clone();
        sql(&mut *s, "set language british; set dateformat dmy").await;
        sql_params(
            &mut *s,
            &format!("insert into {SCHEMA}.t values (@P1, @P2, @P3, @P4, @P5)"),
            params,
        )
        .await;
        sql(&mut *s, "set language us_english").await;
        let rs = sql(
            &mut *s,
            &format!(
                "select count(*) from {SCHEMA}.t where d = '20240305' and dt = '20240305 10:20:00' \
                 and sdt = '20240305 10:20:00' and dt2 = '2024-03-05T10:20:00.123' \
                 and dto = '2024-03-05T10:20:00.123+02:00'"
            ),
        )
        .await;
        assert_eq!(count(&rs), Value::Int(1));
        s.close().await.unwrap();
    });
}
