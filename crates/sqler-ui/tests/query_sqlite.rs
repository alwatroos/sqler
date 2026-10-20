// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plans, binds and runs scripts on a real in-memory `SQLite` session.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_api::{CancelFlag, DriverRegistry, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_sql::Dialect;
use sqler_ui::grid::{GridModel, IngestState, grid_channel};
use sqler_ui::query::{
    BufferSnapshot, ParamInput, ParamKey, ParamKind, QueryJob, QueryJobDone, RunEvent, RunId,
    RunMode, bind, plan,
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
    stop: CancelFlag,
    grids: Vec<GridModel>,
    events: UnboundedReceiver<RunEvent>,
    handle: tokio::task::JoinHandle<QueryJobDone>,
}

/// Plans `text` in run-all mode, binds `inputs` and spawns the job on `session`.
fn start(rt: &Runtime, session: Box<dyn Session>, text: &str, inputs: &[ParamInput]) -> Started {
    let buf = BufferSnapshot::at_cursor(text, 0);
    let plan = plan(D, &buf, RunMode::All).unwrap();
    let statements = bind(D, &plan, inputs).unwrap();
    let (grids, sinks): (Vec<_>, Vec<_>) = statements.iter().map(|_| grid_channel()).unzip();
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let stop = CancelFlag::new();
    let job = QueryJob {
        tab: tab_id(),
        run: RunId::new(1),
        session,
        statements,
        sinks,
        stop: stop.clone(),
        events: tx,
    };
    let handle = rt.spawn(job.run());
    Started {
        stop,
        grids,
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
    assert_eq!(grid.row(0).unwrap().0[0], Value::Int(2));
    assert_eq!(grid.row(1).unwrap().0[0], Value::Int(5));
    assert_eq!(grid.row(1).unwrap().0[1], Value::Text("x".to_owned()));
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
        run.stop.cancel();
        handle.cancel().await.unwrap();
        (&mut run.handle).await.unwrap()
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
    let (_grids, sinks): (Vec<_>, Vec<_>) = statements.iter().map(|_| grid_channel()).unzip();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let stop = CancelFlag::new();
    stop.cancel();
    let job = QueryJob {
        tab: tab_id(),
        run: RunId::new(3),
        session: s,
        statements,
        sinks,
        stop,
        events: tx,
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
    let row = &run.grids[0].row(0).unwrap().0;
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
    let row = &run.grids[0].row(0).unwrap().0;
    assert_eq!(row[0], Value::Int(10));
    assert_eq!(row[1], Value::Text("twelve".to_owned()));
    assert_eq!(row[2], Value::Int(10));
}
