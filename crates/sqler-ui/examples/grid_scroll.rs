// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless run of the results-grid windowing over a synthetic result.
//!
//! Loads the rows through `GridTab::spawn_synthetic` while polling, then simulates frames: scroll
//! by a step, take the visible row range and read the text of every visible cell (the data cost
//! of a frame, with no GPU). Prints `key=value` lines. Exit codes: 0 ok, 1 the windowing is not
//! bounded or the last row was not reached, 2 bad arguments or ingestion failure.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_ui::grid::{GridTab, IngestState, POLL_BUDGET, ScrollViewport, nearest_rank};

struct Args {
    rows: usize,
    columns: usize,
    batch: usize,
    height: f64,
    row_height: f64,
    step: f64,
    overscan: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        rows: 1_000_000,
        columns: 6,
        batch: 1024,
        height: 760.0,
        row_height: 32.0,
        step: 96.0,
        overscan: 2,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let int = |s: String| {
            s.parse::<usize>()
                .map_err(|_| format!("bad number for {flag}: {s}"))
        };
        let float = |s: String| {
            s.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("bad number for {flag}: {s}"))
        };
        match flag.as_str() {
            "--rows" => a.rows = int(value()?)?,
            "--columns" => a.columns = int(value()?)?,
            "--batch" => a.batch = int(value()?)?,
            "--height" => a.height = float(value()?)?,
            "--row-height" => a.row_height = float(value()?)?,
            "--step" => a.step = float(value()?)?,
            "--overscan" => a.overscan = int(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.rows == 0 || a.columns == 0 || a.step <= 0.0 {
        return Err("need rows > 0, columns > 0 and step > 0".to_owned());
    }
    Ok(a)
}

/// Resident set size in bytes on Linux; `None` elsewhere.
fn rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[allow(clippy::cast_precision_loss)] // Byte counts for display only.
fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// One simulated frame: the visible range, then the text of every visible cell.
fn frame(tab: &mut GridTab, vp: &ScrollViewport) -> (usize, Duration) {
    let start = Instant::now();
    let range = vp.visible();
    tab.set_visible(range.clone());
    let mut cells = 0;
    for row in range.clone() {
        for col in 0..tab.column_count() {
            black_box(tab.cell_text(row, col));
            cells += 1;
        }
    }
    black_box(cells);
    (range.len(), start.elapsed())
}

fn run(a: &Args) -> Result<bool, String> {
    let rss_before = rss_bytes();
    let start = Instant::now();
    let (mut tab, _cancel) = GridTab::spawn_synthetic(a.rows, a.columns, a.batch);
    loop {
        match tab.state() {
            IngestState::Done { .. } => break,
            IngestState::Waiting | IngestState::Streaming => {}
            other => return Err(format!("ingestion ended in state {other:?}")),
        }
        if tab.poll(POLL_BUDGET).applied == 0 {
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    let ingest = start.elapsed();
    tab.poll(POLL_BUDGET);
    let rss_after = rss_bytes();

    let mut vp = ScrollViewport::new(a.row_height, a.height, a.overscan);
    vp.set_rows(tab.row_count());
    let cap = vp.max_visible_rows();
    let mut max_rows = 0;
    let mut reached_last = false;
    let mut costs: Vec<u64> = Vec::new();
    loop {
        let (n, cost) = frame(&mut tab, &vp);
        max_rows = max_rows.max(n);
        reached_last |= vp.visible().end == tab.row_count();
        costs.push(nanos(cost));
        if vp.offset() >= vp.max_offset() {
            break;
        }
        vp.scroll_by(a.step);
    }
    vp.jump_to_start();
    let (n, _) = frame(&mut tab, &vp);
    max_rows = max_rows.max(n);
    vp.jump_to_end();
    let (n, jump_cost) = frame(&mut tab, &vp);
    max_rows = max_rows.max(n);
    reached_last |= vp.visible().end == tab.row_count();

    costs.sort_unstable();
    println!("rows={}", tab.row_count());
    println!("ingest_ms={:.1}", ingest.as_secs_f64() * 1000.0);
    println!("frames={}", costs.len());
    println!("max_rows_per_frame={max_rows}");
    println!("max_visible_rows={cap}");
    println!("last_row_reached={reached_last}");
    println!(
        "frame_cost_p50_ns={}",
        nearest_rank(&costs, 50).unwrap_or(0)
    );
    println!(
        "frame_cost_p95_ns={}",
        nearest_rank(&costs, 95).unwrap_or(0)
    );
    println!("frame_cost_max_ns={}", costs.last().copied().unwrap_or(0));
    println!("jump_to_end_ns={}", nanos(jump_cost));
    println!(
        "heap_mb={:.1}",
        mb(u64::try_from(tab.heap_bytes()).unwrap_or(u64::MAX))
    );
    if let (Some(before), Some(after)) = (rss_before, rss_after) {
        println!("rss_delta_mb={:.1}", mb(after.saturating_sub(before)));
    }
    Ok(max_rows <= cap && reached_last)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("grid_scroll: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("grid_scroll: windowing not bounded or last row not reached");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("grid_scroll: {e}");
            ExitCode::from(2)
        }
    }
}
