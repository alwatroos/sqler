// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks of the `PostgreSQL` driver (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use std::time::Duration;

use sqler_driver_api::sqler_core::{DataType, Query, Value};
use sqler_driver_api::{DriverError, QueryEvent, Session};

use common::{connect, fresh_schema, query, run, sql, sql_params, url};

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

#[test]
fn select_one() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let rs = sql(&mut *s, "select 1 as a").await;
        assert_eq!(rs.columns()[0].name, "a");
        assert_eq!(rs.columns()[0].data_type, DataType::Integer);
        assert_eq!(rs.columns()[0].native_type.as_deref(), Some("INT4"));
        assert_eq!(rs.rows()[0].0, vec![Value::Int(1)]);
        s.close().await.unwrap();
    });
}

#[test]
fn streams_in_batches() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let q = Query::new("select g from generate_series(1, 100000) g");
        let mut stream = s.execute(&q).await.unwrap();
        let (mut batches, mut next) = (0, 1_i64);
        let mut done = false;
        while let Some(event) = stream.next_event().await {
            match event.unwrap() {
                QueryEvent::Columns(c) => assert_eq!(c.len(), 1),
                QueryEvent::Rows(rows) => {
                    assert!(rows.len() <= 256);
                    for row in rows {
                        assert_eq!(row.0[0], Value::Int(next));
                        next += 1;
                    }
                    batches += 1;
                }
                QueryEvent::Done { .. } => done = true,
                _ => {}
            }
        }
        assert!(done);
        assert!(batches > 1, "expected several batches, got {batches}");
        assert_eq!(next - 1, 100_000);
        assert!(batches >= 391);
    });
}

fn ts(s: &str) -> Value {
    Value::Timestamp(s.parse().unwrap())
}

#[test]
fn every_type() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_every_type").await;
        sql(&mut *s, "create type mood as enum ('sad', 'happy')").await;
        let rs = sql(
            &mut *s,
            "select true as b, 1::int2 as i2, 2::int4 as i4, 3::int8 as i8, 4::oid as o, \
             1.5::float4 as f4, 2.25::float8 as f8, 12345.678::numeric as n, \
             'NaN'::numeric as nan, 'x'::text as t, 'ab'::char(4) as c, 'v'::varchar as v, \
             'n'::name as nm, '\\x00ff'::bytea as by, '{\"a\":1}'::json as j, \
             '{\"a\": 1}'::jsonb as jb, '2024-01-02'::date as d, '-infinity'::date as dinf, \
             '01:02:03.5'::time as tm, '2024-01-02 03:04:05.25'::timestamp as ts, \
             '2024-01-02 03:04:05+02'::timestamptz as tz, 'infinity'::timestamptz as tzinf, \
             '3B0F1A3E-0000-4000-8000-000000000001'::uuid as u, '01:02:03+02'::timetz as ttz, \
             '1 year 2 mons 3 days 04:05:06'::interval as iv, '10.0.0.1/24'::inet as ip, \
             '10.0.0.0/8'::cidr as cd, array[1, null, 3] as a1, \
             array[['a b', 'c'], ['d', 'NULL']] as a2, '{}'::int[] as a0, \
             '[0:1]={1,2}'::int[] as alb, 'happy'::mood as e, null::int as nul",
        )
        .await;
        let row = &rs.rows()[0].0;
        let cols = rs.columns();
        let expected: Vec<(&str, DataType, Value)> = vec![
            ("b", DataType::Boolean, Value::Bool(true)),
            ("i2", DataType::Integer, Value::Int(1)),
            ("i4", DataType::Integer, Value::Int(2)),
            ("i8", DataType::Integer, Value::Int(3)),
            ("o", DataType::Integer, Value::Int(4)),
            ("f4", DataType::Float, Value::Float(1.5)),
            ("f8", DataType::Float, Value::Float(2.25)),
            (
                "n",
                DataType::Decimal,
                Value::Decimal("12345.678".parse().unwrap()),
            ),
            ("nan", DataType::Decimal, text("NaN")),
            ("t", DataType::Text, text("x")),
            ("c", DataType::Text, text("ab  ")),
            ("v", DataType::Text, text("v")),
            ("nm", DataType::Text, text("n")),
            ("by", DataType::Binary, Value::Bytes(vec![0, 255])),
            ("j", DataType::Json, Value::Json("{\"a\":1}".into())),
            ("jb", DataType::Json, Value::Json("{\"a\": 1}".into())),
            (
                "d",
                DataType::Date,
                Value::Date("2024-01-02".parse().unwrap()),
            ),
            ("dinf", DataType::Date, text("-infinity")),
            (
                "tm",
                DataType::Time,
                Value::Time("01:02:03.5".parse().unwrap()),
            ),
            ("ts", DataType::Timestamp, ts("2024-01-02 03:04:05.25")),
            (
                "tz",
                DataType::TimestampTz,
                Value::TimestampTz("2024-01-02 01:04:05+00:00".parse().unwrap()),
            ),
            ("tzinf", DataType::TimestampTz, text("infinity")),
            (
                "u",
                DataType::Text,
                text("3b0f1a3e-0000-4000-8000-000000000001"),
            ),
            ("ttz", DataType::Text, text("01:02:03+02")),
            ("iv", DataType::Text, text("1 year 2 mons 3 days 04:05:06")),
            ("ip", DataType::Text, text("10.0.0.1/24")),
            ("cd", DataType::Text, text("10.0.0.0/8")),
            ("a1", DataType::Text, text("{1,NULL,3}")),
            ("a2", DataType::Text, text("{{\"a b\",c},{d,\"NULL\"}}")),
            ("a0", DataType::Text, text("{}")),
            ("alb", DataType::Text, text("[0:1]={1,2}")),
            ("e", DataType::Other, text("happy")),
            ("nul", DataType::Integer, Value::Null),
        ];
        assert_eq!(cols.len(), expected.len());
        for (i, (name, ty, value)) in expected.into_iter().enumerate() {
            assert_eq!(cols[i].name, name);
            assert_eq!(cols[i].data_type, ty, "{name}");
            assert_eq!(row[i], value, "{name}");
        }
        assert_eq!(cols[3].native_type.as_deref(), Some("INT8"));
    });
}

#[test]
fn rows_affected() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_rows_affected").await;
        let rs = sql(&mut *s, "create table t (a int primary key, b text)").await;
        assert_eq!(rs.columns().len(), 0);
        assert_eq!(rs.rows_affected(), Some(0));
        let rs = sql(
            &mut *s,
            "insert into t select g, 'x' from generate_series(1, 3) g",
        )
        .await;
        assert_eq!(rs.rows_affected(), Some(3));
        let rs = sql(&mut *s, "update t set b = 'y' where a > 1").await;
        assert_eq!(rs.rows_affected(), Some(2));
        let rs = sql(&mut *s, "delete from t").await;
        assert_eq!(rs.rows_affected(), Some(3));
        // A statement with columns reports none.
        let rs = sql(&mut *s, "select * from t").await;
        assert_eq!(rs.rows_affected(), None);
        assert!(rs.is_empty());
        assert_eq!(rs.columns().len(), 2);
    });
}

#[test]
fn params() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let rs = sql_params(
            &mut *s,
            "select $1::int8 + 1, $2::text, $3::numeric, $4::date, $5::bool, $6::bytea, \
             $7::float8, $8::jsonb, $9::timestamp, $10::text is null, $11::time",
            vec![
                Value::Int(41),
                text("hi"),
                Value::Decimal("1.50".parse().unwrap()),
                Value::Date("2024-01-02".parse().unwrap()),
                Value::Bool(true),
                Value::Bytes(vec![1, 2]),
                Value::Float(0.5),
                Value::Json("{\"a\":[1]}".into()),
                Value::Timestamp("2024-01-02 03:04:05".parse().unwrap()),
                Value::Null,
                Value::Time("01:02:03".parse().unwrap()),
            ],
        )
        .await;
        assert_eq!(
            rs.rows()[0].0,
            vec![
                Value::Int(42),
                text("hi"),
                Value::Decimal("1.50".parse().unwrap()),
                Value::Date("2024-01-02".parse().unwrap()),
                Value::Bool(true),
                Value::Bytes(vec![1, 2]),
                Value::Float(0.5),
                Value::Json("{\"a\": [1]}".into()),
                ts("2024-01-02 03:04:05"),
                Value::Bool(true),
                Value::Time("01:02:03".parse().unwrap()),
            ]
        );
    });
}

async fn within<T>(secs: u64, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), fut)
        .await
        .unwrap_or_else(|_| panic!("timed out after {secs} s"))
}

async fn assert_cancelled(s: &mut dyn Session, sql_text: &str) {
    let pid = match &sql(s, "select pg_backend_pid()").await.rows()[0].0[0] {
        Value::Int(pid) => *pid,
        other => panic!("pid: {other:?}"),
    };
    let handle = s.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        handle.cancel().await.unwrap();
    });
    let q = Query::new(sql_text);
    let started = std::time::Instant::now();
    {
        let mut stream = s.execute(&q).await.unwrap();
        let mut result = None;
        while let Some(event) = within(10, stream.next_event()).await {
            if let Err(e) = event {
                result = Some(e);
                break;
            }
        }
        assert!(matches!(result, Some(DriverError::Cancelled)), "{result:?}");
        assert!(within(10, stream.next_event()).await.is_none());
    }
    assert!(started.elapsed() < Duration::from_secs(10));

    // The backend is idle on the server, seen from a second session.
    let mut other = connect(&common::url().unwrap()).await;
    let rs = sql_params(
        &mut *other,
        "select count(*) from pg_stat_activity where pid = $1::int4 and state = 'active'",
        vec![Value::Int(pid)],
    )
    .await;
    assert_eq!(rs.rows()[0].0[0], Value::Int(0));

    // The same session keeps working.
    let rs = sql(s, "select 1").await;
    assert_eq!(rs.rows()[0].0[0], Value::Int(1));
}

#[test]
fn cancel_a_query() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        assert_cancelled(&mut *s, "select pg_sleep(30)").await;
    });
}

#[test]
fn cancel_a_statement_without_columns() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        assert_cancelled(&mut *s, "do $$ begin perform pg_sleep(30); end $$").await;
    });
}

#[test]
fn statement_timeout_is_a_query_error() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        sql(&mut *s, "set statement_timeout = 200").await;
        let err = query(&mut *s, &Query::new("select pg_sleep(5)"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Query(_)), "{err:?}");
    });
}

#[test]
fn bad_queries_are_query_errors() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let err = query(&mut *s, &Query::new("select * from sqler_no_such_table"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Query(ref m) if m.contains("sqler_no_such_table")));
        // An error while streaming, not while preparing.
        let err = query(
            &mut *s,
            &Query::new("select 1 / (g - 5) from generate_series(1, 9) g"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DriverError::Query(ref m) if m.contains("division by zero")));
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(rs.len(), 1);
    });
}

#[test]
fn connect_failure_does_not_leak_the_password() {
    run(async {
        let err = common::try_connect(&common::config("postgres://u:s3cret@127.0.0.1:1/x"))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, DriverError::Connection(_)));
        assert!(!err.to_string().contains("s3cret"));
    });
}
