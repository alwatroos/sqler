// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the client-side sort and filter of a 1M-row `GridModel` (ADR-0031).
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! against a saved baseline. Each key is the minimum over `--reps` repetitions. Gated keys:
//! `sort_p95_ns` (text sort), `filter_p95_ns` (quick filter over all columns) and
//! `viewport_sorted_p95_ns` (50-row windows of the sorted order), each with 25 % plus an absolute
//! slack. A key missing from the baseline file is noted and not compared. Information only:
//! `sort_int_p95_ns`, `sort_multi_p95_ns`, `filter_cond_p95_ns`, `order_heap_bytes` (published) and
//! `order_peak_heap_bytes` (the most while one text sort replaces another).
//! `slice_max_ns` (the longest 6 ms step of a text sort) has a budget.
//!
//! Three more columns of 1M rows cover the sorts that the 8-byte key cache cannot always decide:
//! `sort_dec_p95_ns` (decimals), `sort_url_p95_ns` (text that shares a 40-byte prefix) and
//! `sort_mixed_p95_ns` (a column of ints and floats). Each has a sort budget
//! (`--budget-sort-slow-ns` for decimals and mixed, `--budget-sort-url-ns` for the text, which
//! compares cells on most steps) and its own `slice_dec_max_ns`, `slice_url_max_ns`,
//! `slice_mixed_max_ns`, each under the same step budget as `slice_max_ns`. Exit codes: 0 ok,
//! 1 budget or regression failure, 2 bad arguments or unreadable baseline. See
//! `docs/src/benchmarks.md` for the method and the rule.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_driver_api::sqler_core::{Column, DataType, Decimal, Row, Value};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use sqler_ui::grid::{
    CellRef, Collation, Condition, FilterOp, FilterSpec, GridLimits, GridModel, GridOrder,
    IngestState, OrderProgress, QuickFilter, RowOrder, SortDirection, SortKey, SortSpec,
    SyntheticSource, grid_channel_with, spawn_ingest,
};

/// The step budget of the view's pump.
const SLICE: Duration = Duration::from_millis(6);

struct Args {
    rows: usize,
    columns: usize,
    batch: usize,
    viewport: usize,
    samples: usize,
    window_samples: usize,
    reps: usize,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    sort_slack_ns: u64,
    filter_slack_ns: u64,
    viewport_slack_ns: u64,
    budget_sort_ns: u64,
    budget_filter_ns: u64,
    budget_viewport_ns: u64,
    budget_slice_ns: u64,
    budget_sort_slow_ns: u64,
    budget_sort_url_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 1_000_000,
        columns: 6,
        batch: 1024,
        viewport: 50,
        samples: 5,
        window_samples: 100_000,
        reps: 3,
        save: None,
        compare: None,
        threshold_pct: 25,
        sort_slack_ns: 20_000_000,
        filter_slack_ns: 10_000_000,
        viewport_slack_ns: 500,
        budget_sort_ns: 1_500_000_000,
        budget_filter_ns: 500_000_000,
        budget_viewport_ns: 1_000_000,
        budget_slice_ns: 16_000_000,
        budget_sort_slow_ns: 500_000_000,
        budget_sort_url_ns: 5_000_000_000,
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
            "--window-samples" => a.window_samples = to_usize(num(value()?)?),
            "--reps" => a.reps = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--sort-slack-ns" => a.sort_slack_ns = num(value()?)?,
            "--filter-slack-ns" => a.filter_slack_ns = num(value()?)?,
            "--viewport-slack-ns" => a.viewport_slack_ns = num(value()?)?,
            "--budget-sort-ns" => a.budget_sort_ns = num(value()?)?,
            "--budget-filter-ns" => a.budget_filter_ns = num(value()?)?,
            "--budget-viewport-ns" => a.budget_viewport_ns = num(value()?)?,
            "--budget-slice-ns" => a.budget_slice_ns = num(value()?)?,
            "--budget-sort-slow-ns" => a.budget_sort_slow_ns = num(value()?)?,
            "--budget-sort-url-ns" => a.budget_sort_url_ns = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.window_samples == 0 || a.reps == 0 || a.rows == 0 {
        return Err("need samples, window-samples, reps and rows > 0".to_owned());
    }
    if a.columns < 4 || a.viewport == 0 || a.viewport > a.rows {
        return Err("need columns >= 4 and 0 < viewport <= rows".to_owned());
    }
    Ok(a)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Reads the payload of a cell: the length plus the first and last byte of text, the value for
/// the inline kinds.
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

/// Rows of three columns whose sort the key cache decides only in part: a decimal, a URL-like
/// text that shares its first 40 bytes, and a mix of ints and floats. The order is pseudo-random.
struct HardSource {
    rows: usize,
    batch: usize,
    next_row: usize,
    stage: u8,
    rng: u64,
}

impl HardSource {
    fn new(rows: usize, batch: usize) -> Self {
        Self {
            rows,
            batch: batch.max(1),
            next_row: 0,
            stage: 0,
            rng: 0xD1B5_4A32_D192_ED03,
        }
    }

    fn row(&mut self) -> Row {
        let (a, b, c) = (
            xorshift(&mut self.rng),
            xorshift(&mut self.rng),
            xorshift(&mut self.rng),
        );
        let sign = if a % 7 == 0 { "-" } else { "" };
        let dec = format!("{sign}{}.{:04}", a % 10_000_000, b % 10_000)
            .parse::<Decimal>()
            .map_or(Value::Null, Value::Decimal);
        let url = format!(
            "https://example.com/api/v1/customers/{:08}/orders",
            b % 100_000_000
        );
        #[allow(clippy::cast_precision_loss)] // a bench value far below 2^53
        let mixed = if c % 2 == 0 {
            Value::Int(i64::try_from(c % 2_000_000).unwrap_or(0) - 1_000_000)
        } else {
            Value::Float((c % 2_000_000) as f64 / 2.0 - 500_000.25)
        };
        Row(vec![dec, Value::Text(url), mixed])
    }
}

impl QueryStream for HardSource {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let event = match self.stage {
            0 => {
                self.stage = 1;
                Some(QueryEvent::Columns(vec![
                    Column::new("dec", DataType::Decimal),
                    Column::new("url", DataType::Text),
                    Column::new("mixed", DataType::Float),
                ]))
            }
            1 if self.next_row < self.rows => {
                let n = self.batch.min(self.rows - self.next_row);
                self.next_row += n;
                Some(QueryEvent::Rows((0..n).map(|_| self.row()).collect()))
            }
            1 => {
                self.stage = 2;
                Some(QueryEvent::Done {
                    rows_affected: None,
                })
            }
            _ => None,
        };
        Box::pin(std::future::ready(event.map(Ok)))
    }
}

fn build_model(a: &Args) -> Result<GridModel, String> {
    ingest_all(SyntheticSource::new(a.rows, a.columns, a.batch))
}

fn ingest_all<S: QueryStream + 'static>(source: S) -> Result<GridModel, String> {
    // The bench measures a whole 1M-row result, so it is not paged.
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
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

fn key(column: usize, direction: SortDirection) -> SortKey {
    SortKey { column, direction }
}

fn sort_spec(keys: Vec<SortKey>) -> SortSpec {
    SortSpec { keys }
}

fn quick(text: &str) -> FilterSpec {
    FilterSpec {
        quick: Some(QuickFilter {
            text: text.to_owned(),
            column: None,
        }),
        conditions: Vec::new(),
    }
}

fn conditions(list: Vec<Condition>) -> FilterSpec {
    FilterSpec {
        quick: None,
        conditions: list,
    }
}

/// Runs one full job to completion and returns its duration and the finished order.
fn full_run(model: &GridModel, sort: &SortSpec, filter: &FilterSpec) -> (u64, GridOrder) {
    let mut order = GridOrder::default();
    order.set_collation(Collation::CaseInsensitive);
    order.set_sort(sort.clone());
    order.set_filter(filter.clone());
    let start = Instant::now();
    let progress = order.step(model, None);
    let ns = nanos(start.elapsed());
    black_box(progress);
    (ns, order)
}

/// The p95 over `samples` full runs.
fn run_p95(model: &GridModel, samples: usize, sort: &SortSpec, filter: &FilterSpec) -> u64 {
    let mut timings: Vec<u64> = (0..samples)
        .map(|_| full_run(model, sort, filter).0)
        .collect();
    timings.sort_unstable();
    percentile(&timings, 95)
}

/// The longest `step(Some(6 ms))` call of one text sort, run until it ends.
fn slice_max(model: &GridModel, sort: &SortSpec) -> u64 {
    let mut order = GridOrder::default();
    order.set_sort(sort.clone());
    let mut longest = 0;
    loop {
        let start = Instant::now();
        let progress = order.step(model, Some(SLICE));
        longest = longest.max(nanos(start.elapsed()));
        if !matches!(progress, OrderProgress::Working { .. }) {
            return longest;
        }
    }
}

/// The most heap `GridOrder::heap_bytes` reports while a text sort replaces a published text
/// sort, in 6 ms steps: the published order and inverse, plus the key cache, the kept rows, the
/// merge buffers and the new inverse of the running job.
fn peak_heap(model: &GridModel, sort: &SortSpec) -> usize {
    let mut order = GridOrder::default();
    order.set_sort(sort.clone());
    order.step(model, None);
    order.set_sort(sort_spec(vec![key(1, SortDirection::Descending)]));
    let mut peak = 0;
    loop {
        let progress = order.step(model, Some(SLICE));
        peak = peak.max(order.heap_bytes());
        if !matches!(progress, OrderProgress::Working { .. }) {
            return peak;
        }
    }
}

/// The p95 of mapping a random window of the sorted order and reading every cell.
fn viewport_p95(model: &GridModel, order: &GridOrder, a: &Args, rng: &mut u64) -> u64 {
    let rows = order.rows(model.row_count());
    let span = (rows.len() - a.viewport + 1) as u64;
    let mut timings = Vec::with_capacity(a.window_samples);
    for _ in 0..a.window_samples {
        let first = to_usize(xorshift(rng) % span);
        let start = Instant::now();
        for display in first..first + a.viewport {
            let Some(m) = rows.model_row(display) else {
                continue;
            };
            let view = model.viewport(m, 1);
            for row in view.iter() {
                for cell in row.iter() {
                    black_box(touch(cell));
                }
            }
        }
        timings.push(nanos(start.elapsed()));
    }
    timings.sort_unstable();
    percentile(&timings, 95)
}

/// The gated keys of a saved report. A key is `None` when the file does not have it.
struct Baseline {
    sort: Option<u64>,
    filter: Option<u64>,
    viewport: Option<u64>,
}

fn read_baseline(path: &str) -> Result<Baseline, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let key = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse().ok())
    };
    Ok(Baseline {
        sort: key("sort_p95_ns="),
        filter: key("filter_p95_ns="),
        viewport: key("viewport_sorted_p95_ns="),
    })
}

/// The limit a measurement may reach: the baseline plus `pct` percent plus an absolute slack.
fn limit_of(base: u64, pct: u64, slack: u64) -> u128 {
    u128::from(base) * (u128::from(pct) + 100) / 100 + u128::from(slack)
}

/// Compares one key; a key the baseline lacks is noted, not failed.
fn gate(name: &str, base: Option<u64>, now: u64, pct: u64, slack: u64) -> bool {
    let Some(base) = base else {
        eprintln!("note: the baseline has no {name}; it is not compared");
        return true;
    };
    let limit = limit_of(base, pct, slack);
    if u128::from(now) > limit {
        eprintln!("FAIL: {name} {now} ns regressed against baseline {base} ns (limit {limit} ns)");
        return false;
    }
    true
}

/// The seven measurements (minimum over the repetitions) and the heap of the text-sorted order:
/// `sort`, `filter`, `sort_int`, `sort_multi`, `filter_cond`, `slice`, `viewport`.
fn measure(a: &Args, model: &GridModel) -> ([u64; 7], usize, usize) {
    let text_up = sort_spec(vec![key(1, SortDirection::Ascending)]);
    let int_down = sort_spec(vec![key(0, SortDirection::Descending)]);
    let multi = sort_spec(vec![
        key(3, SortDirection::Ascending),
        key(1, SortDirection::Descending),
    ]);
    let none = SortSpec::default();
    let quick_99 = quick("99");
    let cond = conditions(vec![
        Condition {
            column: 2,
            op: FilterOp::Greater,
            operand: "500000".to_owned(),
        },
        Condition {
            column: 1,
            op: FilterOp::Contains,
            operand: "7".to_owned(),
        },
    ]);
    let no_filter = FilterSpec::default();

    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    // The gate uses the minimum over the repetitions: noise on a shared runner only adds time.
    let mut best = [u64::MAX; 7];
    let mut heap = 0;
    let peak = peak_heap(model, &text_up);
    for _ in 0..a.reps {
        let m = [
            run_p95(model, a.samples, &text_up, &no_filter),
            run_p95(model, a.samples, &none, &quick_99),
            run_p95(model, a.samples, &int_down, &no_filter),
            run_p95(model, a.samples, &multi, &no_filter),
            run_p95(model, a.samples, &none, &cond),
            slice_max(model, &text_up),
        ];
        let (_, sorted) = full_run(model, &text_up, &no_filter);
        heap = sorted.heap_bytes();
        let viewport = viewport_p95(model, &sorted, a, &mut rng);
        for (slot, v) in best.iter_mut().zip(m.into_iter().chain([viewport])) {
            *slot = (*slot).min(v);
        }
    }
    (best, heap, peak)
}

/// The slow-sort scenarios on the hard model: sort p95 and longest step for the decimal, URL
/// and mixed columns, as `[sort_dec, sort_url, sort_mixed, slice_dec, slice_url, slice_mixed]`
/// (minimum over the repetitions).
fn measure_hard(a: &Args, model: &GridModel) -> [u64; 6] {
    let specs: Vec<SortSpec> = (0..3)
        .map(|c| sort_spec(vec![key(c, SortDirection::Ascending)]))
        .collect();
    let none = FilterSpec::default();
    let samples = a.samples.min(3);
    let mut best = [u64::MAX; 6];
    for _ in 0..a.reps {
        for (i, spec) in specs.iter().enumerate() {
            let sort = run_p95(model, samples, spec, &none);
            let slice = slice_max(model, spec);
            best[i] = best[i].min(sort);
            best[3 + i] = best[3 + i].min(slice);
        }
    }
    best
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(read_baseline(path).map_err(|e| (2, e))?),
        None => None,
    };
    let model = build_model(&a).map_err(|e| (1, e))?;

    let (best, heap, peak) = measure(&a, &model);
    drop(model);
    let hard = ingest_all(HardSource::new(a.rows, a.batch)).map_err(|e| (1, e))?;
    let [
        sort_dec,
        sort_url,
        sort_mixed,
        slice_dec,
        slice_url,
        slice_mixed,
    ] = measure_hard(&a, &hard);
    drop(hard);
    let [
        sort,
        filter,
        sort_int,
        sort_multi,
        filter_cond,
        slice,
        viewport,
    ] = best;
    let report = format!(
        "bench=grid_sort\nos={}-{}\nrows={}\ncolumns={}\nviewport_rows={}\nsamples={}\n\
         window_samples={}\nreps={}\nsort_p95_ns={sort}\nsort_int_p95_ns={sort_int}\n\
         sort_multi_p95_ns={sort_multi}\nfilter_p95_ns={filter}\nfilter_cond_p95_ns={filter_cond}\n\
         viewport_sorted_p95_ns={viewport}\nslice_max_ns={slice}\norder_heap_bytes={heap}\norder_peak_heap_bytes={peak}\n\
         sort_dec_p95_ns={sort_dec}\nsort_url_p95_ns={sort_url}\nsort_mixed_p95_ns={sort_mixed}\n\
         slice_dec_max_ns={slice_dec}\nslice_url_max_ns={slice_url}\nslice_mixed_max_ns={slice_mixed}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.rows,
        a.columns,
        a.viewport,
        a.samples,
        a.window_samples,
        a.reps,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut ok = true;
    for (name, now, budget) in [
        ("sort_p95_ns", sort, a.budget_sort_ns),
        ("filter_p95_ns", filter, a.budget_filter_ns),
        ("viewport_sorted_p95_ns", viewport, a.budget_viewport_ns),
        ("slice_max_ns", slice, a.budget_slice_ns),
        ("sort_dec_p95_ns", sort_dec, a.budget_sort_slow_ns),
        ("sort_url_p95_ns", sort_url, a.budget_sort_url_ns),
        ("sort_mixed_p95_ns", sort_mixed, a.budget_sort_slow_ns),
        ("slice_dec_max_ns", slice_dec, a.budget_slice_ns),
        ("slice_url_max_ns", slice_url, a.budget_slice_ns),
        ("slice_mixed_max_ns", slice_mixed, a.budget_slice_ns),
    ] {
        if now > budget {
            eprintln!("FAIL: {name} {now} ns exceeds the budget of {budget} ns");
            ok = false;
        }
    }
    if let Some(base) = baseline {
        let pct = a.threshold_pct;
        ok &= gate("sort_p95_ns", base.sort, sort, pct, a.sort_slack_ns);
        ok &= gate("filter_p95_ns", base.filter, filter, pct, a.filter_slack_ns);
        ok &= gate(
            "viewport_sorted_p95_ns",
            base.viewport,
            viewport,
            pct,
            a.viewport_slack_ns,
        );
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
