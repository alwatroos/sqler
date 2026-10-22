// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `query.run`: the guard core against SQLite and scripted sessions, and the real path through a component.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty
)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use sqler_driver_api::QueryEvent;
use sqler_driver_api::sqler_core::{Column, DataType, Environment, Row, Value};
use sqler_plugin_api::{CapabilitySet, Limits, QueryLimits};
use sqler_sql::StatementClass;

use super::exec::{NOT_AVAILABLE, QueryCtx, run_guarded};
use super::support::{After, ScriptData, Scripted, SqliteData, TestPlugin, caps, data};
use super::{ApprovalStage, Decision, MAX_PARAMS, MAX_SQL_BYTES, PluginData, WriteReason};
use crate::error::ErrorCode;
use crate::runtime::wit::query::{QueryRequest, QueryResult};
use crate::runtime::wit::types::{PluginError, Value as W};

async fn guard<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("hang guard")
}

fn req(sql: &str) -> QueryRequest {
    QueryRequest {
        connection: "db".to_owned(),
        sql: sql.to_owned(),
        params: Vec::new(),
        max_rows: None,
        timeout_ms: None,
    }
}

/// One call of `run_guarded` as a plugin `who` with `caps` would make it.
async fn call(
    who: &TestPlugin,
    effective: &CapabilitySet,
    data: Option<Arc<PluginData>>,
    limits: QueryLimits,
    request: QueryRequest,
) -> Result<QueryResult, (ErrorCode, String)> {
    let ctx = QueryCtx {
        plugin: &who.plugin,
        effective,
        data,
        limits,
    };
    guard(run_guarded(&ctx, request))
        .await
        .map_err(|e: PluginError| (e.code.into(), e.message))
}

/// WIT values have no `PartialEq`; compare their `Debug` text.
fn show<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}")
}

fn limits() -> QueryLimits {
    Limits::DEFAULT.query
}

fn code<T: std::fmt::Debug>(r: Result<T, (ErrorCode, String)>) -> ErrorCode {
    r.unwrap_err().0
}

fn me() -> TestPlugin {
    TestPlugin::new("tester", "The Tester")
}

const INSERT: &str = "insert into users (id, name) values (?, ?)";

fn insert_request() -> QueryRequest {
    let mut r = req(INSERT);
    r.params = vec![W::Integer(9), W::Text("zed".into())];
    r
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_runs_with_db_read() {
    let who = me();
    let src = SqliteData::new("read", None, false).await;
    let d = data(src.clone(), None);
    let r = call(
        &who,
        &caps(&["db.read"]),
        Some(d),
        limits(),
        req("select id, name from users order by id"),
    )
    .await
    .unwrap();
    let names: Vec<_> = r.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name"]);
    assert_eq!(r.rows.len(), 5);
    assert_eq!(
        show(&r.rows[0]),
        show(&[W::Integer(1), W::Text("ann".into())])
    );
    assert!(!r.truncated);
    assert_eq!(src.executed(), ["select id, name from users order by id"]);
    assert_eq!(src.releases.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn nothing_is_allowed_without_a_db_capability_and_the_service_comes_second() {
    let who = me();
    let none = CapabilitySet::new();
    // No capability: denied even when there is no service.
    assert_eq!(
        code(call(&who, &none, None, limits(), req("select 1")).await),
        ErrorCode::CapabilityDenied
    );
    // A capability and no service: failed with the pinned message.
    let (c, m) = call(&who, &caps(&["db.read"]), None, limits(), req(""))
        .await
        .unwrap_err();
    assert_eq!((c, m.as_str()), (ErrorCode::Failed, NOT_AVAILABLE));
    let (c, m) = call(&who, &caps(&["db.write"]), None, limits(), req("x"))
        .await
        .unwrap_err();
    assert_eq!((c, m.as_str()), (ErrorCode::Failed, NOT_AVAILABLE));
}

#[tokio::test(flavor = "current_thread")]
async fn a_write_without_db_write_is_denied_and_nothing_runs() {
    let who = me();
    let src = SqliteData::new("nowrite", None, false).await;
    let approver = Scripted::always(Decision::Approve);
    let d = data(src.clone(), Some(approver.clone()));
    let r = call(
        &who,
        &caps(&["db.read"]),
        Some(d),
        limits(),
        insert_request(),
    )
    .await;
    assert_eq!(code(r), ErrorCode::CapabilityDenied);
    assert!(src.executed().is_empty());
    assert!(
        approver.requests().is_empty(),
        "no dialog for a call that cannot run"
    );
    assert_eq!(src.acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_without_db_read_is_denied_even_with_db_write() {
    let who = me();
    let src = SqliteData::new("nodbread", None, false).await;
    let d = data(src.clone(), Some(Scripted::always(Decision::Approve)));
    let r = call(
        &who,
        &caps(&["db.write"]),
        Some(d),
        limits(),
        req("select 1"),
    )
    .await;
    assert_eq!(code(r), ErrorCode::CapabilityDenied);
    assert!(src.executed().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn an_approved_write_runs_and_the_request_names_everything() {
    let who = me();
    let src = SqliteData::new("approve", None, false).await;
    let approver = Scripted::always(Decision::Approve);
    let d = data(src.clone(), Some(approver.clone()));
    let r = call(
        &who,
        &caps(&["db.write"]),
        Some(d),
        limits(),
        insert_request(),
    )
    .await
    .unwrap();
    assert_eq!(r.rows_affected, Some(1));
    assert_eq!(src.executed(), [INSERT]);
    let requests = approver.requests();
    assert_eq!(requests.len(), 1);
    let q = &requests[0];
    assert_eq!(q.sql, INSERT);
    assert_eq!(q.params, [Value::Int(9), Value::Text("zed".into())]);
    assert_eq!(q.plugin.as_str(), "tester");
    assert_eq!(q.plugin_name, "The Tester");
    assert_eq!(q.connection.id, "db");
    assert_eq!(q.class, StatementClass::Write);
    assert_eq!(q.reason, WriteReason::Write);
    assert_eq!(q.stage, ApprovalStage::Confirm);
    // The write is real.
    let d = data(src.clone(), None);
    let r = call(
        &who,
        &caps(&["db.read"]),
        Some(d),
        limits(),
        req("select name from users where id = 9"),
    )
    .await
    .unwrap();
    assert_eq!(show(&r.rows), show(&[vec![W::Text("zed".into())]]));
}

#[tokio::test(flavor = "current_thread")]
async fn deny_and_no_approver_are_declined_and_nothing_runs() {
    let who = me();
    let src = SqliteData::new("deny", None, false).await;
    let approver = Scripted::always(Decision::Deny);
    for d in [
        data(src.clone(), Some(approver.clone())),
        data(src.clone(), None),
    ] {
        let r = call(
            &who,
            &caps(&["db.write"]),
            Some(d),
            limits(),
            insert_request(),
        )
        .await;
        assert_eq!(code(r), ErrorCode::Declined);
    }
    assert!(src.executed().is_empty());
    assert_eq!(src.acquires.load(Ordering::SeqCst), 0);
    assert_eq!(approver.requests().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn prod_asks_twice_and_a_second_denial_runs_nothing() {
    let who = me();
    let src = SqliteData::new("prod", Some(Environment::Prod), false).await;
    let approver = Scripted::always(Decision::Approve);
    let d = data(src.clone(), Some(approver.clone()));
    call(
        &who,
        &caps(&["db.write"]),
        Some(d),
        limits(),
        insert_request(),
    )
    .await
    .unwrap();
    let stages: Vec<_> = approver.requests().iter().map(|r| r.stage).collect();
    assert_eq!(stages, [ApprovalStage::Confirm, ApprovalStage::ProdSecond]);
    assert_eq!(src.executed().len(), 1);

    let src = SqliteData::new("prod2", Some(Environment::Prod), false).await;
    let approver = Scripted::new(|r| {
        if r.stage == ApprovalStage::ProdSecond {
            Decision::Deny
        } else {
            Decision::Approve
        }
    });
    let d = data(src.clone(), Some(approver.clone()));
    let r = call(
        &who,
        &caps(&["db.write"]),
        Some(d),
        limits(),
        insert_request(),
    )
    .await;
    assert_eq!(code(r), ErrorCode::Declined);
    assert_eq!(approver.requests().len(), 2);
    assert!(src.executed().is_empty());
    assert_eq!(src.acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_read_only_connection_is_declined_for_anything_but_a_read() {
    let who = me();
    let src = SqliteData::new("ro", None, true).await;
    let approver = Scripted::always(Decision::Approve);
    let d = data(src.clone(), Some(approver.clone()));
    let all = caps(&["db.read", "db.write"]);
    let r = call(&who, &all, Some(d.clone()), limits(), insert_request()).await;
    assert_eq!(code(r), ErrorCode::Declined);
    assert!(approver.requests().is_empty());
    assert!(src.executed().is_empty());
    call(&who, &all, Some(d), limits(), req("select 1"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_statements_need_db_write_and_an_approval() {
    let who = me();
    // `frobnicate` is Unknown for SQLite; `select 1` on mssql has no dialect, so it is Unknown too.
    let events = vec![
        QueryEvent::Columns(vec![Column::new("a", DataType::Integer)]),
        QueryEvent::Rows(vec![Row(vec![Value::Int(1)])]),
        QueryEvent::Done {
            rows_affected: None,
        },
    ];
    for (driver, sql) in [("sqlite", "frobnicate x"), ("mssql", "select 1")] {
        let src = ScriptData::with_driver(driver, events.clone(), After::End);
        let approver = Scripted::always(Decision::Approve);
        let with = data(src.clone(), Some(approver.clone()));
        let mut r = req(sql);
        r.connection = "fake".into();
        assert_eq!(
            code(
                call(
                    &who,
                    &caps(&["db.read"]),
                    Some(with.clone()),
                    limits(),
                    r.clone()
                )
                .await
            ),
            ErrorCode::CapabilityDenied,
            "{driver}"
        );
        assert_eq!(
            code(
                call(
                    &who,
                    &caps(&["db.write"]),
                    Some(data(src.clone(), None)),
                    limits(),
                    r.clone()
                )
                .await
            ),
            ErrorCode::Declined
        );
        assert_eq!(src.acquires.load(Ordering::SeqCst), 0);
        call(&who, &caps(&["db.write"]), Some(with), limits(), r)
            .await
            .unwrap();
        let q = &approver.requests()[0];
        assert_eq!(
            (q.class, q.reason),
            (StatementClass::Unknown, WriteReason::Unknown)
        );
        assert_eq!(q.sql, sql);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn explain_analyze_needs_an_approval() {
    let who = me();
    let events = vec![
        QueryEvent::Columns(vec![Column::new("QUERY PLAN", DataType::Text)]),
        QueryEvent::Done {
            rows_affected: None,
        },
    ];
    let src = ScriptData::with_driver("postgres", events, After::End);
    let approver = Scripted::always(Decision::Approve);
    let d = data(src.clone(), Some(approver.clone()));
    let mut r = req("EXPLAIN (ANALYZE, BUFFERS) select 1");
    r.connection = "fake".into();
    assert_eq!(
        code(
            call(
                &who,
                &caps(&["db.read"]),
                Some(d.clone()),
                limits(),
                r.clone()
            )
            .await
        ),
        ErrorCode::CapabilityDenied
    );
    call(&who, &caps(&["db.write"]), Some(d), limits(), r.clone())
        .await
        .unwrap();
    let q = &approver.requests()[0];
    assert_eq!(
        (q.class, q.reason),
        (StatementClass::Read, WriteReason::ExplainAnalyze)
    );
    // A plain EXPLAIN is a read.
    let mut plain = req("EXPLAIN select 1");
    plain.connection = "fake".into();
    let d = data(src, None);
    call(&who, &caps(&["db.read"]), Some(d), limits(), plain)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn argument_and_connection_errors() {
    let who = me();
    let src = SqliteData::new("args", None, false).await;
    let d = data(src.clone(), Some(Scripted::always(Decision::Approve)));
    let all = caps(&["db.read", "db.write"]);
    let go = |r: QueryRequest| call(&who, &all, Some(d.clone()), limits(), r);
    for sql in ["", "  \n", "-- nothing", "select 1; select 2"] {
        assert_eq!(
            code(go(req(sql)).await),
            ErrorCode::InvalidArgument,
            "{sql:?}"
        );
    }
    let mut unknown = req("select 1");
    unknown.connection = "nope".into();
    assert_eq!(code(go(unknown).await), ErrorCode::NotFound);
    let mut long = req("select 1");
    long.sql = format!("select '{}'", "x".repeat(MAX_SQL_BYTES));
    assert_eq!(code(go(long).await), ErrorCode::InvalidArgument);
    let mut many = req("select 1");
    many.params = vec![W::Null; MAX_PARAMS + 1];
    assert_eq!(code(go(many).await), ErrorCode::InvalidArgument);
    let mut zero_rows = req("select 1");
    zero_rows.max_rows = Some(0);
    assert_eq!(code(go(zero_rows).await), ErrorCode::InvalidArgument);
    let mut zero_time = req("select 1");
    zero_time.timeout_ms = Some(0);
    assert_eq!(code(go(zero_time).await), ErrorCode::InvalidArgument);
    assert!(src.executed().is_empty());
    assert_eq!(src.acquires.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn parameters_round_trip_and_a_bad_date_is_invalid() {
    let who = me();
    let src = SqliteData::new("params", None, false).await;
    let d = data(src.clone(), None);
    let read = caps(&["db.read"]);
    let mut r = req("select ?, ?, ?, ?, ?");
    r.params = vec![
        W::Integer(7),
        W::Text("héllo".into()),
        W::Float(1.5),
        W::Bytes(vec![1, 2, 3]),
        W::Null,
    ];
    let out = call(&who, &read, Some(d.clone()), limits(), r)
        .await
        .unwrap();
    assert_eq!(
        show(&out.rows[0]),
        show(&[
            W::Integer(7),
            W::Text("héllo".into()),
            W::Float(1.5),
            W::Bytes(vec![1, 2, 3]),
            W::Null
        ])
    );
    let mut ok_date = req("select ?");
    ok_date.params = vec![W::Date("2024-02-29".into())];
    call(&who, &read, Some(d.clone()), limits(), ok_date)
        .await
        .unwrap();
    let before = src.acquires.load(Ordering::SeqCst);
    for bad in [
        W::Date("2023-02-29".into()),
        W::Time("99:00".into()),
        W::Decimal("x".into()),
    ] {
        let mut r = req("select ?");
        r.params = vec![bad];
        assert_eq!(
            code(call(&who, &read, Some(d.clone()), limits(), r).await),
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(src.acquires.load(Ordering::SeqCst), before);
}

#[tokio::test(flavor = "current_thread")]
async fn the_row_cap_defaults_requests_and_clamps() {
    let who = me();
    let src = SqliteData::new("rows", None, false).await;
    let d = data(src.clone(), None);
    let read = caps(&["db.read"]);
    let small = QueryLimits {
        max_rows_default: 2,
        max_rows_cap: 3,
        ..limits()
    };
    let run = |max_rows: Option<u32>| {
        let mut r = req("select id from users order by id");
        r.max_rows = max_rows;
        call(&who, &read, Some(d.clone()), small, r)
    };
    let r = run(None).await.unwrap();
    assert_eq!((r.rows.len(), r.truncated), (2, true), "default");
    let r = run(Some(1)).await.unwrap();
    assert_eq!((r.rows.len(), r.truncated), (1, true), "requested");
    let r = run(Some(100)).await.unwrap();
    assert_eq!((r.rows.len(), r.truncated), (3, true), "clamped");
    // Exactly the cap and then Done is not truncated.
    let mut r = req("select id from users where id <= 3 order by id");
    r.max_rows = Some(3);
    let r = call(&who, &read, Some(d.clone()), small, r).await.unwrap();
    assert_eq!((r.rows.len(), r.truncated), (3, false));
    assert_eq!(
        src.releases.load(Ordering::SeqCst),
        src.acquires.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_byte_cap_truncates_without_cancelling() {
    let who = me();
    let chunk = "x".repeat(1 << 20);
    let rows: Vec<Row> = (0..10)
        .map(|_| Row(vec![Value::Text(chunk.clone())]))
        .collect();
    let events = vec![
        QueryEvent::Columns(vec![Column::new("t", DataType::Text)]),
        QueryEvent::Rows(rows),
        QueryEvent::Done {
            rows_affected: None,
        },
    ];
    let src = ScriptData::new(events, After::End);
    let d = data(src.clone(), None);
    let big = QueryLimits {
        max_rows_cap: 100,
        max_rows_default: 100,
        ..limits()
    };
    let mut r = req("select t");
    r.connection = "fake".into();
    let out = call(&who, &caps(&["db.read"]), Some(d), big, r)
        .await
        .unwrap();
    // 1 MiB + 16 per cell: seven rows fit in 8 MiB, the eighth does not.
    assert_eq!(out.rows.len(), 7);
    assert!(out.truncated);
    assert_eq!(src.cancels.load(Ordering::SeqCst), 0);
    assert_eq!(src.releases.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_timeout_cancels_the_driver_releases_the_session_and_returns_no_rows() {
    let who = me();
    for src in [ScriptData::hanging(), ScriptData::hanging_execute()] {
        let d = data(src.clone(), None);
        let mut r = req("select a");
        r.connection = "fake".into();
        r.timeout_ms = Some(30);
        let out = call(&who, &caps(&["db.read"]), Some(d), limits(), r).await;
        assert_eq!(code(out), ErrorCode::Timeout);
        assert_eq!(src.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(src.releases.load(Ordering::SeqCst), 1);
        assert_eq!(src.acquires.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_driver_error_is_failed_and_the_session_goes_back() {
    let who = me();
    let src = SqliteData::new("fail", None, false).await;
    let d = data(src.clone(), None);
    let (c, m) = call(
        &who,
        &caps(&["db.read"]),
        Some(d),
        limits(),
        req("select * from nope"),
    )
    .await
    .unwrap_err();
    assert_eq!(c, ErrorCode::Failed);
    assert!(m.chars().count() <= 512 && !m.is_empty(), "{m}");
    assert_eq!(src.releases.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_connect_error_text_never_reaches_the_guest() {
    let who = me();
    let src = ScriptData::new(Vec::new(), After::End);
    *src.acquire_error.lock().unwrap() = Some(super::DataError::Driver(
        "tls handshake with db.prod.example:5432 as user admin password=hunter2 failed".to_owned(),
    ));
    let d = data(src.clone(), None);
    let mut request = req("select 1");
    request.connection = "fake".to_owned();
    let (c, m) = call(&who, &caps(&["db.read"]), Some(d), limits(), request)
        .await
        .unwrap_err();
    assert_eq!(c, ErrorCode::Failed);
    for needle in ["db.prod", "admin", "hunter2", "5432", "tls"] {
        assert!(!m.contains(needle), "{m}");
    }
    assert_eq!(m, "the connection could not be opened");
}

#[test]
fn a_host_written_unavailable_text_passes_and_a_driver_text_does_not() {
    use super::DataError;
    let e = super::exec::data_error(&DataError::NotAvailable("a transaction is open".into()));
    assert_eq!(e.message, "a transaction is open");
    let e = super::exec::data_error(&DataError::Driver("host=db.prod user=x".into()));
    assert!(!e.message.contains("db.prod") && !e.message.contains("user"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_dropped_call_cancels_and_releases_the_session() {
    let who = me();
    let src = ScriptData::hanging();
    let mut reached = src.take_reached();
    let mut released = src.take_released();
    let d = data(src.clone(), None);
    let all = caps(&["db.read"]);
    let mut r = req("select a");
    r.connection = "fake".into();
    let ctx = QueryCtx {
        plugin: &who.plugin,
        effective: &all,
        data: Some(d),
        limits: limits(),
    };
    {
        let run = run_guarded(&ctx, r);
        tokio::pin!(run);
        // Polls the call until the driver hangs, then drops it (as a deadline or a cancel would).
        tokio::select! {
            r = &mut run => panic!("the call ended: {r:?}"),
            got = guard(reached.recv()) => assert!(got.is_some()),
        }
    }
    assert!(guard(released.recv()).await.is_some());
    assert_eq!(src.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(src.releases.load(Ordering::SeqCst), 1);
}

#[cfg(feature = "cl")]
mod wasm {
    use super::*;
    use crate::error::CallError;
    use crate::query::support::wasm::{Fixture, command, request, run_query};
    use crate::runtime::wit::types as wt;

    fn to_pair(e: &wt::PluginError) -> (ErrorCode, String) {
        (e.code.into(), e.message.clone())
    }

    #[test]
    fn a_read_through_the_host_state_works() {
        let (f, src) = Fixture::serving(&["db.read"], || SqliteData::new("wasm-read", None, false));
        let out = f
            .block_on(command(
                &f,
                run_query(request("db", "select count(*) from users")),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(show(&out.rows), show(&[vec![W::Integer(5)]]));
        assert_eq!(src.executed().len(), 1);
    }

    #[test]
    fn the_gate_runs_on_the_real_path_too() {
        // db.read only: a write is denied; no service: failed.
        let f = Fixture::new(&["db.read"], None);
        let e = f
            .block_on(command(&f, run_query(request("db", "select 1"))))
            .unwrap()
            .unwrap_err();
        assert_eq!(to_pair(&e), (ErrorCode::Failed, NOT_AVAILABLE.to_owned()));
        let f = Fixture::new(&[], None);
        let e = f
            .block_on(command(&f, run_query(request("db", "select 1"))))
            .unwrap()
            .unwrap_err();
        assert_eq!(to_pair(&e).0, ErrorCode::CapabilityDenied);
    }

    fn hanging_fixture() -> (Fixture, Arc<ScriptData>) {
        let src = ScriptData::hanging();
        let f = Fixture::new(&["db.read"], Some(data(src.clone(), None)));
        (f, src)
    }

    #[test]
    fn cancelling_the_call_cancels_and_releases_the_session() {
        let (f, src) = hanging_fixture();
        let (mut reached, mut released) = (src.take_reached(), src.take_released());
        let outcome = f.block_on(async {
            let mut r = request("fake", "select a");
            r.timeout_ms = Some(60_000);
            let call = command(&f, run_query(r));
            assert!(reached.recv().await.is_some());
            call.cancel();
            let outcome = call.await;
            assert!(released.recv().await.is_some());
            outcome
        });
        assert_eq!(outcome.unwrap_err(), CallError::Cancelled);
        assert_eq!(src.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(src.releases.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refreshing_the_grants_cancels_and_releases_the_session() {
        let (f, src) = hanging_fixture();
        let (mut reached, mut released) = (src.take_reached(), src.take_released());
        let outcome = f.block_on(async {
            let mut r = request("fake", "select a");
            r.timeout_ms = Some(60_000);
            let call = command(&f, run_query(r));
            assert!(reached.recv().await.is_some());
            f.host.refresh_grants(&f.id);
            let outcome = call.await;
            assert!(released.recv().await.is_some());
            outcome
        });
        assert!(outcome.is_err());
        assert_eq!(src.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(src.releases.load(Ordering::SeqCst), 1);
    }
}
