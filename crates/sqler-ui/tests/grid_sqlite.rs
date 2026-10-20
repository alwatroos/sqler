// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Streams a `SQLite` result into the grid model while the main thread polls it.

// Helpers outside `#[test]` fns also unwrap; clippy.toml only exempts the test fns themselves.
#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use sqler_driver_api::DriverRegistry;
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query, Value};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_ui::grid::{IngestState, grid_channel, ingest};

const SQL: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x < 5000) \
                   SELECT x, 'r' || x FROM c";

#[test]
fn sqlite_rows_reach_the_grid() {
    let (mut model, sink) = grid_channel();
    let worker = std::thread::spawn(move || {
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
    });

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
    assert_eq!(view.get(0).unwrap().0[0], Value::Int(4991));
    assert_eq!(view.get(9).unwrap().0[1], Value::Text("r5000".to_owned()));
}
