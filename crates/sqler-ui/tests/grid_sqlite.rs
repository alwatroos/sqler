// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Streams a `SQLite` result into the grid model while the main thread polls it.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_api::{DriverError, DriverRegistry};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_ui::grid::{
    CellRef, GridLimits, GridModel, GridSink, IngestState, IngestSummary, grid_channel,
    grid_channel_with, ingest,
};

const SQL: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 5000) \
                   SELECT x, 'r' || x FROM c";

type Worker = JoinHandle<Result<IngestSummary, DriverError>>;

/// Runs `SQL` on a `SQLite` session and ingests it into `sink`, on its own thread and runtime.
fn spawn_worker(sink: GridSink) -> Worker {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut registry = DriverRegistry::new();
                registry.register(Box::new(SqliteDriver::new())).unwrap();
                let config =
                    ConnectionConfig::new("t", DriverId::new(DRIVER_ID).unwrap(), ":memory:");
                let mut session = registry.connect(&config).await.unwrap();
                let query = Query::new(SQL);
                let mut stream = session.execute(&query).await.unwrap();
                ingest(&mut *stream, sink).await
            })
    })
}

/// Polls the model until it reaches `want`.
fn wait_for(model: &mut GridModel, want: &IngestState) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while model.state() != want {
        assert!(
            Instant::now() < deadline,
            "stuck in {:?}, wanted {want:?}",
            model.state()
        );
        model.poll(usize::MAX);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Waits until `worker` has finished (it must, within the deadline).
fn join(worker: Worker) -> Result<IngestSummary, DriverError> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "worker did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.join().unwrap()
}

#[test]
fn sqlite_rows_reach_the_grid() {
    let (mut model, sink) = grid_channel();
    let worker = spawn_worker(sink);

    let deadline = Instant::now() + Duration::from_secs(10);
    while matches!(model.state(), IngestState::Waiting | IngestState::Streaming) {
        assert!(Instant::now() < deadline, "ingestion timed out");
        model.poll(usize::MAX);
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(worker.join().unwrap().unwrap().rows, 5000);
    assert!(matches!(model.state(), IngestState::Done { .. }));
    assert_eq!(model.row_count(), 5000);
    assert_eq!(model.columns().len(), 2);
    let view = model.viewport(4990, 50);
    assert_eq!(view.len(), 10);
    assert_eq!(view.get(0).unwrap().get(0), Some(CellRef::Int(4991)));
    assert_eq!(
        view.get(9).unwrap().get(1).unwrap().to_value(),
        Value::Text("r5000".to_owned())
    );
}

#[test]
fn sqlite_pauses_at_the_page_and_fetch_more_continues() {
    let (mut model, sink) = grid_channel_with(GridLimits::rows(1_000));
    let worker = spawn_worker(sink);

    wait_for(&mut model, &IngestState::Paused);
    assert_eq!(model.row_count(), 1_000);
    // The worker is still holding the stream and does not read on.
    std::thread::sleep(Duration::from_millis(100));
    model.poll(usize::MAX);
    assert_eq!(model.row_count(), 1_000);
    assert!(!worker.is_finished());

    assert!(model.fetch_more());
    wait_for(&mut model, &IngestState::Paused);
    assert_eq!(model.row_count(), 2_000);
    assert_eq!(model.row(1_999).unwrap().get(0), Some(CellRef::Int(2_000)));

    // Fetch until the result ends; 5,000 rows is exactly five pages and ends as `Done`.
    while matches!(model.state(), IngestState::Paused) {
        assert!(model.fetch_more());
        let deadline = Instant::now() + Duration::from_secs(20);
        while model.state() == &IngestState::Streaming {
            assert!(Instant::now() < deadline, "ingestion timed out");
            model.poll(usize::MAX);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(matches!(model.state(), IngestState::Done { .. }));
    assert_eq!(model.row_count(), 5_000);
    assert_eq!(join(worker).unwrap().rows, 5_000);
}

#[test]
fn sqlite_cancel_while_paused_ends_the_worker() {
    let (mut model, sink) = grid_channel_with(GridLimits::rows(1_000));
    let canceller = sink.canceller();
    let worker = spawn_worker(sink);
    wait_for(&mut model, &IngestState::Paused);
    canceller.cancel();
    assert!(matches!(join(worker), Err(DriverError::Cancelled)));
    model.poll(usize::MAX);
    assert_eq!(model.state(), &IngestState::Cancelled);
    assert_eq!(model.row_count(), 1_000);

    // Cancelling through the model works the same way.
    let (mut model, sink) = grid_channel_with(GridLimits::rows(1_000));
    let worker = spawn_worker(sink);
    wait_for(&mut model, &IngestState::Paused);
    model.cancel();
    assert!(matches!(join(worker), Err(DriverError::Cancelled)));
}

#[test]
fn sqlite_close_while_paused_ends_the_worker() {
    let (mut model, sink) = grid_channel_with(GridLimits::rows(1_000));
    let worker = spawn_worker(sink);
    wait_for(&mut model, &IngestState::Paused);
    // Closing the tab drops the model, which frees the rows and wakes the worker.
    drop(model);
    assert!(matches!(join(worker), Err(DriverError::Cancelled)));
}
