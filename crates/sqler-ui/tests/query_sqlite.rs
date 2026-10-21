// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plans, binds and runs scripts on a real in-memory `SQLite` session.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_api::{DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::Dialect;
use sqler_ui::grid::edit::SchemaProbe;
use sqler_ui::grid::{GridCancel, GridLimits, GridModel, GridSink, IngestState, grid_channel_with};
use sqler_ui::query::{
    BufferSnapshot, ParamInput, ParamKey, ParamKind, QueryJob, QueryJobDone, RunEvent, RunId,
    RunMode, RunStop, bind, plan,
};
use sqler_ui::tabs::{TabConnection, TabId, TabKind, TabStrip};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::UnboundedReceiver;

const D: Option<Dialect> = Some(Dialect::Sqlite);

fn runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn tab_id() -> TabId {
    let connection = TabConnection {
        name: "t".to_owned(),
        driver: DriverId::new(DRIVER_ID).unwrap(),
        color: None,
        environment: None,
    };
    TabStrip::default().open(connection, TabKind::Query)
}

async fn session() -> Box<dyn Session> {
    let mut registry = DriverRegistry::new();
    registry.register(Box::new(SqliteDriver::new())).unwrap();
    let config = ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
    registry.connect(&config).await.unwrap()
}

struct Started {
    stop: RunStop,
    grids: Vec<GridModel>,
    cancels: Vec<GridCancel>,
    events: UnboundedReceiver<RunEvent>,
    handle: tokio::task::JoinHandle<QueryJobDone>,
}

/// Plans `text` in run-all mode, binds `inputs` and spawns the job on `session`.
fn start(rt: &Runtime, session: Box<dyn Session>, text: &str, inputs: &[ParamInput]) -> Started {
    start_with(rt, session, text, inputs, GridLimits::default())
}

/// Like [`start`], with the page limits of every grid.
fn start_with(
    rt: &Runtime,
    session: Box<dyn Session>,
    text: &str,
    inputs: &[ParamInput],
    limits: GridLimits,
) -> Started {
    start_probed(rt, session, text, inputs, limits, |_| Vec::new())
}

/// Like [`start_with`], with the schema probes `probes` builds from the bound statements.
fn start_probed(
    rt: &Runtime,
    session: Box<dyn Session>,
    text: &str,
    inputs: &[ParamInput],
    limits: GridLimits,
    probes: impl FnOnce(&[sqler_ui::query::BoundStatement]) -> Vec<Option<SchemaProbe>>,
) -> Started {
    let buf = BufferSnapshot::at_cursor(text, 0);
    let plan = plan(D, &buf, RunMode::All).unwrap();
    let statements = bind(D, &plan, inputs).unwrap();
    let (grids, sinks): (Vec<_>, Vec<_>) =
        statements.iter().map(|_| grid_channel_with(limits)).unzip();
    let cancels = sinks.iter().map(GridSink::canceller).collect();
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let stop = RunStop::new(&sinks);
    let probes = probes(&statements);
    let job = QueryJob {
        tab: tab_id(),
        run: RunId::new(1),
        session,
        statements,
        sinks,
        stop: stop.clone(),
        events: tx,
        probes,
    };
    let handle = rt.spawn(job.run());
    Started {
        stop,
        grids,
        cancels,
        events,
        handle,
    }
}

fn drain(events: &mut UnboundedReceiver<RunEvent>) -> Vec<RunEvent> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

fn settle(grid: &mut GridModel) {
    grid.poll(usize::MAX);
}

#[test]
fn run_all_streams_each_statement_into_its_grid() {
    let rt = runtime();
    let s = rt.block_on(session());
    let text = "create table t (a integer, b text);\n\
                insert into t values (?, 'x'), (2, 'y');\n\
                select a, b from t order by a;";
    let inputs = [ParamInput {
        key: ParamKey::Anonymous {
            statement: 1,
            ordinal: 0,
        },
        kind: ParamKind::Integer,
        text: "5".to_owned(),
    }];
    let mut run = start(&rt, s, text, &inputs);
    let done = rt.block_on(&mut run.handle).unwrap();
    assert_eq!(done.run, RunId::new(1));

    let events = drain(&mut run.events);
    let kinds: Vec<_> = events
        .iter()
        .map(|e| match e {
            RunEvent::Started { index } => ('s', *index),
            RunEvent::Finished { index, .. } => ('f', *index),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [('s', 0), ('f', 0), ('s', 1), ('f', 1), ('s', 2), ('f', 2)]
    );
    assert!(matches!(
        events[3],
        RunEvent::Finished {
            rows: 0,
            rows_affected: Some(2),
            ..
        }
    ));
    assert!(matches!(events[5], RunEvent::Finished { rows: 2, .. }));

    for grid in &mut run.grids {
        settle(grid);
        assert!(matches!(grid.state(), IngestState::Done { .. }));
    }
    let grid = &run.grids[2];
    assert_eq!(grid.row_count(), 2);
    assert_eq!(grid.columns().len(), 2);
    assert_eq!(
        grid.row(0).unwrap().get(0).unwrap().to_value(),
        Value::Int(2)
    );
    assert_eq!(
        grid.row(1).unwrap().get(0).unwrap().to_value(),
        Value::Int(5)
    );
    assert_eq!(
        grid.row(1).unwrap().get(1).unwrap().to_value(),
        Value::Text("x".to_owned())
    );
}

#[test]
fn a_failing_statement_stops_the_run_and_returns_the_session() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start(&rt, s, "select 1; select * from nope; select 3", &[]);
    let mut done = rt.block_on(&mut run.handle).unwrap();

    let events = drain(&mut run.events);
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(matches!(events[0], RunEvent::Started { index: 0 }));
    assert!(matches!(
        events[1],
        RunEvent::Finished {
            index: 0,
            rows: 1,
            ..
        }
    ));
    assert!(matches!(events[2], RunEvent::Started { index: 1 }));
    let RunEvent::Failed { index, message, .. } = &events[3] else {
        panic!("{:?}", events[3]);
    };
    assert_eq!(*index, 1);
    assert!(message.contains("nope"), "{message}");

    settle(&mut run.grids[0]);
    assert!(matches!(run.grids[0].state(), IngestState::Done { .. }));
    settle(&mut run.grids[1]);
    assert!(matches!(run.grids[1].state(), IngestState::Failed(_)));
    settle(&mut run.grids[2]);
    assert!(matches!(run.grids[2].state(), IngestState::Failed(_)));

    let rows = rt.block_on(async {
        let query = Query::new("select 7");
        let mut stream = done.session.execute(&query).await.unwrap();
        collect(&mut *stream).await.unwrap()
    });
    assert_eq!(rows.rows().len(), 1);
}

#[test]
fn cancel_during_a_long_query_ends_the_run_and_keeps_the_session() {
    let rt = runtime();
    let s = rt.block_on(session());
    let handle = s.cancel_handle();
    let text = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 1000000000) \
                SELECT x FROM c; select 2";
    let mut run = start(&rt, s, text, &[]);

    let mut done = rt.block_on(async {
        let first = run.events.recv().await.unwrap();
        assert!(matches!(first, RunEvent::Started { index: 0 }));
        tokio::time::sleep(Duration::from_millis(100)).await;
        // `stop` wakes the statement even if 100k rows arrived first and it is paused.
        run.stop.cancel();
        handle.cancel().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), &mut run.handle)
            .await
            .unwrap_or_else(|_| panic!("the cancelled job did not finish"))
            .unwrap()
    });

    let events = drain(&mut run.events);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0], RunEvent::Cancelled { index: 0 }));
    settle(&mut run.grids[0]);
    assert_eq!(run.grids[0].state(), &IngestState::Cancelled);

    let rows = rt.block_on(async {
        let query = Query::new("select 1");
        let mut stream = done.session.execute(&query).await.unwrap();
        collect(&mut *stream).await.unwrap()
    });
    assert_eq!(rows.rows().len(), 1);
}

#[test]
fn a_stop_flag_set_before_the_next_statement_cancels_it() {
    let rt = runtime();
    let s = rt.block_on(session());
    let buf = BufferSnapshot::at_cursor("select 1; select 2", 0);
    let plan = plan(D, &buf, RunMode::All).unwrap();
    let statements = bind(D, &plan, &[]).unwrap();
    let (_grids, sinks): (Vec<_>, Vec<_>) = statements
        .iter()
        .map(|_| grid_channel_with(GridLimits::default()))
        .unzip();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let stop = RunStop::new(&sinks);
    stop.cancel();
    let job = QueryJob {
        tab: tab_id(),
        run: RunId::new(3),
        session: s,
        statements,
        sinks,
        stop,
        events: tx,
        probes: Vec::new(),
    };
    let done = rt.block_on(job.run());
    assert_eq!(done.run, RunId::new(3));
    let events = drain(&mut events);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], RunEvent::Cancelled { index: 0 }));
}

#[test]
fn parameters_are_bound_not_spliced() {
    let rt = runtime();
    let s = rt.block_on(session());
    let evil = "x'); drop table t; --";
    let inputs = [
        ParamInput {
            key: ParamKey::Positional(1),
            kind: ParamKind::Auto,
            text: "7".to_owned(),
        },
        ParamInput {
            key: ParamKey::Named(":name".to_owned()),
            kind: ParamKind::Text,
            text: evil.to_owned(),
        },
    ];
    let mut run = start(
        &rt,
        s,
        "select ?1 as a, :name as b, :name as c, 1.5 as d",
        &inputs,
    );
    rt.block_on(&mut run.handle).unwrap();
    settle(&mut run.grids[0]);
    assert!(matches!(run.grids[0].state(), IngestState::Done { .. }));
    let row = run.grids[0].row(0).unwrap().to_row().0;
    assert_eq!(row[0], Value::Int(7));
    assert_eq!(row[1], Value::Text(evil.to_owned()));
    assert_eq!(row[2], Value::Text(evil.to_owned()));
    assert_eq!(row[3], Value::Float(1.5));
}

#[test]
fn named_and_anonymous_parameters_in_a_script() {
    let rt = runtime();
    let s = rt.block_on(session());
    let buf = BufferSnapshot::at_cursor("select :id, ?, :id", 0);
    let p = plan(D, &buf, RunMode::Statement).unwrap();
    assert_eq!(p.params.len(), 2);
    let inputs: Vec<_> = p
        .params
        .iter()
        .zip(["10", "twelve"])
        .map(|(key, text)| ParamInput {
            key: key.clone(),
            kind: ParamKind::Auto,
            text: text.to_owned(),
        })
        .collect();
    let mut run = start(&rt, s, "select :id, ?, :id", &inputs);
    rt.block_on(&mut run.handle).unwrap();
    settle(&mut run.grids[0]);
    let row = run.grids[0].row(0).unwrap().to_row().0;
    assert_eq!(row[0], Value::Int(10));
    assert_eq!(row[1], Value::Text("twelve".to_owned()));
    assert_eq!(row[2], Value::Int(10));
}

/// 5,000 rows, then a second statement.
const PAGED: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 5000) \
                     SELECT x FROM c; select 2";

/// Polls `grid` until `want`, with a deadline.
fn wait_for(grid: &mut GridModel, want: &IngestState) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while grid.state() != want {
        assert!(
            Instant::now() < deadline,
            "stuck in {:?}, wanted {want:?}",
            grid.state()
        );
        grid.poll(usize::MAX);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The job result, within a deadline.
fn finish(rt: &Runtime, run: &mut Started) -> QueryJobDone {
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), &mut run.handle)
            .await
            .unwrap_or_else(|_| panic!("the job did not finish"))
            .unwrap()
    })
}

fn select_one(rt: &Runtime, done: &mut QueryJobDone) {
    let rows = rt.block_on(async {
        let query = Query::new("select 1");
        let mut stream = done.session.execute(&query).await.unwrap();
        collect(&mut *stream).await.unwrap()
    });
    assert_eq!(rows.rows().len(), 1);
}

#[test]
fn the_page_bound_pauses_the_statement_and_holds_the_run() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_with(&rt, s, PAGED, &[], GridLimits::rows(1_000));
    wait_for(&mut run.grids[0], &IngestState::Paused);
    assert_eq!(run.grids[0].row_count(), 1_000);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!run.handle.is_finished());
    let events = drain(&mut run.events);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0], RunEvent::Started { index: 0 }));
    // Release the session: closing the grids ends the run.
    run.grids.clear();
    finish(&rt, &mut run);
}

#[test]
fn fetch_more_continues_until_the_statement_finishes() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_with(&rt, s, PAGED, &[], GridLimits::rows(1_000));
    let mut pages = 1;
    loop {
        wait_for(&mut run.grids[0], &IngestState::Paused);
        assert_eq!(run.grids[0].row_count(), pages * 1_000);
        assert!(run.grids[0].fetch_more());
        pages += 1;
        if pages == 5 {
            break;
        }
    }
    // The fifth page ends exactly at the result: `Done`, not `Paused`.
    wait_for(
        &mut run.grids[0],
        &IngestState::Done {
            rows_affected: None,
        },
    );
    assert_eq!(run.grids[0].row_count(), 5_000);
    let mut done = finish(&rt, &mut run);
    let events = drain(&mut run.events);
    assert!(
        events.iter().any(|e| matches!(
            e,
            RunEvent::Finished {
                index: 0,
                rows: 5_000,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(RunEvent::Finished { index: 1, .. })
    ));
    select_one(&rt, &mut done);
}

/// Cancelling only `stop` (no sink canceller, no driver handle) must wake a paused statement.
#[test]
fn stop_alone_wakes_a_paused_job() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_with(&rt, s, PAGED, &[], GridLimits::rows(1_000));
    wait_for(&mut run.grids[0], &IngestState::Paused);
    run.stop.cancel();
    let mut done = finish(&rt, &mut run);
    let events = drain(&mut run.events);
    assert!(
        matches!(events.last(), Some(RunEvent::Cancelled { index: 0 })),
        "{events:?}"
    );
    settle(&mut run.grids[0]);
    assert_eq!(run.grids[0].state(), &IngestState::Cancelled);
    select_one(&rt, &mut done);
}

#[test]
fn cancel_while_paused_ends_the_run_and_keeps_the_session() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_with(&rt, s, PAGED, &[], GridLimits::rows(1_000));
    wait_for(&mut run.grids[0], &IngestState::Paused);
    run.stop.cancel();
    for cancel in &run.cancels {
        cancel.cancel();
    }
    let mut done = finish(&rt, &mut run);
    let events = drain(&mut run.events);
    assert!(
        matches!(events.last(), Some(RunEvent::Cancelled { index: 0 })),
        "{events:?}"
    );
    settle(&mut run.grids[0]);
    assert_eq!(run.grids[0].state(), &IngestState::Cancelled);
    select_one(&rt, &mut done);
}

#[test]
fn close_while_paused_returns_the_session_and_skips_later_statements() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_with(&rt, s, PAGED, &[], GridLimits::rows(1_000));
    wait_for(&mut run.grids[0], &IngestState::Paused);
    // Closing the tab drops the grids: the rows are freed and the paused statement wakes.
    run.grids.clear();
    let mut done = finish(&rt, &mut run);
    let events = drain(&mut run.events);
    assert!(
        events.iter().all(|e| !matches!(
            e,
            RunEvent::Started { index: 1 } | RunEvent::Finished { .. }
        )),
        "{events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(RunEvent::Cancelled { index: 0 })
    ));
    select_one(&rt, &mut done);
}

/// Probes the statements that are a plain `SELECT` from one unqualified table.
fn probe_all(statements: &[sqler_ui::query::BoundStatement]) -> Vec<Option<SchemaProbe>> {
    statements
        .iter()
        .map(|s| SchemaProbe::for_sql(Dialect::Sqlite, &s.sql))
        .collect()
}

#[test]
fn resolved_follows_an_eligible_finished_and_reads_the_runs_own_session() {
    let rt = runtime();
    let mut s = rt.block_on(session());
    rt.block_on(async {
        for sql in [
            "CREATE TABLE t (id INTEGER)",
            "CREATE TEMP TABLE t (id INTEGER)",
            "CREATE TABLE only_main (id INTEGER)",
        ] {
            let q = Query::new(sql);
            let mut stream = s.execute(&q).await.unwrap();
            collect(&mut *stream).await.unwrap();
        }
    });
    let mut run = start_probed(
        &rt,
        s,
        "select * from t; select 1; select * from only_main; select * from main.only_main",
        &[],
        GridLimits::default(),
        probe_all,
    );
    rt.block_on(&mut run.handle).unwrap();
    let events = drain(&mut run.events);
    let resolved: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Resolved { index, schema } => Some((*index, schema.clone())),
            _ => None,
        })
        .collect();
    // The temp table shadows `main.t`; `select 1` and the qualified name have no probe.
    assert_eq!(
        resolved,
        [(0, Some("temp".to_owned())), (2, Some("main".to_owned()))]
    );
    // Each answer comes right after its own statement's Finished.
    let order: Vec<_> = events
        .iter()
        .map(|e| match e {
            RunEvent::Started { index } => ('s', *index),
            RunEvent::Finished { index, .. } => ('f', *index),
            RunEvent::Resolved { index, .. } => ('r', *index),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        order,
        [
            ('s', 0),
            ('f', 0),
            ('r', 0),
            ('s', 1),
            ('f', 1),
            ('s', 2),
            ('f', 2),
            ('r', 2),
            ('s', 3),
            ('f', 3),
        ]
    );
}

#[test]
fn no_probes_means_no_resolved() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start(&rt, s, "select 1; select 2", &[]);
    rt.block_on(&mut run.handle).unwrap();
    assert!(
        drain(&mut run.events)
            .iter()
            .all(|e| !matches!(e, RunEvent::Resolved { .. }))
    );
}

#[test]
fn a_failed_probe_resolves_to_none() {
    let rt = runtime();
    let s = rt.block_on(session());
    let mut run = start_probed(
        &rt,
        s,
        "create table t (id integer); select * from t",
        &[],
        GridLimits::default(),
        // A `PostgreSQL` lookup on `SQLite` fails to prepare.
        |statements| {
            statements
                .iter()
                .map(|s| SchemaProbe::for_sql(Dialect::Postgres, &s.sql))
                .collect()
        },
    );
    rt.block_on(&mut run.handle).unwrap();
    let events = drain(&mut run.events);
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::Resolved {
            index: 1,
            schema: None
        }
    )));
}

#[test]
fn nothing_is_resolved_after_a_cancel_or_a_failure() {
    let rt = runtime();
    // Cancelled before the first statement.
    let s = rt.block_on(session());
    let buf = BufferSnapshot::at_cursor("select * from sqlite_master", 0);
    let plan = plan(D, &buf, RunMode::All).unwrap();
    let statements = bind(D, &plan, &[]).unwrap();
    let (_grids, sinks): (Vec<_>, Vec<_>) = statements
        .iter()
        .map(|_| grid_channel_with(GridLimits::default()))
        .unzip();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let stop = RunStop::new(&sinks);
    stop.cancel();
    let probes = probe_all(&statements);
    assert!(probes[0].is_some());
    let job = QueryJob {
        tab: tab_id(),
        run: RunId::new(4),
        session: s,
        statements,
        sinks,
        stop,
        events: tx,
        probes,
    };
    rt.block_on(job.run());
    let events = drain(&mut events);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], RunEvent::Cancelled { index: 0 }));

    // A statement that fails is not followed by a lookup.
    let s = rt.block_on(session());
    let mut run = start_probed(
        &rt,
        s,
        "select * from missing_table",
        &[],
        GridLimits::default(),
        probe_all,
    );
    rt.block_on(&mut run.handle).unwrap();
    let events = drain(&mut run.events);
    assert!(matches!(events.last(), Some(RunEvent::Failed { .. })));
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, RunEvent::Resolved { .. }))
    );
}
