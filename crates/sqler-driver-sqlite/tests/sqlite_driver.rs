// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! End-to-end checks of the `SQLite` driver through `dyn` objects only.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::path::PathBuf;

use sqler_driver_api::sqler_core::{
    Column, ConnectionConfig, DataType, DriverId, Query, ResultSet, Value,
};
use sqler_driver_api::{DriverError, DriverRegistry, QueryEvent, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn registry() -> DriverRegistry {
    let mut r = DriverRegistry::new();
    r.register(Box::new(SqliteDriver::new())).unwrap();
    r
}

fn config(target: &str) -> ConnectionConfig {
    ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), target)
}

async fn memory() -> Box<dyn Session> {
    registry().connect(&config(":memory:")).await.unwrap()
}

async fn query(session: &mut dyn Session, q: &Query) -> Result<ResultSet, DriverError> {
    let mut stream = session.execute(q).await?;
    collect(&mut *stream).await
}

async fn sql(session: &mut dyn Session, text: &str) -> ResultSet {
    query(session, &Query::new(text)).await.unwrap()
}

fn temp_db(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.sqlite"));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn driver_id() {
    use sqler_driver_api::Driver as _;
    assert_eq!(SqliteDriver::new().id(), "sqlite");
}

#[test]
fn every_storage_class() {
    run(async {
        let mut s = memory().await;
        let rs = sql(
            &mut *s,
            "select 1 as i, 1.5 as r, 'x' as t, x'00ff' as b, NULL as n",
        )
        .await;
        let names: Vec<_> = rs.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["i", "r", "t", "b", "n"]);
        assert_eq!(
            rs.rows()[0].0,
            vec![
                Value::Int(1),
                Value::Float(1.5),
                Value::Text("x".into()),
                Value::Bytes(vec![0, 255]),
                Value::Null,
            ]
        );
        assert_eq!(rs.rows_affected(), None);
    });
}

#[test]
fn declared_types_and_runtime_values() {
    run(async {
        let mut s = memory().await;
        sql(
            &mut *s,
            "create table t (a INTEGER, b REAL, c TEXT, d BLOB, e BOOLEAN, f DATE, g DATETIME, h JSON)",
        )
        .await;
        sql(
            &mut *s,
            "insert into t values (1, 2.5, 'x', x'01', 1, '2024-01-02', '2024-01-02 03:04:05', 'v')",
        )
        .await;
        let rs = sql(&mut *s, "select * from t").await;
        let types: Vec<DataType> = rs.columns().iter().map(|c: &Column| c.data_type).collect();
        assert_eq!(
            types,
            [
                DataType::Integer,
                DataType::Float,
                DataType::Text,
                DataType::Binary,
                DataType::Boolean,
                DataType::Date,
                DataType::Timestamp,
                DataType::Other,
            ]
        );
        assert!(rs.columns().iter().all(|c| c.native_type.is_some()));
        let row = &rs.rows()[0];
        assert_eq!(row.get(4), Some(&Value::Int(1)));
        assert_eq!(
            row.get(5),
            Some(&Value::Date("2024-01-02".parse().unwrap()))
        );
        assert_eq!(
            row.get(6),
            Some(&Value::Timestamp("2024-01-02 03:04:05".parse().unwrap()))
        );
    });
}

#[test]
fn temporal_text_decodes_with_fallbacks() {
    run(async {
        let mut s = memory().await;
        sql(&mut *s, "create table t (d DATE, ts DATETIME, tm TIME)").await;
        sql(
            &mut *s,
            "insert into t values ('2024-02-29', '2024-01-02T03:04:05.5+02:00', '23:59:58'), \
             ('garbage', 'also garbage', '25:00'), (19000, 'x', NULL)",
        )
        .await;
        sql(
            &mut *s,
            "insert into t values (NULL, '2024-01-02 03:04:05Z', NULL)",
        )
        .await;
        let rs = sql(&mut *s, "select * from t").await;
        let rows = rs.rows();
        assert_eq!(
            rows[0].get(0),
            Some(&Value::Date("2024-02-29".parse().unwrap()))
        );
        assert_eq!(
            rows[0].get(1),
            Some(&Value::TimestampTz(
                "2024-01-02 03:04:05.5+02:00".parse().unwrap()
            ))
        );
        assert_eq!(
            rows[0].get(2),
            Some(&Value::Time("23:59:58".parse().unwrap()))
        );
        assert_eq!(rows[1].get(0), Some(&Value::Text("garbage".into())));
        assert_eq!(rows[1].get(1), Some(&Value::Text("also garbage".into())));
        assert_eq!(rows[1].get(2), Some(&Value::Text("25:00".into())));
        assert_eq!(rows[2].get(0), Some(&Value::Int(19000)));
        assert_eq!(rows[2].get(2), Some(&Value::Null));
        assert!(matches!(rows[3].get(1), Some(Value::TimestampTz(_))));
    });
}

#[test]
fn malformed_non_ascii_datetime_falls_back_to_text() {
    run(async {
        let mut s = memory().await;
        sql(&mut *s, "create table t (ts DATETIME, d DATE, tm TIME)").await;
        sql(
            &mut *s,
            "insert into t values ('2024-01-02 03:04:05+aéb', '20é4-01-02', '1é:00:00')",
        )
        .await;
        let rs = sql(&mut *s, "select * from t").await;
        let row = &rs.rows()[0];
        assert_eq!(
            row.get(0),
            Some(&Value::Text("2024-01-02 03:04:05+aéb".into()))
        );
        assert_eq!(row.get(1), Some(&Value::Text("20é4-01-02".into())));
        assert_eq!(row.get(2), Some(&Value::Text("1é:00:00".into())));
    });
}

#[test]
fn typed_params_round_trip() {
    run(async {
        let mut s = memory().await;
        let date: sqler_driver_api::sqler_core::Date = "2024-01-02".parse().unwrap();
        let time: sqler_driver_api::sqler_core::Time = "03:04:05.25".parse().unwrap();
        let ts: sqler_driver_api::sqler_core::Timestamp = "2024-01-02 03:04:05".parse().unwrap();
        let tz: sqler_driver_api::sqler_core::TimestampTz =
            "2024-01-02 03:04:05-05:00".parse().unwrap();
        let dec: sqler_driver_api::sqler_core::Decimal = "12.50".parse().unwrap();
        sql(
            &mut *s,
            "create table t (d DATE, tm TIME, ts DATETIME, tz DATETIME, n NUMERIC, j JSON)",
        )
        .await;
        let q = Query::new("insert into t values (?, ?, ?, ?, ?, ?)").with_params(vec![
            Value::Date(date),
            Value::Time(time),
            Value::Timestamp(ts),
            Value::TimestampTz(tz),
            Value::Decimal(dec),
            Value::Json("{\"a\":1}".into()),
        ]);
        query(&mut *s, &q).await.unwrap();
        let rs = sql(&mut *s, "select * from t").await;
        let row = &rs.rows()[0];
        assert_eq!(row.get(0), Some(&Value::Date(date)));
        assert_eq!(row.get(1), Some(&Value::Time(time)));
        assert_eq!(row.get(2), Some(&Value::Timestamp(ts)));
        assert_eq!(row.get(3), Some(&Value::TimestampTz(tz)));
        // NUMERIC affinity stores the text as a real; JSON stays text.
        assert_eq!(row.get(4), Some(&Value::Float(12.5)));
        assert_eq!(row.get(5), Some(&Value::Text("{\"a\":1}".into())));
    });
}

#[test]
fn params_bind_in_order() {
    run(async {
        let mut s = memory().await;
        let q = Query::new("select ?, ?, ?, ?, ?, ?").with_params(vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(7),
            Value::Float(0.25),
            Value::Text("hi".into()),
            Value::Bytes(vec![1, 2]),
        ]);
        let rs = query(&mut *s, &q).await.unwrap();
        assert_eq!(
            rs.rows()[0].0,
            vec![
                Value::Null,
                Value::Int(1),
                Value::Int(7),
                Value::Float(0.25),
                Value::Text("hi".into()),
                Value::Bytes(vec![1, 2]),
            ]
        );
    });
}

#[test]
fn ddl_and_insert_report_rows_affected() {
    run(async {
        let mut s = memory().await;
        let rs = sql(&mut *s, "create table t (a integer)").await;
        assert_eq!(rs.columns().len(), 0);
        assert_eq!(rs.rows_affected(), Some(0));
        let rs = sql(&mut *s, "insert into t values (1), (2), (3)").await;
        assert_eq!(rs.rows_affected(), Some(3));
        let rs = sql(&mut *s, "select count(*) from t").await;
        assert_eq!(rs.rows()[0].get(0), Some(&Value::Int(3)));
    });
}

#[test]
fn file_database_persists_across_sessions() {
    let path = temp_db("persist");
    let target = path.to_str().unwrap().to_owned();
    run(async {
        let r = registry();
        let mut s = r.connect(&config(&target)).await.unwrap();
        sql(&mut *s, "create table t (a integer)").await;
        sql(&mut *s, "insert into t values (42)").await;
        s.close().await.unwrap();

        let mut s = r.connect(&config(&target)).await.unwrap();
        let rs = sql(&mut *s, "select a from t").await;
        assert_eq!(rs.rows()[0].get(0), Some(&Value::Int(42)));
        s.close().await.unwrap();
    });
    assert!(path.exists());
}

#[test]
fn memory_sessions_are_private() {
    run(async {
        let mut a = memory().await;
        let mut b = memory().await;
        sql(&mut *a, "create table t (a integer)").await;
        let err = query(&mut *b, &Query::new("select * from t"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Query(_)));
    });
}

#[test]
fn bad_sql_is_query_error() {
    run(async {
        let mut s = memory().await;
        let err = query(&mut *s, &Query::new("selec 1")).await.unwrap_err();
        assert!(matches!(err, DriverError::Query(ref m) if !m.is_empty()));
        // The session survives the error.
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(rs.len(), 1);
    });
}

#[test]
fn unopenable_path_is_connection_error() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("no-such-dir")
        .join("db.sqlite");
    run(async {
        let err = registry()
            .connect(&config(path.to_str().unwrap()))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, DriverError::Connection(_)));
    });
}

#[test]
fn none_after_done() {
    run(async {
        let mut s = memory().await;
        let q = Query::new("select 1");
        let mut stream = s.execute(&q).await.unwrap();
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Columns(_)))
        ));
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Rows(_)))
        ));
        assert!(matches!(
            stream.next_event().await,
            Some(Ok(QueryEvent::Done { .. }))
        ));
        assert!(stream.next_event().await.is_none());
        assert!(stream.next_event().await.is_none());
    });
}

#[test]
fn empty_result_has_columns_and_done() {
    run(async {
        let mut s = memory().await;
        let rs = sql(&mut *s, "select 1 as a where 0").await;
        assert_eq!(rs.columns().len(), 1);
        assert!(rs.is_empty());
    });
}

const BIG: &str = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 100000) \
                   select x from c";

#[test]
fn large_result_is_batched() {
    run(async {
        let mut s = memory().await;
        let q = Query::new(BIG);
        let mut stream = s.execute(&q).await.unwrap();
        let mut batches = 0;
        let mut rows = 0;
        while let Some(event) = stream.next_event().await {
            if let QueryEvent::Rows(r) = event.unwrap() {
                assert!(r.len() <= 256);
                batches += 1;
                rows += r.len();
            }
        }
        assert_eq!(rows, 100_000);
        assert!(batches > 1);
    });
}

#[test]
fn cancel_mid_stream_then_session_is_reusable() {
    run(async {
        let mut s = memory().await;
        let handle = s.cancel_handle();
        let q = Query::new(BIG);
        {
            let mut stream = s.execute(&q).await.unwrap();
            assert!(matches!(
                stream.next_event().await,
                Some(Ok(QueryEvent::Columns(_)))
            ));
            assert!(matches!(
                stream.next_event().await,
                Some(Ok(QueryEvent::Rows(_)))
            ));
            handle.cancel().await.unwrap();
            assert!(matches!(
                stream.next_event().await,
                Some(Err(DriverError::Cancelled))
            ));
            assert!(stream.next_event().await.is_none());
        }
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(rs.rows()[0].get(0), Some(&Value::Int(1)));
    });
}

#[test]
fn dropping_a_stream_mid_result_keeps_session_usable() {
    run(async {
        let mut s = memory().await;
        let q = Query::new(BIG);
        {
            let mut stream = s.execute(&q).await.unwrap();
            let _ = stream.next_event().await;
            let _ = stream.next_event().await;
        }
        let rs = sql(&mut *s, "select 2").await;
        assert_eq!(rs.rows()[0].get(0), Some(&Value::Int(2)));
    });
}
