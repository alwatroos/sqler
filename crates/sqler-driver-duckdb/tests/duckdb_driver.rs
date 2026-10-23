// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! In-process checks of the `DuckDB` driver through `dyn` objects only (no container).

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{
    ConnectionConfig, DataType, Date, DriverId, Query, ResultSet, Time, Timestamp, TimestampTz,
    Value,
};
use sqler_driver_api::{Driver as _, DriverError, DriverRegistry, QueryEvent, Session, collect};
use sqler_driver_duckdb::{DRIVER_ID, DuckDbDriver, ENGINE_ID};

fn run<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn registry() -> DriverRegistry {
    let mut r = DriverRegistry::new();
    r.register(Box::new(DuckDbDriver::new())).unwrap();
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

async fn sql_err(session: &mut dyn Session, text: &str) -> String {
    match query(session, &Query::new(text)).await {
        Err(DriverError::Query(m)) => m,
        other => panic!("expected a query error, got {other:?}"),
    }
}

fn temp_db(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.duckdb"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("duckdb.wal"));
    path
}

fn cell(rs: &ResultSet, col: usize) -> &Value {
    rs.rows()[0].get(col).unwrap()
}

#[test]
fn ids() {
    assert_eq!(DuckDbDriver::new().id(), "duckdb");
    assert_eq!(ENGINE_ID, "duckdb");
}

#[test]
fn query_select_one() {
    run(async {
        let mut s = memory().await;
        let rs = sql(&mut *s, "select 1 as one").await;
        assert_eq!(rs.columns()[0].name, "one");
        assert_eq!(rs.columns()[0].data_type, DataType::Integer);
        assert_eq!(rs.columns()[0].native_type, None);
        assert_eq!(rs.len(), 1);
        assert_eq!(*cell(&rs, 0), Value::Int(1));
        s.close().await.unwrap();
    });
}

#[test]
fn file_is_created_and_persists_across_reconnect() {
    run(async {
        let path = temp_db("persist");
        let target = path.to_str().unwrap();
        let reg = registry();
        let mut s = reg.connect(&config(target)).await.unwrap();
        sql(&mut *s, "create table t (a integer)").await;
        sql(&mut *s, "insert into t values (7)").await;
        s.close().await.unwrap();
        assert!(path.exists());
        let mut s = reg.connect(&config(target)).await.unwrap();
        let rs = sql(&mut *s, "select a from t").await;
        assert_eq!(*cell(&rs, 0), Value::Int(7));
        s.close().await.unwrap();
    });
}

#[test]
fn missing_directory_error_hides_the_path() {
    run(async {
        let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("no-such-dir-p15")
            .join("x.duckdb");
        let target = target.to_str().unwrap().to_owned();
        let Err(err) = registry().connect(&config(&target)).await else {
            panic!("connect must fail");
        };
        assert!(matches!(err, DriverError::Connection(_)));
        let text = err.to_string();
        assert!(text.contains("could not be opened"), "{text}");
        assert!(!text.contains("no-such-dir-p15"), "{text}");
    });
}

#[test]
fn memory_databases_are_private() {
    run(async {
        let mut a = memory().await;
        let mut b = memory().await;
        sql(&mut *a, "create table only_a (x integer)").await;
        let m = sql_err(&mut *b, "select * from only_a").await;
        assert!(m.contains("only_a"), "{m}");
    });
}

#[test]
fn large_result_streams_in_bounded_batches() {
    run(async {
        let mut s = memory().await;
        let q = Query::new("select i, i::varchar as t from range(1000000) r(i)");
        let mut stream = s.execute(&q).await.unwrap();
        let mut seen_columns = false;
        let mut total = 0usize;
        let mut batches = 0usize;
        loop {
            match stream.next_event().await {
                Some(Ok(QueryEvent::Columns(cols))) => {
                    assert!(!seen_columns && total == 0);
                    assert_eq!(cols.len(), 2);
                    seen_columns = true;
                }
                Some(Ok(QueryEvent::Rows(rows))) => {
                    assert!(seen_columns);
                    assert!(!rows.is_empty() && rows.len() <= 500, "{}", rows.len());
                    total += rows.len();
                    batches += 1;
                }
                Some(Ok(QueryEvent::Done { rows_affected })) => {
                    assert!(seen_columns);
                    assert_eq!(rows_affected, None);
                    break;
                }
                Some(Ok(_)) => panic!("unknown event"),
                Some(Err(e)) => panic!("{e}"),
                None => panic!("ended before Done"),
            }
        }
        assert!(stream.next_event().await.is_none());
        assert_eq!(total, 1_000_000);
        assert!(batches >= 2000);
    });
}

#[test]
fn cancel_mid_stream_then_session_works() {
    run(async {
        let mut s = memory().await;
        let handle = s.cancel_handle();
        let q = Query::new("select i from range(1000000000) r(i)");
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
            let started = Instant::now();
            handle.cancel().await.unwrap();
            let mut outcome = None;
            while let Some(event) = stream.next_event().await {
                if let Err(e) = event {
                    outcome = Some(e);
                    break;
                }
            }
            assert!(
                matches!(outcome, Some(DriverError::Cancelled)),
                "{outcome:?}"
            );
            assert!(stream.next_event().await.is_none());
            assert!(started.elapsed() < Duration::from_secs(30));
        }
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(*cell(&rs, 0), Value::Int(1));
    });
}

#[test]
fn cancel_during_a_long_single_step() {
    run(async {
        let mut s = memory().await;
        let handle = s.cancel_handle();
        let q = Query::new("select sum(hash(i)) from range(100000000000) r(i)");
        let started = Instant::now();
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            handle.cancel().await.unwrap();
        };
        let (outcome, ()) = tokio::join!(query(&mut *s, &q), canceller);
        assert!(
            matches!(outcome, Err(DriverError::Cancelled)),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(30));
        let rs = sql(&mut *s, "select 2").await;
        assert_eq!(*cell(&rs, 0), Value::Int(2));
    });
}

#[test]
fn dropped_stream_frees_the_session() {
    run(async {
        let mut s = memory().await;
        {
            let q = Query::new("select i from range(1000000000) r(i)");
            let mut stream = s.execute(&q).await.unwrap();
            let _ = stream.next_event().await;
        }
        let rs = sql(&mut *s, "select 3").await;
        assert_eq!(*cell(&rs, 0), Value::Int(3));
    });
}

#[test]
fn types() {
    run(async {
        let mut s = memory().await;
        let rs = sql(
            &mut *s,
            "select 1234.5678::decimal(18,4) as d18, \
             12345678901234567890.12345678::decimal(38,8) as d38, \
             170141183460469231731687303715884105727::hugeint as h, \
             [1, 2, 3] as l, {'a': 1, 'b': 'x'} as st, \
             timestamp '2024-03-05 06:07:08.123456' as ts, \
             timestamptz '2024-03-05 06:07:08+00' as tz, \
             '\\xAA\\xBB'::blob as bl, date '2024-02-29' as d, time '12:34:56' as tm, \
             'NaN'::double as nan, NULL as n, true as b, 1.5::double as f, 'text' as t",
        )
        .await;
        let ts: Timestamp = "2024-03-05 06:07:08.123456".parse().unwrap();
        let expected = [
            Value::Decimal("1234.5678".parse().unwrap()),
            Value::Decimal("12345678901234567890.12345678".parse().unwrap()),
            Value::Decimal("170141183460469231731687303715884105727".parse().unwrap()),
            Value::Text("[1, 2, 3]".to_owned()),
            Value::Text("{a: 1, b: x}".to_owned()),
            Value::Timestamp(ts),
            Value::TimestampTz(
                TimestampTz::new("2024-03-05 06:07:08".parse().unwrap(), 0).unwrap(),
            ),
            Value::Bytes(vec![0xAA, 0xBB]),
            Value::Date("2024-02-29".parse::<Date>().unwrap()),
            Value::Time("12:34:56".parse::<Time>().unwrap()),
            Value::Text("NaN".to_owned()),
            Value::Null,
            Value::Bool(true),
            Value::Float(1.5),
            Value::Text("text".to_owned()),
        ];
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(
                cell(&rs, i),
                want,
                "column {} ({})",
                i,
                rs.columns()[i].name
            );
        }
        let types: Vec<_> = rs.columns().iter().map(|c| c.data_type).collect();
        assert_eq!(types[0], DataType::Decimal);
        assert_eq!(types[3], DataType::Other);
        assert_eq!(types[4], DataType::Other);
        assert_eq!(types[5], DataType::Timestamp);
        assert_eq!(types[6], DataType::TimestampTz);
        assert_eq!(types[7], DataType::Binary);
        assert_eq!(types[8], DataType::Date);
        assert_eq!(types[9], DataType::Time);
        assert_eq!(types[12], DataType::Boolean);
        assert_eq!(types[13], DataType::Float);
        assert_eq!(types[14], DataType::Text);
    });
}

#[test]
fn infinity_dates_become_text() {
    run(async {
        let mut s = memory().await;
        let rs = sql(
            &mut *s,
            "select 'infinity'::date as d, 'infinity'::timestamp as t",
        )
        .await;
        assert!(matches!(cell(&rs, 0), Value::Text(_)));
        assert!(matches!(cell(&rs, 1), Value::Text(_)));
    });
}

#[test]
fn bound_parameters() {
    run(async {
        let mut s = memory().await;
        let q = Query::new("select ?::bigint + 1 as a, ?::varchar as b, ?::boolean as c")
            .with_params(vec![Value::Int(41), Value::Text("hi".into()), Value::Null]);
        let rs = query(&mut *s, &q).await.unwrap();
        assert_eq!(*cell(&rs, 0), Value::Int(42));
        assert_eq!(*cell(&rs, 1), Value::Text("hi".into()));
        assert_eq!(*cell(&rs, 2), Value::Null);
    });
}

#[test]
fn writes_report_rows_affected_and_reread() {
    run(async {
        let mut s = memory().await;
        let create = sql(&mut *s, "create table w (id integer, name varchar)").await;
        assert!(create.rows_affected().is_none() || create.rows_affected() == Some(0));
        let ins = sql(&mut *s, "insert into w values (1, 'a'), (2, 'b'), (3, 'c')").await;
        assert_eq!(ins.columns().len(), 0);
        assert_eq!(ins.rows_affected(), Some(3));
        let upd = sql(&mut *s, "update w set name = 'z' where id >= 2").await;
        assert_eq!(upd.rows_affected(), Some(2));
        let del = sql(&mut *s, "delete from w where id = 1").await;
        assert_eq!(del.rows_affected(), Some(1));
        let rs = sql(&mut *s, "select id, name from w order by id").await;
        assert_eq!(rs.len(), 2);
        assert_eq!(*rs.rows()[0].get(1).unwrap(), Value::Text("z".into()));
        let ret = sql(&mut *s, "insert into w values (9, 'r') returning id").await;
        assert_eq!(ret.columns()[0].name, "id");
        assert_eq!(*cell(&ret, 0), Value::Int(9));
    });
}

#[test]
fn sandbox_blocks_files_extensions_and_reconfiguration() {
    run(async {
        let csv = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("p15-sandbox.csv");
        std::fs::write(&csv, "a\n1\n").unwrap();
        let mut s = memory().await;
        let path = csv.to_str().unwrap().replace('\'', "''");
        sql_err(&mut *s, &format!("select * from read_csv('{path}')")).await;
        sql_err(&mut *s, "install httpfs").await;
        sql_err(&mut *s, "set enable_external_access = true").await;
        sql_err(&mut *s, "set allow_community_extensions = true").await;
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(*cell(&rs, 0), Value::Int(1));
        let _ = std::fs::remove_file(&csv);
    });
}

#[test]
fn two_sessions_share_one_file() {
    run(async {
        let path = temp_db("shared");
        let target = path.to_str().unwrap();
        let reg = registry();
        let mut a = reg.connect(&config(target)).await.unwrap();
        let mut b = reg.connect(&config(target)).await.unwrap();
        sql(&mut *a, "create table s (v integer)").await;
        sql(&mut *a, "insert into s values (1)").await;
        let rs = sql(&mut *b, "select count(*) from s").await;
        assert_eq!(*cell(&rs, 0), Value::Int(1));
        sql(&mut *b, "insert into s values (2)").await;
        let rs = sql(&mut *a, "select sum(v)::bigint from s").await;
        assert_eq!(*cell(&rs, 0), Value::Int(3));
        a.close().await.unwrap();
        // The instance stays open for the other session, and reopens after both close.
        let rs = sql(&mut *b, "select count(*) from s").await;
        assert_eq!(*cell(&rs, 0), Value::Int(2));
        b.close().await.unwrap();
        let mut c = reg.connect(&config(target)).await.unwrap();
        let rs = sql(&mut *c, "select count(*) from s").await;
        assert_eq!(*cell(&rs, 0), Value::Int(2));
    });
}

#[test]
fn query_errors_never_contain_the_database_path() {
    run(async {
        let path = temp_db("scrub-me");
        let target = path.to_str().unwrap().to_owned();
        let mut s = registry().connect(&config(&target)).await.unwrap();
        let m = sql_err(&mut *s, "select * from missing_table").await;
        assert!(m.contains("missing_table"), "{m}");
        let m = sql_err(
            &mut *s,
            &format!("attach '{}' as other", target.replace('\'', "''")),
        )
        .await;
        assert!(!m.contains(&target) && m.contains("<database>"), "{m}");
        let m = sql_err(&mut *s, "select * from read_csv('scrub-me.csv')").await;
        assert!(!m.contains(&target), "{m}");
    });
}

#[test]
fn syntax_error_is_a_query_error() {
    run(async {
        let mut s = memory().await;
        sql_err(&mut *s, "selec 1").await;
        let rs = sql(&mut *s, "select 1").await;
        assert_eq!(rs.len(), 1);
    });
}
