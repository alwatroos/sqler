// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Streaming: 1,000,000 synthetic rows go through every built-in format with a batch buffer of
//! 1,000 rows and a sink that only counts. No `Vec` of all rows exists, and the output grows
//! while the rows are written, so nothing is accumulated until the end.

// Helpers of an integration test are outside the `allow-unwrap-in-tests` scope of clippy.toml.
#![allow(clippy::unwrap_used)]

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sqler_core::{Column, DataType, Date, Row, Time, Timestamp, Value};
use sqler_export::{ExportControl, ExportOptions, Exporter, Registry};

const ROWS: usize = 1_000_000;
const BATCH: usize = 1_000;

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

fn stream(id: &str) {
    let registry = Registry::with_builtin();
    let format = registry.get(id).unwrap();
    let mut options = ExportOptions::default();
    "t".clone_into(&mut options.sql.table);
    let count = Arc::new(AtomicU64::new(0));
    let sink = Box::new(CountingSink(Arc::clone(&count)));
    let mut exporter = Exporter::new(format, &options, sink, ExportControl::default()).unwrap();
    exporter.begin(&columns()).unwrap();

    let mut buffer: Vec<Row> = (0..BATCH).map(|_| Row(Vec::new())).collect();
    let start = Instant::now();
    let mut last = 0;
    for batch in 0..ROWS / BATCH {
        for (i, row) in buffer.iter_mut().enumerate() {
            fill(row, batch * BATCH + i);
        }
        exporter.write_batch(&buffer).unwrap();
        if (batch + 1) % 10 == 0 {
            let now = count.load(Ordering::Relaxed);
            assert!(now > last, "{id}: no output after batch {batch}");
            last = now;
        }
    }
    let summary = exporter.finish().unwrap();
    let secs = start.elapsed().as_secs_f64();
    assert_eq!(summary.rows, ROWS as u64);
    assert_eq!(summary.bytes, count.load(Ordering::Relaxed));
    #[allow(clippy::cast_precision_loss)]
    let rate = ROWS as f64 / secs;
    println!("{id}: rows_per_sec={rate:.0} bytes={}", summary.bytes);
}

#[test]
fn csv_streams() {
    stream("csv");
}

#[test]
fn json_streams() {
    stream("json");
}

#[test]
fn sql_streams() {
    stream("sql");
}

#[test]
fn markdown_streams() {
    stream("markdown");
}
