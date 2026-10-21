// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of viewport fetches from a 1M-row `GridModel`.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! against a saved baseline: `p95_ns` (minimum over `--reps` repetitions, plus an absolute
//! `--slack-ns`) and, when the baseline file has the key, `cells_p95_ns` (the same rule with
//! `--cells-slack-ns`). With `--staged N` (default 0, off) it also stages N cell edits on the
//! 1M-row grid and times the display path with edits pending: `staged_p95_ns` (minimum over the
//! repetitions) is held to the same budget, and compared against the baseline only when the
//! baseline file has that key. `--compare` may repeat: the baseline is the minimum over the files,
//! which is how the workflow interleaves base and PR rounds. `--pr FILE` (repeatable) judges
//! previously saved runs instead of measuring: no ingestion, the minimum per key over the files is
//! checked against the budget and the baseline (the staged keys count only if every file has them).
//! Exit codes: 0 ok, 1 budget or regression failure, 2 bad arguments or unreadable baseline. See
//! `docs/src/benchmarks.md` for the method and the rule.
//!
//! `p50_ns` and `p95_ns` time the viewport fetch plus the construction of a `RowRef` for each
//! visible row; they read no cell and no per-row memory. `cells_p50_ns` and `cells_p95_ns` also
//! read every cell of those rows: the value of inline cells, and the length plus the first and
//! last byte of text, JSON and binary cells, so the walk touches the arenas.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, ObjectKind, ObjectPath, PrimaryKey, Table,
};
use sqler_sql::Dialect;
use sqler_ui::grid::edit::{EditContext, EditOrigin, EditorInput};
use sqler_ui::grid::{
    CellPos, CellRef, ColumnLayout, DisplayMap, GridLimits, GridModel, GridTab, IdentityRows,
    IngestState, Selection, SyntheticSource, grid_channel_with, spawn_ingest,
};

struct Args {
    rows: usize,
    columns: usize,
    batch: usize,
    viewport: usize,
    samples: usize,
    reps: usize,
    save: Option<String>,
    compare: Vec<String>,
    pr: Vec<String>,
    threshold_pct: u64,
    slack_ns: u64,
    cells_slack_ns: u64,
    budget_p95_ns: u64,
    layout_rows: usize,
    layout_columns: usize,
    staged: usize,
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
        compare: Vec::new(),
        pr: Vec::new(),
        threshold_pct: 25,
        slack_ns: 100,
        cells_slack_ns: 500,
        budget_p95_ns: 1_000_000,
        layout_rows: 100_000,
        layout_columns: 50,
        staged: 0,
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
            "--compare" => a.compare.push(value()?),
            "--pr" => a.pr.push(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--cells-slack-ns" => a.cells_slack_ns = num(value()?)?,
            "--budget-p95-ns" => a.budget_p95_ns = num(value()?)?,
            "--layout-rows" => a.layout_rows = to_usize(num(value()?)?),
            "--layout-columns" => a.layout_columns = to_usize(num(value()?)?),
            "--staged" => a.staged = to_usize(num(value()?)?),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.reps == 0 || a.rows == 0 || a.viewport > a.rows {
        return Err("need samples > 0, reps > 0, rows > 0 and viewport <= rows".to_owned());
    }
    if a.layout_columns < 2 || a.viewport > a.layout_rows {
        return Err("need layout-columns >= 2 and viewport <= layout-rows".to_owned());
    }
    if a.staged > a.rows || (a.staged > 0 && a.columns < 2) {
        return Err("need staged <= rows and, with staged, columns >= 2".to_owned());
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

/// What the staged-edits measurement reports.
struct Staged {
    /// Total time to stage every edit through the editor, in nanoseconds.
    stage_ns: u64,
    p50: u64,
    p95: u64,
}

fn synthetic_type(k: usize) -> DataType {
    match k % 4 {
        1 => DataType::Text,
        2 => DataType::Float,
        3 => DataType::Boolean,
        _ => DataType::Integer,
    }
}

/// A grid tab over `model` that is editable as `SELECT * FROM bench` (key `c0`, one table).
fn editable_tab(model: GridModel, columns: usize) -> GridTab {
    let infos = (0..columns)
        .map(|k| {
            let info = ColumnInfo::new(format!("c{k}"), synthetic_type(k));
            if k == 0 {
                info.with_nullable(false)
            } else {
                info
            }
        })
        .collect();
    let table = Table::new(DbObject::new(
        ObjectKind::Table,
        ObjectPath::root().in_schema("main"),
        "bench",
    ))
    .with_columns(infos)
    .with_primary_key(PrimaryKey::new(vec!["c0".to_owned()]));
    let mut tab = GridTab::new(model);
    tab.set_edit_context(EditContext {
        dialect: Some(Dialect::Sqlite),
        origin: EditOrigin::QueryResult("SELECT * FROM bench".to_owned()),
        table: Some(std::sync::Arc::new(table)),
        relation_kind: Some(ObjectKind::Table),
        resolved_schema: Some("main".to_owned()),
        production: false,
        apply_block: None,
    });
    tab
}

/// Stages `a.staged` text-cell edits, evenly spread over the rows, then times the display path of
/// a viewport of `GridTab::display_cell` calls that starts on a staged row. Minimum over the
/// repetitions, like the other keys.
fn measure_staged(a: &Args, model: GridModel) -> Result<Staged, String> {
    let mut tab = editable_tab(model, a.columns);
    let stride = (a.rows / a.staged).max(1);
    let staged_rows: Vec<usize> = (0..a.staged).map(|i| i * stride).collect();
    let start = Instant::now();
    {
        let (model, editor) = tab.edit_parts();
        for &row in &staged_rows {
            let pos = CellPos { row, col: 1 };
            editor
                .start_edit(model, pos)
                .map_err(|e| format!("cannot edit row {row}: {e}"))?;
            editor
                .commit_edit(model, EditorInput::Text("edited".to_owned()))
                .map_err(|e| format!("cannot stage row {row}: {e}"))?;
        }
    }
    let stage_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    if tab.edit().pending() != a.staged {
        return Err(format!(
            "staged {} edits, expected {}",
            tab.edit().pending(),
            a.staged
        ));
    }
    let max_first = (a.rows - a.viewport) as u64;
    let mut rng = 0xD1B5_4A32_D192_ED03_u64;
    let (mut p50, mut p95) = (u64::MAX, u64::MAX);
    for _ in 0..a.reps {
        let mut timings = Vec::with_capacity(a.samples);
        for _ in 0..a.samples {
            let pick = to_usize(xorshift(&mut rng) % (staged_rows.len() as u64));
            let first = staged_rows[pick].min(to_usize(max_first));
            let start = Instant::now();
            for row in first..first + a.viewport {
                for col in 0..a.columns {
                    black_box(tab.display_cell(row, col));
                }
            }
            let ns = start.elapsed().as_nanos();
            timings.push(u64::try_from(ns).unwrap_or(u64::MAX));
        }
        timings.sort_unstable();
        p50 = p50.min(percentile(&timings, 50));
        p95 = p95.min(percentile(&timings, 95));
    }
    Ok(Staged { stage_ns, p50, p95 })
}

/// The gated keys of a saved report. `cells_p95_ns` and `staged_p95_ns` are absent from reports
/// of earlier versions.
struct Baseline {
    p95: u64,
    cells_p95: Option<u64>,
    staged_p95: Option<u64>,
}

fn field(text: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .and_then(|v| v.trim().parse().ok())
}

fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))
}

/// Minimum over the baseline files: noise on a shared runner only adds time. `cells_p95` is
/// `Some` only when every file has the key; the same for `staged_p95`.
fn read_baseline(paths: &[String]) -> Result<Option<Baseline>, String> {
    let mut merged: Option<Baseline> = None;
    for path in paths {
        let text = read_file(path)?;
        let p95 = field(&text, "p95_ns").ok_or_else(|| format!("no p95_ns in {path}"))?;
        let cells_p95 = field(&text, "cells_p95_ns");
        let staged_p95 = field(&text, "staged_p95_ns");
        merged = Some(match merged {
            None => Baseline {
                p95,
                cells_p95,
                staged_p95,
            },
            Some(m) => Baseline {
                p95: m.p95.min(p95),
                cells_p95: m.cells_p95.zip(cells_p95).map(|(x, y)| x.min(y)),
                staged_p95: m.staged_p95.zip(staged_p95).map(|(x, y)| x.min(y)),
            },
        });
    }
    Ok(merged)
}

/// The keys of a measurement (or of a judged set of saved runs). The staged keys are `Some` only
/// when staged edits were measured (in every judged file).
struct Measured {
    p50: u64,
    p95: u64,
    cells_p50: u64,
    cells_p95: u64,
    staged_p50: Option<u64>,
    staged_p95: Option<u64>,
}

/// Judge mode: the minimum per key over saved runs; every key is required.
fn read_pr(paths: &[String]) -> Result<Measured, String> {
    let mut staged = Some((u64::MAX, u64::MAX));
    let mut best = Measured {
        p50: u64::MAX,
        p95: u64::MAX,
        cells_p50: u64::MAX,
        cells_p95: u64::MAX,
        staged_p50: None,
        staged_p95: None,
    };
    for path in paths {
        let text = read_file(path)?;
        let need = |key: &str| field(&text, key).ok_or_else(|| format!("no {key} in {path}"));
        best.p50 = best.p50.min(need("p50_ns")?);
        best.p95 = best.p95.min(need("p95_ns")?);
        best.cells_p50 = best.cells_p50.min(need("cells_p50_ns")?);
        best.cells_p95 = best.cells_p95.min(need("cells_p95_ns")?);
        staged = staged
            .zip(field(&text, "staged_p50_ns").zip(field(&text, "staged_p95_ns")))
            .map(|((a, b), (c, d))| (a.min(c), b.min(d)));
    }
    (best.staged_p50, best.staged_p95) = match staged {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };
    Ok(best)
}

/// The limit a measurement may reach: the baseline plus `pct` percent plus an absolute slack.
fn limit_of(base: u64, pct: u64, slack: u64) -> u128 {
    u128::from(base) * (u128::from(pct) + 100) / 100 + u128::from(slack)
}

/// Compares the measurements with the baseline, printing each failure. `cells_p95` and the staged
/// p95 are compared only when the baseline has the key (and, for the staged one, it was measured).
fn within_baseline(
    a: &Args,
    base: &Baseline,
    p95: u64,
    cells_p95: u64,
    staged_p95: Option<u64>,
) -> bool {
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
    match (base.staged_p95, staged_p95) {
        (Some(staged_base), Some(staged)) => {
            let limit = limit_of(staged_base, a.threshold_pct, a.cells_slack_ns);
            if u128::from(staged) > limit {
                eprintln!(
                    "FAIL: staged_p95 {staged} ns regressed against baseline {staged_base} ns (limit {limit} ns)"
                );
                ok = false;
            }
        }
        (None, Some(_)) => {
            eprintln!("note: the baseline has no staged_p95_ns; staged edits are not compared");
        }
        _ => {}
    }
    ok
}

/// Applies the budget and the baseline comparison; true when every check passed.
fn gate(a: &Args, baseline: Option<&Baseline>, m: &Measured) -> bool {
    let (p95, cells_p95, staged_p95) = (m.p95, m.cells_p95, m.staged_p95);
    let mut ok = true;
    if p95 > a.budget_p95_ns {
        eprintln!(
            "FAIL: p95 {p95} ns exceeds the budget of {} ns",
            a.budget_p95_ns
        );
        ok = false;
    }
    if let Some(p) = staged_p95
        && p > a.budget_p95_ns
    {
        eprintln!(
            "FAIL: staged p95 {p} ns exceeds the budget of {} ns",
            a.budget_p95_ns
        );
        ok = false;
    }
    if let Some(base) = baseline {
        ok &= within_baseline(a, base, p95, cells_p95, staged_p95);
    }
    ok
}

/// Measures the viewport fetches: the minimum over the repetitions of each percentile, plus the
/// ingestion facts of the report.
fn measure(a: &Args) -> Result<(Measured, String), String> {
    let rss_before = rss_bytes();
    let (model, ingest_time) = build_model(a)?;
    let rss_after = rss_bytes();
    let heap_bytes = model.heap_bytes();

    let span = (a.rows - a.viewport + 1) as u64;
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    // The gate uses the minimum over the repetitions: noise on a shared runner only adds time, so
    // the fastest repetition is the closest to the real cost.
    let mut best = Measured {
        p50: u64::MAX,
        p95: u64::MAX,
        cells_p50: u64::MAX,
        cells_p95: u64::MAX,
        staged_p50: None,
        staged_p95: None,
    };
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
        best.p50 = best.p50.min(percentile(&timings, 50));
        best.p95 = best.p95.min(percentile(&timings, 95));

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
        best.cells_p50 = best.cells_p50.min(percentile(&timings, 50));
        best.cells_p95 = best.cells_p95.min(percentile(&timings, 95));
    }

    let rss_delta = match (rss_before, rss_after) {
        (Some(b), Some(e)) => e.saturating_sub(b).to_string(),
        _ => "n/a".to_owned(),
    };
    let layout_model = build_layout_model(a)?;
    let (layout_p50, layout_p95) = measure_layout(a, &layout_model);
    drop(layout_model);
    let staged = if a.staged > 0 {
        Some(measure_staged(a, model)?)
    } else {
        None
    };
    best.staged_p50 = staged.as_ref().map(|s| s.p50);
    best.staged_p95 = staged.as_ref().map(|s| s.p95);
    let staged_report = staged.as_ref().map_or_else(String::new, |s| {
        format!(
            "staged_edits={}\nstaged_stage_ms={}\nstaged_p50_ns={}\nstaged_p95_ns={}\n",
            a.staged,
            s.stage_ns / 1_000_000,
            s.p50,
            s.p95
        )
    });
    let Measured {
        p50,
        p95,
        cells_p50,
        cells_p95,
        ..
    } = best;
    let report = format!(
        "bench=grid_viewport\nos={}-{}\nrows={}\ncolumns={}\nviewport_rows={}\nsamples={}\nreps={}\n\
         p50_ns={p50}\np95_ns={p95}\ncells_p50_ns={cells_p50}\ncells_p95_ns={cells_p95}\n\
         ingest_ms={}\nheap_bytes={}\nrss_delta_bytes={rss_delta}\n\
         layout_rows={}\nlayout_columns={}\nlayout_pinned=2\nlayout_p50_ns={layout_p50}\n\
         layout_p95_ns={layout_p95}\n{staged_report}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.rows,
        a.columns,
        a.viewport,
        a.samples,
        a.reps,
        ingest_time.as_millis(),
        heap_bytes,
        a.layout_rows,
        a.layout_columns,
    );
    Ok((best, report))
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baselines are read first so a bad path fails before the long run.
    let baseline = read_baseline(&a.compare).map_err(|e| (2, e))?;

    let (best, mut report) = if a.pr.is_empty() {
        measure(&a).map_err(|e| (1, e))?
    } else {
        let best = read_pr(&a.pr).map_err(|e| (2, e))?;
        let report = format!(
            "bench=grid_viewport\nos={}-{}\nmode=judge\nrounds={}\n\
             p50_ns={}\np95_ns={}\ncells_p50_ns={}\ncells_p95_ns={}\n{}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            a.pr.len(),
            best.p50,
            best.p95,
            best.cells_p50,
            best.cells_p95,
            match (best.staged_p50, best.staged_p95) {
                (Some(p50), Some(p95)) => format!("staged_p50_ns={p50}\nstaged_p95_ns={p95}\n"),
                _ => String::new(),
            },
        );
        (best, report)
    };
    if !a.pr.is_empty()
        && let Some(base) = &baseline
    {
        let cells = base
            .cells_p95
            .map_or_else(String::new, |c| format!("base_cells_p95_ns={c}\n"));
        let staged = base
            .staged_p95
            .map_or_else(String::new, |c| format!("base_staged_p95_ns={c}\n"));
        report = format!("{report}base_p95_ns={}\n{cells}{staged}", base.p95);
    }
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let ok = gate(&a, baseline.as_ref(), &best);
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
