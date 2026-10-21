// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of copy-as on a 100,000-row x 10-column `GridModel` (1,000,000 cells).
//!
//! Std-only (`harness = false`). Prints `key=value` lines:
//! - `gather_slice_p50_ns` / `gather_slice_p95_ns`: one `CopyJob::gather` call over
//!   `GATHER_SLICE_CELLS` cells, the work done on the UI thread per turn;
//! - `render_ms_tsv` / `render_ms_insert`: `CopyJob::render` of the whole selection, the work done
//!   on the background executor.
//!
//! Exit codes: 0 ok, 1 the slice p95 is over `--budget-slice-ms` (default 8), 2 bad arguments or
//! a failed copy.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_ui::grid::{
    ColumnLayout, CopyFormat, CopyJob, CopyLimits, DisplayMap, GATHER_SLICE_CELLS, GridLimits,
    GridModel, IdentityRows, IngestState, InsertTarget, Selection, SyntheticSource,
    grid_channel_with, spawn_ingest,
};

struct Args {
    rows: usize,
    columns: usize,
    reps: usize,
    budget_slice_ms: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 100_000,
        columns: 10,
        reps: 3,
        budget_slice_ms: 8,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let mut num = || {
            let s = value()?;
            s.parse::<u64>()
                .map_err(|_| format!("bad number for {flag}: {s}"))
        };
        match flag.as_str() {
            "--rows" => a.rows = usize::try_from(num()?).unwrap_or(usize::MAX),
            "--columns" => a.columns = usize::try_from(num()?).unwrap_or(usize::MAX),
            "--reps" => a.reps = usize::try_from(num()?).unwrap_or(usize::MAX),
            "--budget-slice-ms" => a.budget_slice_ms = num()?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.rows == 0 || a.columns == 0 || a.reps == 0 {
        return Err("need rows > 0, columns > 0 and reps > 0".to_owned());
    }
    Ok(a)
}

fn build_model(a: &Args) -> Result<GridModel, String> {
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
    let handle = spawn_ingest(SyntheticSource::new(a.rows, a.columns, 1024), sink)
        .map_err(|e| format!("cannot spawn ingestion thread: {e}"))?;
    loop {
        match model.state() {
            IngestState::Done { .. } => break,
            IngestState::Waiting | IngestState::Streaming => {}
            other => return Err(format!("ingestion ended in state {other:?}")),
        }
        if model.poll(usize::MAX) == 0 {
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    if handle.join().is_err() {
        return Err("ingestion thread panicked".to_owned());
    }
    Ok(model)
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u128], pct: usize) -> u128 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn job(model: &GridModel, format: CopyFormat) -> Result<CopyJob, String> {
    let layout = ColumnLayout::new(vec![100.0; model.columns().len()]);
    let rows = IdentityRows(model.row_count());
    let map = DisplayMap {
        rows: &rows,
        columns: &layout,
    };
    let mut selection = Selection::default();
    selection.select_all();
    let limits = CopyLimits {
        max_cells: usize::MAX,
        max_bytes: usize::MAX,
    };
    CopyJob::new(
        model,
        &map,
        &selection,
        format,
        InsertTarget::placeholder(None),
        limits,
    )
    .map_err(|e| e.to_string())
}

fn run(a: &Args) -> Result<bool, String> {
    let model = build_model(a)?;
    let mut slices: Vec<u128> = Vec::new();
    for _ in 0..a.reps {
        let mut j = job(&model, CopyFormat::Tsv)?;
        loop {
            let start = Instant::now();
            let done = j.gather(&model, GATHER_SLICE_CELLS);
            slices.push(start.elapsed().as_nanos());
            if done {
                break;
            }
        }
    }
    slices.sort_unstable();
    let (p50, p95) = (percentile(&slices, 50), percentile(&slices, 95));
    println!("rows={}", a.rows);
    println!("columns={}", a.columns);
    println!("slice_cells={GATHER_SLICE_CELLS}");
    println!("slices={}", slices.len());
    println!("gather_slice_p50_ns={p50}");
    println!("gather_slice_p95_ns={p95}");
    for (name, format) in [("tsv", CopyFormat::Tsv), ("insert", CopyFormat::Insert)] {
        let mut j = job(&model, format)?;
        j.gather(&model, usize::MAX);
        let start = Instant::now();
        let out = j.render().map_err(|e| e.to_string())?;
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("render_ms_{name}={ms:.1}");
        println!("render_bytes_{name}={}", out.text.len());
    }
    let budget_ns = u128::from(a.budget_slice_ms) * 1_000_000;
    let ok = p95 <= budget_ns;
    if !ok {
        eprintln!(
            "FAIL: gather slice p95 {p95} ns is over the budget of {budget_ns} ns; lower GATHER_SLICE_CELLS"
        );
    }
    Ok(ok)
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("grid_copy: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("grid_copy: {e}");
            ExitCode::from(2)
        }
    }
}
