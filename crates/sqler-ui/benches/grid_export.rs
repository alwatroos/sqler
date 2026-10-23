// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the export pipeline: rows per second from a source into a file.
//!
//! Std-only (`harness = false`). Options: `--rows N` (default 1,000,000), `--format ID` (default
//! `csv`; any id the build knows), `--source loaded|fresh` (default `loaded`). `loaded` fills a
//! `GridModel` from a `SyntheticSource` first (not timed) and times the copy out of the model plus
//! the write. `fresh` times a recursive-CTE run on `SQLite` through `fresh_rows` plus the write.
//! Prints `<source>_<id>_rows_per_sec=<n>` and the row and byte counts. Cargo's `--bench` flag is
//! ignored. Exit codes: 0 ok, 1 the export failed, 2 bad arguments.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use sqler_config::ConnectionSpec;
use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Query};
use sqler_driver_api::{BoxFuture, DriverRegistry, Session};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};
use sqler_export::{ExportOptions, Registry};
use sqler_ui::browser::{ConnectFailure, ConnectRequest, Connector, HostKeyPrompt, SecretSource};
use sqler_ui::export::{
    ExportOutcome, ExportStop, FEED_CAPACITY, Feed, LoadedCursor, LoadedRows, WriteSpec,
    fresh_rows, write_feed,
};
use sqler_ui::grid::{
    GridLimits, GridModel, IngestState, SyntheticSource, grid_channel_with, spawn_ingest,
};
use tokio::sync::mpsc;

/// Rows per message from the loaded rows, as the app copies them.
const COPY_BATCH: usize = 2048;

struct Args {
    rows: usize,
    format: String,
    fresh: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 1_000_000,
        format: "csv".to_owned(),
        fresh: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--rows" => {
                a.rows = value()?
                    .parse()
                    .map_err(|_| "--rows needs a number".to_owned())?;
            }
            "--format" => a.format = value()?,
            "--source" => {
                a.fresh = match value()?.as_str() {
                    "loaded" => false,
                    "fresh" => true,
                    other => return Err(format!("unknown source: {other}")),
                };
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(a)
}

/// The smallest `Connector` over the `SQLite` driver: no secrets, no tunnel.
struct SqliteConnector(Arc<DriverRegistry>);

impl Connector for SqliteConnector {
    fn driver_ids(&self) -> Vec<DriverId> {
        self.0.ids().cloned().collect()
    }

    fn connect(
        &self,
        request: ConnectRequest,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, ConnectFailure>> {
        let registry = Arc::clone(&self.0);
        let config = ConnectionConfig::new(
            request.spec.name.clone(),
            request.spec.driver.clone(),
            request.spec.target.clone(),
        );
        Box::pin(async move {
            registry
                .connect(&config)
                .await
                .map_err(ConnectFailure::Driver)
        })
    }

    fn trust_host_key(
        &self,
        _key: HostKeyPrompt,
    ) -> BoxFuture<'static, Result<(), ConnectFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

fn fill_model(rows: usize) -> Result<GridModel, String> {
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
    let handle = spawn_ingest(SyntheticSource::new(rows, 6, 1024), sink)
        .map_err(|e| format!("cannot spawn ingestion thread: {e}"))?;
    loop {
        match model.state() {
            IngestState::Done { .. } => break,
            IngestState::Waiting | IngestState::Streaming => {}
            other => return Err(format!("ingestion ended in state {other:?}")),
        }
        if model.poll(usize::MAX) == 0 {
            thread::sleep(Duration::from_micros(50));
        }
    }
    if handle.join().is_err() {
        return Err("ingestion thread panicked".to_owned());
    }
    Ok(model)
}

fn scratch_file(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("sqler-grid-export-{}-{tag}", std::process::id()))
}

fn run(
    a: &Args,
    extension: &str,
    options: ExportOptions,
) -> Result<(ExportOutcome, Duration), String> {
    let path = scratch_file(&format!("out.{extension}"));
    let spec = WriteSpec {
        format_id: a.format.clone(),
        options,
        formats: Arc::new(Registry::with_builtin()),
        path,
        job: 0,
    };
    let stop = ExportStop::new();
    let (tx, rx) = mpsc::channel::<Feed>(FEED_CAPACITY);
    if a.fresh {
        let mut registry = DriverRegistry::new();
        registry
            .register(Box::new(SqliteDriver::new()))
            .map_err(|e| e.to_string())?;
        let connector: Arc<dyn Connector> = Arc::new(SqliteConnector(Arc::new(registry)));
        let request = ConnectRequest {
            spec: ConnectionSpec::new(
                "bench",
                DriverId::new(DRIVER_ID).map_err(|e| e.to_string())?,
                ":memory:",
            ),
            db_password: SecretSource::None,
            ssh_secret: SecretSource::None,
        };
        let sql = format!(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < {}) \
             SELECT i AS c0, 'r' || i AS c1, i + 0.5 AS c2, i % 2 AS c3, i * 4 AS c4, 'x' || i AS c5 FROM n",
            a.rows
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        let writer = {
            let stop = stop.clone();
            thread::spawn(move || write_feed(rx, spec, &stop, Box::new(|_| {})))
        };
        runtime.block_on(fresh_rows(connector, request, Query::new(sql), tx, stop));
        let outcome = writer.join().map_err(|_| "writer panicked".to_owned())?;
        Ok((outcome, started.elapsed()))
    } else {
        let model = fill_model(a.rows)?;
        let started = Instant::now();
        let writer = {
            let stop = stop.clone();
            thread::spawn(move || write_feed(rx, spec, &stop, Box::new(|_| {})))
        };
        let mut cursor = LoadedCursor::new(LoadedRows::All {
            rows: model.row_count(),
        });
        while let Some(feed) = cursor.next_feed(&model, COPY_BATCH) {
            if tx.blocking_send(feed).is_err() {
                break;
            }
        }
        drop(tx);
        let outcome = writer.join().map_err(|_| "writer panicked".to_owned())?;
        Ok((outcome, started.elapsed()))
    }
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("grid_export: {msg}");
            return ExitCode::from(2);
        }
    };
    let registry = Registry::with_builtin();
    let Some(format) = registry.get(&a.format) else {
        eprintln!("grid_export: unknown format {}", a.format);
        return ExitCode::from(2);
    };
    let mut options = ExportOptions::default();
    // The SQL format needs a table.
    "bench".clone_into(&mut options.sql.table);
    options.sql.dialect = sqler_sql::Dialect::Sqlite;
    let extension = format.extension();
    let source = if a.fresh { "fresh" } else { "loaded" };
    let (outcome, elapsed) = match run(&a, extension, options) {
        Ok(done) => done,
        Err(msg) => {
            eprintln!("grid_export: {msg}");
            return ExitCode::from(1);
        }
    };
    let path = scratch_file(&format!("out.{extension}"));
    let _ = std::fs::remove_file(&path);
    match outcome {
        Ok(summary) => {
            #[allow(clippy::cast_precision_loss)] // figures for people
            let per_sec = summary.rows as f64 / elapsed.as_secs_f64();
            println!("rows={}", summary.rows);
            println!("bytes={}", summary.bytes);
            println!("elapsed_ms={}", elapsed.as_millis());
            println!("{source}_{}_rows_per_sec={per_sec:.0}", a.format);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("grid_export: export failed: {failure}");
            ExitCode::from(1)
        }
    }
}
