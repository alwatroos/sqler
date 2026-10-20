// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of viewport fetches from a 1M-row `GridModel`.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! the p95 against a saved baseline. Exit codes: 0 ok, 1 budget or regression failure, 2 bad
//! arguments or unreadable baseline. See `docs/src/benchmarks.md` for the method and the rule.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_ui::grid::{GridModel, IngestState, SyntheticSource, grid_channel, spawn_ingest};

struct Args {
    rows: usize,
    columns: usize,
    batch: usize,
    viewport: usize,
    samples: usize,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    budget_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 1_000_000,
        columns: 6,
        batch: 1024,
        viewport: 50,
        samples: 100_000,
        save: None,
        compare: None,
        threshold_pct: 25,
        slack_ns: 50,
        budget_p95_ns: 1_000_000,
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
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--budget-p95-ns" => a.budget_p95_ns = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.rows == 0 || a.viewport > a.rows {
        return Err("need samples > 0, rows > 0 and viewport <= rows".to_owned());
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

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn build_model(a: &Args) -> Result<(GridModel, Duration), String> {
    let (mut model, sink) = grid_channel();
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

fn base_p95(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("p95_ns="))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("no p95_ns in {path}"))
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(base_p95(path).map_err(|e| (2, e))?),
        None => None,
    };

    let rss_before = rss_bytes();
    let (model, ingest_time) = build_model(&a).map_err(|e| (1, e))?;
    let rss_after = rss_bytes();

    let span = (a.rows - a.viewport + 1) as u64;
    let mut rng = 0x9E37_79B9_7F4A_7C15_u64;
    let mut timings = Vec::with_capacity(a.samples);
    for _ in 0..a.samples {
        let first = to_usize(xorshift(&mut rng) % span);
        let start = Instant::now();
        let view = model.viewport(first, a.viewport);
        for row in view.iter() {
            for value in &row.0 {
                black_box(value);
            }
        }
        let ns = start.elapsed().as_nanos();
        timings.push(u64::try_from(ns).unwrap_or(u64::MAX));
    }
    timings.sort_unstable();
    let p50 = percentile(&timings, 50);
    let p95 = percentile(&timings, 95);

    let rss_delta = match (rss_before, rss_after) {
        (Some(b), Some(e)) => e.saturating_sub(b).to_string(),
        _ => "n/a".to_owned(),
    };
    let report = format!(
        "bench=grid_viewport\nos={}-{}\nrows={}\ncolumns={}\nviewport_rows={}\nsamples={}\n\
         p50_ns={p50}\np95_ns={p95}\ningest_ms={}\nheap_bytes={}\nrss_delta_bytes={rss_delta}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.rows,
        a.columns,
        a.viewport,
        a.samples,
        ingest_time.as_millis(),
        model.heap_bytes(),
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
        let limit =
            u128::from(base) * (u128::from(a.threshold_pct) + 100) / 100 + u128::from(a.slack_ns);
        if u128::from(p95) > limit {
            eprintln!("FAIL: p95 {p95} ns regressed against baseline {base} ns (limit {limit} ns)");
            ok = false;
        }
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
