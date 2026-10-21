// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Memory proof for the binary formats: 1,000,000 rows through `Parquet` and `XLSX` while the
//! resident set stays flat. This file is its own test binary with a single `#[test]`, so no other
//! test thread allocates in the process while RSS is sampled.
//!
//! The resident size after batch 1,000 (1M rows) must be at most 12 MiB above the one after
//! batch 100. A writer that kept rows, or the whole output, would grow by hundreds of MiB.

// Helpers of an integration test are outside the `allow-unwrap-in-tests` scope of clippy.toml.
#![allow(clippy::unwrap_used)]
#![cfg(any(feature = "parquet", feature = "xlsx"))]

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqler_core::{Column, DataType, Date, Row, Time, Timestamp, Value};
use sqler_export::{ExportControl, ExportOptions, Exporter, Registry};

const BATCH: usize = 1_000;
const BATCHES: usize = 1_000;
const EARLY: usize = 100;
const MAX_GROWTH: usize = 12 * 1024 * 1024;

struct CountingSink(Arc<AtomicU64>);

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn columns() -> Vec<Column> {
    vec![
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
        Column::new("score", DataType::Float),
        Column::new("note", DataType::Text),
        Column::new("created", DataType::Timestamp),
    ]
}

fn fill(row: &mut Row, n: usize) {
    let day = u8::try_from(n % 28 + 1).unwrap();
    let hour = u8::try_from(n % 24).unwrap();
    let ts = Timestamp::new(
        Date::new(2024, 1, day).unwrap(),
        Time::new(hour, 30, 15, 0).unwrap(),
    );
    row.0.clear();
    row.0.push(Value::Int(i64::try_from(n).unwrap()));
    row.0
        .push(Value::Text(format!("name {n}, \"quoted\" | it's")));
    #[allow(clippy::cast_precision_loss)]
    row.0.push(Value::Float(n as f64 * 0.25));
    row.0.push(Value::Null);
    row.0.push(Value::Timestamp(ts));
}

fn rss() -> usize {
    memory_stats::memory_stats().unwrap().physical_mem
}

/// Streams 1M rows through `id`; returns (RSS after batch 100, RSS after batch 1,000, bytes).
fn stream(id: &str, interval: usize) -> (usize, usize, u64) {
    let registry = Registry::with_builtin();
    let format = registry.get(id).unwrap();
    let mut options = ExportOptions::default();
    "t".clone_into(&mut options.sql.table);
    let count = Arc::new(AtomicU64::new(0));
    let sink = Box::new(CountingSink(Arc::clone(&count)));
    let mut exporter = Exporter::new(format, &options, sink, ExportControl::default()).unwrap();
    exporter.begin(&columns()).unwrap();

    let mut buffer: Vec<Row> = (0..BATCH).map(|_| Row(Vec::new())).collect();
    let (mut early, mut late, mut last) = (0, 0, 0);
    for batch in 0..BATCHES {
        for (i, row) in buffer.iter_mut().enumerate() {
            fill(row, batch * BATCH + i);
        }
        exporter.write_batch(&buffer).unwrap();
        let done = batch + 1;
        if done % interval == 0 {
            let now = count.load(Ordering::Relaxed);
            assert!(now > last, "{id}: the sink did not grow by batch {done}");
            last = now;
        }
        if done == EARLY {
            early = rss();
        }
        if done == BATCHES {
            late = rss();
        }
    }
    let summary = exporter.finish().unwrap();
    assert_eq!(summary.rows, (BATCH * BATCHES) as u64);
    (early, late, summary.bytes)
}

#[test]
fn binary_formats_stream_one_million_rows_in_flat_memory() {
    let mut formats: Vec<(&str, usize)> = Vec::new();
    // A row group is 65,536 rows, 66 batches; XLSX deflates every 64 KiB of sheet XML.
    #[cfg(feature = "parquet")]
    formats.push(("parquet", 66));
    #[cfg(feature = "xlsx")]
    formats.push(("xlsx", 10));
    for (id, interval) in formats {
        let (early, late, bytes) = stream(id, interval);
        let growth = late.saturating_sub(early);
        println!("{id}: rss_after_100k={early} rss_after_1m={late} growth={growth} bytes={bytes}");
        assert!(
            growth <= MAX_GROWTH,
            "{id}: RSS grew by {growth} bytes between batch {EARLY} and batch {BATCHES}"
        );
    }
}
