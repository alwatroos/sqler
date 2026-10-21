// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Throughput of the writers on synthetic rows.
//!
//! Std-only (`harness = false`). Streams `--rows N` (default 1,000,000) rows of five columns
//! (integer, text, float, null, timestamp) in batches of `--batch N` (1,000) into a sink that
//! discards the bytes, for every format of `Registry::with_builtin()` (so only the formats the enabled features compile), and prints `<id>_rows_per_sec=` and
//! `<id>_mib_per_sec=` lines. `--format ID` runs one format only (for a per-format peak RSS). A `--bench` argument (added by `cargo bench`) is ignored. Exit
//! codes: 0 ok, 1 writer error, 2 bad arguments.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;

use sqler_core::{Column, DataType, Date, Row, Time, Timestamp, Value};
use sqler_export::{ExportControl, ExportOptions, Exporter, Format, Registry};

/// Discards everything.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
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

/// Fills `row` with the synthetic values of row number `n`.
fn fill(row: &mut Row, n: i64) {
    let day = u8::try_from(n % 28 + 1).unwrap_or(1);
    let hour = u8::try_from(n % 24).unwrap_or(0);
    let ts = match (Date::new(2024, 1, day), Time::new(hour, 30, 15, 0)) {
        (Ok(d), Ok(t)) => Value::Timestamp(Timestamp::new(d, t)),
        _ => Value::Null,
    };
    row.0.clear();
    row.0.push(Value::Int(n));
    row.0.push(Value::Text(format!("name {n}, \"quoted\"")));
    #[allow(clippy::cast_precision_loss)]
    row.0.push(Value::Float(n as f64 * 0.25));
    row.0.push(Value::Null);
    row.0.push(ts);
}

fn run(id: &str, rows: i64, batch: usize) -> Result<(f64, f64), String> {
    let registry = Registry::with_builtin();
    let format = registry.get(id).ok_or("unknown format")?;
    let mut options = ExportOptions::default();
    "bench".clone_into(&mut options.sql.table);
    let mut exporter = Exporter::new(
        format,
        &options,
        Box::new(Discard),
        ExportControl::default(),
    )
    .map_err(|e| e.to_string())?;
    exporter.begin(&columns()).map_err(|e| e.to_string())?;
    let mut buffer: Vec<Row> = (0..batch).map(|_| Row(Vec::new())).collect();
    let start = Instant::now();
    let mut n = 0_i64;
    while n < rows {
        let len = usize::try_from((rows - n).min(i64::try_from(batch).unwrap_or(1))).unwrap_or(1);
        for row in &mut buffer[..len] {
            fill(row, n);
            n += 1;
        }
        exporter
            .write_batch(&buffer[..len])
            .map_err(|e| e.to_string())?;
    }
    let summary = exporter.finish().map_err(|e| e.to_string())?;
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    #[allow(clippy::cast_precision_loss)]
    let (rps, mibps) = (
        summary.rows as f64 / secs,
        summary.bytes as f64 / secs / (1024.0 * 1024.0),
    );
    Ok((rps, mibps))
}

fn main() -> ExitCode {
    let mut rows = 1_000_000_i64;
    let mut batch = 1_000_usize;
    let mut only: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => {}
            "--rows" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) => rows = v,
                None => return usage(),
            },
            "--format" => match args.next() {
                Some(v) => only = Some(v),
                None => return usage(),
            },
            "--batch" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) if v > 0 => batch = v,
                _ => return usage(),
            },
            _ => return usage(),
        }
    }
    let ids: Vec<&'static str> = Registry::with_builtin().formats().map(Format::id).collect();
    if let Some(wanted) = &only
        && !ids.contains(&wanted.as_str())
    {
        eprintln!("format {wanted:?} is not registered (enabled features: {ids:?})");
        return ExitCode::from(2);
    }
    for id in ids {
        if only.as_deref().is_some_and(|wanted| wanted != id) {
            continue;
        }
        match run(id, rows, batch) {
            Ok((rps, mibps)) => {
                println!("{id}_rows_per_sec={rps:.0}");
                println!("{id}_mib_per_sec={mibps:.1}");
            }
            Err(e) => {
                eprintln!("{id}: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!("usage: export_throughput [--rows N] [--batch N] [--format ID]");
    ExitCode::from(2)
}
