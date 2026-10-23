// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks of the `SQL Server` driver (see AGENTS.md §3).

#![allow(clippy::unwrap_used)]

mod common;

use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{DataType, Query, Value};
use sqler_driver_api::{DriverError, QueryEvent};

use DataType as T;

use common::{connect, query, run, sql, sql_params, url};

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
        assert_eq!(rs.columns()[0].native_type.as_deref(), Some("int"));
        assert_eq!(rs.rows()[0].0, vec![Value::Int(1)]);
        assert_eq!(rs.rows_affected(), None);
        s.close().await.unwrap();
    });
}

#[test]
fn streams_in_batches() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let q = Query::new(
            "select top (100000) row_number() over (order by (select null)) as n \
             from sys.all_objects a cross join sys.all_objects b",
        );
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
                QueryEvent::Done { rows_affected } => {
                    assert_eq!(rows_affected, None);
                    done = true;
                }
                _ => {}
            }
        }
        assert!(done);
        assert_eq!(next - 1, 100_000);
        assert!(batches >= 391, "got {batches} batches");
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
        let rs = sql(
            &mut *s,
            "select cast(1 as bit) as b, cast(255 as tinyint) as ti, cast(-2 as smallint) as si, \
             cast(3 as int) as i, cast(4 as bigint) as bi, cast(1.5 as real) as r, \
             cast(2.25 as float) as f, cast(12345.678 as numeric(10, 3)) as n, \
             cast(N'h\u{e9}llo' as nvarchar(20)) as nv, cast('abc' as varchar(10)) as v, \
             cast('0E984725-C51C-4BF4-9960-E1C80E27ABA0' as uniqueidentifier) as g, \
             cast(0x0102 as varbinary(10)) as vb, cast('<a/>' as xml) as x, \
             cast('2024-02-29' as date) as d, cast('12:34:56.1234567' as time(7)) as t, \
             cast('2024-02-29 01:02:03' as datetime) as dt, \
             cast('2024-02-29 01:02:00' as smalldatetime) as sdt, \
             cast('2024-02-29 01:02:03.1234567' as datetime2(7)) as dt2, \
             cast('2024-01-02 03:04:05.5 +02:00' as datetimeoffset(1)) as dto, \
             cast(12.3456 as money) as m, cast(null as int) as nul",
        )
        .await;
        let row = &rs.rows()[0].0;
        let types: Vec<DataType> = rs.columns().iter().map(|c| c.data_type).collect();
        assert_eq!(
            types,
            [
                T::Boolean,
                T::Integer,
                T::Integer,
                T::Integer,
                T::Integer,
                T::Float,
                T::Float,
                T::Decimal,
                T::Text,
                T::Text,
                T::Text,
                T::Binary,
                T::Text,
                T::Date,
                T::Time,
                T::Timestamp,
                T::Timestamp,
                T::Timestamp,
                T::TimestampTz,
                T::Float,
                T::Integer,
            ]
        );
        assert_eq!(row[0], Value::Bool(true));
        assert_eq!(row[1], Value::Int(255));
        assert_eq!(row[2], Value::Int(-2));
        assert_eq!(row[3], Value::Int(3));
        assert_eq!(row[4], Value::Int(4));
        assert_eq!(row[5], Value::Float(1.5));
        assert_eq!(row[6], Value::Float(2.25));
        assert_eq!(row[7], Value::Decimal("12345.678".parse().unwrap()));
        assert_eq!(row[8], text("h\u{e9}llo"));
        assert_eq!(row[9], text("abc"));
        assert_eq!(row[10], text("0E984725-C51C-4BF4-9960-E1C80E27ABA0"));
        assert_eq!(row[11], Value::Bytes(vec![1, 2]));
        assert_eq!(row[12], text("<a/>"));
        assert_eq!(row[13], Value::Date("2024-02-29".parse().unwrap()));
        assert_eq!(row[14], Value::Time("12:34:56.1234567".parse().unwrap()));
        assert_eq!(row[15], ts("2024-02-29 01:02:03"));
        assert_eq!(row[16], ts("2024-02-29 01:02:00"));
        assert_eq!(row[17], ts("2024-02-29 01:02:03.1234567"));
        assert_eq!(
            row[18],
            Value::TimestampTz("2024-01-02 03:04:05.5+02:00".parse().unwrap())
        );
        assert_eq!(row[19], Value::Float(12.3456));
        assert_eq!(row[20], Value::Null);
    });
}

#[test]
fn params_bind_in_order() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let rs = sql_params(
            &mut *s,
            "select @P1 as a, @P2 as b, @P3 as c, @P4 as d, @P5 as e, @P6 as f",
            vec![
                Value::Int(7),
                text("x\u{e9}"),
                Value::Null,
                Value::Float(2.5),
                Value::Bytes(vec![9, 8]),
                Value::Bool(true),
            ],
        )
        .await;
        assert_eq!(
            rs.rows()[0].0,
            vec![
                Value::Int(7),
                text("x\u{e9}"),
                Value::Null,
                Value::Float(2.5),
                Value::Bytes(vec![9, 8]),
                Value::Bool(true),
            ]
        );
        // Values without a tiberius type bind as text and convert on the server.
        let rs = sql_params(
            &mut *s,
            "select cast(@P1 as date) as d, cast(@P2 as datetime2(7)) as t",
            vec![
                Value::Date("2024-02-29".parse().unwrap()),
                ts("2024-02-29 01:02:03.123456789"),
            ],
        )
        .await;
        assert_eq!(
            rs.rows()[0].0[0],
            Value::Date("2024-02-29".parse().unwrap())
        );
        assert_eq!(rs.rows()[0].0[1], ts("2024-02-29 01:02:03.1234567"));
    });
}

#[test]
fn error_text_carries_the_number_and_the_session_survives() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let err = query(&mut *s, &Query::new("select 1/0")).await.unwrap_err();
        let DriverError::Query(msg) = err else {
            panic!("not a query error: {err:?}");
        };
        assert!(msg.contains("Divide by zero"), "{msg}");
        assert!(msg.ends_with("(error 8134)"), "{msg}");
        assert_eq!(
            sql(&mut *s, "select 2 as a").await.rows()[0].0,
            [Value::Int(2)]
        );
    });
}

#[test]
fn statement_without_a_result_set_and_extra_result_sets() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let rs = sql(&mut *s, "declare @x int = 1").await;
        assert_eq!(rs.columns(), []);
        assert!(rs.is_empty());
        assert_eq!(rs.rows_affected(), None);
        let rs = sql(&mut *s, "select 1 as a; select 2 as b, 3 as c").await;
        assert_eq!(rs.columns().len(), 1);
        assert_eq!(rs.rows().len(), 1);
    });
}

#[test]
fn plain_batches_keep_session_state() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        sql(&mut *s, "use tempdb").await;
        let rs = sql(&mut *s, "select db_name() as d").await;
        assert_eq!(rs.rows()[0].0, [text("tempdb")]);
    });
}

#[test]
fn login_failure_has_no_password() {
    let Some(url) = url() else { return };
    run(async {
        let bad = url.replace("Sqler-Test-1pw", "Wrong-Pw-9x");
        let err = common::try_connect(&common::config(&bad))
            .await
            .err()
            .unwrap();
        let text = err.to_string();
        assert!(matches!(err, DriverError::Connection(_)), "{text}");
        assert!(!text.contains("Wrong-Pw-9x"), "{text}");
        assert!(!text.contains("14333"), "{text}");
    });
}

#[test]
fn cancel_ends_the_statement_and_the_session() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let handle = s.cancel_handle();
        // A cancel with nothing running does nothing.
        handle.cancel().await.unwrap();
        assert_eq!(sql(&mut *s, "select 1 as a").await.len(), 1);

        let started = Instant::now();
        let canceller = tokio::spawn({
            let handle = handle.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(500)).await;
                handle.cancel().await
            }
        });
        let err = query(&mut *s, &Query::new("waitfor delay '00:01:00'"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Cancelled), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        canceller.await.unwrap().unwrap();

        // The session ended with the cancel.
        let err = s.execute(&Query::new("select 1")).await.err().unwrap();
        assert!(
            matches!(
                err,
                DriverError::Connection(ref m)
                    if m == "the SQL Server connection was closed by a cancel; connect again"
            ),
            "{err:?}"
        );

        let mut fresh = connect(&url).await;
        assert_eq!(sql(&mut *fresh, "select 1 as a").await.len(), 1);
    });
}

#[test]
fn cancel_while_the_consumer_is_paused() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        let handle = s.cancel_handle();
        let q = Query::new(
            "select top (1000000) row_number() over (order by (select null)) as n \
             from sys.all_objects a cross join sys.all_objects b",
        );
        let mut stream = s.execute(&q).await.unwrap();
        // Read the columns and one batch, then stop reading.
        let mut seen = 0;
        while seen < 2 {
            let event = stream.next_event().await.unwrap().unwrap();
            assert!(!matches!(event, QueryEvent::Done { .. }));
            seen += 1;
        }
        handle.cancel().await.unwrap();
        let mut ended = false;
        for _ in 0..10 {
            match stream.next_event().await {
                Some(Err(DriverError::Cancelled)) => {
                    ended = true;
                    break;
                }
                Some(Ok(_)) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(ended, "the stream did not end with Cancelled");
        assert!(stream.next_event().await.is_none());
    });
}
