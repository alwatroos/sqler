// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of viewport fetches from a 1M-row `GridModel`.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! against a saved baseline: `p95_ns` (minimum over `--reps` repetitions, plus an absolute
//! `--slack-ns`) and, when the baseline file has the key, `cells_p95_ns` (the same rule with
//! `--cells-slack-ns`). Exit codes: 0 ok, 1 budget or regression failure, 2 bad arguments or
//! unreadable baseline. See `docs/src/benchmarks.md` for the method and the rule.
//!
//! `p50_ns` and `p95_ns` time the viewport fetch plus the construction of a `RowRef` for each
//! visible row; they read no cell and no per-row memory. `cells_p50_ns` and `cells_p95_ns` also
//! read every cell of those rows: the value of inline cells, and the length plus the first and
//! last byte of text, JSON and binary cells, so the walk touches the arenas.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_ui::grid::{
    CellPos, CellRef, ColumnLayout, DisplayMap, GridLimits, GridModel, IdentityRows, IngestState,
    Selection, SyntheticSource, grid_channel_with, spawn_ingest,
};

struct Args {
    rows: usize,
    columns: usize,
    batch: usize,
    viewport: usize,
    samples: usize,
    reps: usize,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    cells_slack_ns: u64,
    budget_p95_ns: u64,
    layout_rows: usize,
    layout_columns: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 1_000_000,
        columns: 6,
        batch: 1024,
        viewport: 50,
        samples: 100_000,
        reps: 3,
        save: None,
        compare: None,
        threshold_pct: 25,
        slack_ns: 100,
        cells_slack_ns: 500,
        budget_p95_ns: 1_000_000,
        layout_rows: 100_000,
        layout_columns: 50,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let num = |s: String| {
            s.parse::<u64>()
                .map_err(|_| format!("bad number for {flag}: {s}"))
        };
        match flag.as_str() {
            "--rows" => a.rows = to_usize(num(value()?)?),
            "--columns" => a.columns = to_usize(num(value()?)?),
            "--batch" => a.batch = to_usize(num(value()?)?),
            "--viewport" => a.viewport = to_usize(num(value()?)?),
            "--samples" => a.samples = to_usize(num(value()?)?),
            "--reps" => a.reps = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--cells-slack-ns" => a.cells_slack_ns = num(value()?)?,
            "--budget-p95-ns" => a.budget_p95_ns = num(value()?)?,
            "--layout-rows" => a.layout_rows = to_usize(num(value()?)?),
            "--layout-columns" => a.layout_columns = to_usize(num(value()?)?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.reps == 0 || a.rows == 0 || a.viewport > a.rows {
        return Err("need samples > 0, reps > 0, rows > 0 and viewport <= rows".to_owned());
    }
    if a.layout_columns < 2 || a.viewport > a.layout_rows {
        return Err("need layout-columns >= 2 and viewport <= layout-rows".to_owned());
    }
    Ok(a)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// Resident set size in bytes on Linux; `None` elsewhere.
fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Reads the payload of a cell so the walk reaches the arenas: the length plus the first and last
/// byte of text, JSON and binary cells, the value itself for the inline kinds.
fn touch(cell: CellRef<'_>) -> u64 {
    let ends = |b: &[u8]| {
        let first = b.first().copied().map_or(0, u64::from);
        let last = b.last().copied().map_or(0, u64::from);
        (b.len() as u64).wrapping_add(first).wrapping_add(last)
    };
    match cell {
        CellRef::Null => 0,
        CellRef::Bool(v) => u64::from(v),
        CellRef::Int(v) => v.cast_unsigned(),
        CellRef::Float(v) => v.to_bits(),
        CellRef::Text(t) | CellRef::Json(t) => ends(t.as_bytes()),
        CellRef::Bytes(b) => ends(b),
        _ => 1,
    }
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn build_model(a: &Args) -> Result<(GridModel, Duration), String> {
    // The bench measures a whole 1M-row result, so it is not paged.
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
    let start = Instant::now();
    let handle = spawn_ingest(SyntheticSource::new(a.rows, a.columns, a.batch), sink)
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
    let elapsed = start.elapsed();
    if handle.join().is_err() {
        return Err("ingestion thread panicked".to_owned());
    }
    Ok((model, elapsed))
}

/// A second model for the layout measurement, built like [`build_model`].
fn build_layout_model(a: &Args) -> Result<GridModel, String> {
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
    let source = SyntheticSource::new(a.layout_rows, a.layout_columns, a.batch);
    let handle =
        spawn_ingest(source, sink).map_err(|e| format!("cannot spawn ingestion thread: {e}"))?;
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

/// A reversed column order with model columns 0 and 1 pinned.
fn reversed_pinned_layout(columns: usize) -> ColumnLayout {
    let mut layout = ColumnLayout::new(vec![120.0; columns]);
    for target in 0..columns - 1 {
        layout.move_display(columns - 1, target);
    }
    layout.pin(0);
    layout.pin(1);
    layout
}

/// Minimum over `reps` of the p50 and p95 of building one viewport and visiting every display
/// column of its rows through the layout and a 10x5 selection.
fn measure_layout(a: &Args, model: &GridModel) -> (u64, u64) {
    let layout = reversed_pinned_layout(a.layout_columns);
    let order = IdentityRows(a.layout_rows);
    let map = DisplayMap {
        rows: &order,
        columns: &layout,
    };
    let mut selection = Selection::default();
    selection.click(CellPos { row: 0, col: 0 }, false);
    selection.click(CellPos { row: 9, col: 4 }, true);
    let span = (a.layout_rows - a.viewport + 1) as u64;
    let mut rng = 0x2545_F491_4F6C_DD1D_u64;
    let (mut p50, mut p95) = (u64::MAX, u64::MAX);
    for _ in 0..a.reps {
        let mut timings = Vec::with_capacity(a.samples);
        for _ in 0..a.samples {
            let first = to_usize(xorshift(&mut rng) % span);
            let start = Instant::now();
            let view = model.viewport(first, a.viewport);
            for (offset, row) in view.iter().enumerate() {
                for display in 0..layout.len() {
                    if let Some(model_col) = layout.model_column(display) {
                        black_box(row.get(model_col));
                        black_box(selection.contains(first + offset, display, &map));
                    }
                }
            }
            let ns = start.elapsed().as_nanos();
            timings.push(u64::try_from(ns).unwrap_or(u64::MAX));
        }
        timings.sort_unstable();
        p50 = p50.min(percentile(&timings, 50));
        p95 = p95.min(percentile(&timings, 95));
    }
    (p50, p95)
}

/// The gated keys of a saved report. `cells_p95_ns` is absent from reports of earlier versions.
struct Baseline {
    p95: u64,
    cells_p95: Option<u64>,
}

fn read_baseline(path: &str) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let key = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse().ok())
    };
    Ok(Baseline {
        p95: key("p95_ns=").ok_or_else(|| format!("no p95_ns in {path}"))?,
        cells_p95: key("cells_p95_ns="),
    })
}

/// The limit a measurement may reach: the baseline plus `pct` percent plus an absolute slack.
fn limit_of(base: u64, pct: u64, slack: u64) -> u128 {
    u128::from(base) * (u128::from(pct) + 100) / 100 + u128::from(slack)
}

/// Compares the measurements with the baseline, printing each failure. `cells_p95` is compared
/// only when the baseline has the key.
fn within_baseline(a: &Args, base: &Baseline, p95: u64, cells_p95: u64) -> bool {
    let mut ok = true;
    let limit = limit_of(base.p95, a.threshold_pct, a.slack_ns);
    if u128::from(p95) > limit {
        eprintln!(
            "FAIL: p95 {p95} ns regressed against baseline {} ns (limit {limit} ns)",
            base.p95
        );
        ok = false;
    }
    if let Some(cells_base) = base.cells_p95 {
        let limit = limit_of(cells_base, a.threshold_pct, a.cells_slack_ns);
        if u128::from(cells_p95) > limit {
            eprintln!(
                "FAIL: cells_p95 {cells_p95} ns regressed against baseline {cells_base} ns (limit {limit} ns)"
            );
            ok = false;
        }
    } else {
        eprintln!("note: the baseline has no cells_p95_ns; cell reads are not compared");
    }
    ok
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(read_baseline(path).map_err(|e| (2, e))?),
        None => None,
    };

    let rss_before = rss_bytes();
    let (model, ingest_time) = build_model(&a).map_err(|e| (1, e))?;
    let rss_after = rss_bytes();

    let span = (a.rows - a.viewport + 1) as u64;
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    // The gate uses the minimum over the repetitions: noise on a shared runner only adds time, so
    // the fastest repetition is the closest to the real cost.
    let (mut p50, mut p95) = (u64::MAX, u64::MAX);
    let (mut cells_p50, mut cells_p95) = (u64::MAX, u64::MAX);
    for _ in 0..a.reps {
        // Gated: the viewport fetch and a `RowRef` for each visible row. It reads no cell and no
        // per-row memory (earlier versions read each row's `Vec` header), so it does less work
        // than the walk of the base; the cell reads are timed and gated as `cells_*` below.
        let mut timings = Vec::with_capacity(a.samples);
        for _ in 0..a.samples {
            let first = to_usize(xorshift(&mut rng) % span);
            let start = Instant::now();
            let view = model.viewport(first, a.viewport);
            for row in view.iter() {
                black_box(row.iter().size_hint());
            }
            let ns = start.elapsed().as_nanos();
            timings.push(u64::try_from(ns).unwrap_or(u64::MAX));
        }
        timings.sort_unstable();
        p50 = p50.min(percentile(&timings, 50));
        p95 = p95.min(percentile(&timings, 95));

        // The same fetch, reading every cell of the visible rows. Random positions in a large
        // model make this a cold-memory read. Gated against a baseline that has the key.
        timings.clear();
        for _ in 0..a.samples {
            let first = to_usize(xorshift(&mut rng) % span);
            let start = Instant::now();
            let view = model.viewport(first, a.viewport);
            for row in view.iter() {
                for cell in row.iter() {
                    black_box(touch(cell));
                }
            }
            let ns = start.elapsed().as_nanos();
            timings.push(u64::try_from(ns).unwrap_or(u64::MAX));
        }
        timings.sort_unstable();
        cells_p50 = cells_p50.min(percentile(&timings, 50));
        cells_p95 = cells_p95.min(percentile(&timings, 95));
    }

    let rss_delta = match (rss_before, rss_after) {
        (Some(b), Some(e)) => e.saturating_sub(b).to_string(),
        _ => "n/a".to_owned(),
    };
    let layout_model = build_layout_model(&a).map_err(|e| (1, e))?;
    let (layout_p50, layout_p95) = measure_layout(&a, &layout_model);
    let report = format!(
        "bench=grid_viewport\nos={}-{}\nrows={}\ncolumns={}\nviewport_rows={}\nsamples={}\nreps={}\n\
         p50_ns={p50}\np95_ns={p95}\ncells_p50_ns={cells_p50}\ncells_p95_ns={cells_p95}\n\
         ingest_ms={}\nheap_bytes={}\nrss_delta_bytes={rss_delta}\n\
         layout_rows={}\nlayout_columns={}\nlayout_pinned=2\nlayout_p50_ns={layout_p50}\n\
         layout_p95_ns={layout_p95}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.rows,
        a.columns,
        a.viewport,
        a.samples,
        a.reps,
        ingest_time.as_millis(),
        model.heap_bytes(),
        a.layout_rows,
        a.layout_columns,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut ok = true;
    if p95 > a.budget_p95_ns {
        eprintln!(
            "FAIL: p95 {p95} ns exceeds the budget of {} ns",
            a.budget_p95_ns
        );
        ok = false;
    }
    if let Some(base) = baseline {
        ok &= within_baseline(&a, &base, p95, cells_p95);
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(code)
        }
    }
}
