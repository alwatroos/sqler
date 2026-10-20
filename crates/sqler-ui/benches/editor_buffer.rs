// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the headless SQL editor logic on a large script.
//!
//! Std-only (`harness = false`). Prints `key=value` lines. Exit codes: 0 ok, 1 when the p95 of the bracket index
//! build or of formatting exceeds the budget, 2 on bad arguments.
//!
//! Metrics (nanoseconds, nearest-rank percentile over `--samples` runs on a synthetic script of `--lines` lines):
//! - `bracket_build_p95_ns`: build the bracket index of the whole text.
//! - `bracket_lookup_p95_ns`: find the pair at a cursor (index already built).
//! - `format_p95_ns`: the edit that formats the whole buffer.
//! - `occurrence_p95_ns`: find the next occurrence of a word, searching from the start of the text.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use sqler_sql::{Dialect, FormatOptions};
use sqler_ui::editor::{BracketIndex, EditorBuffer, format_edit, next_occurrence, synthetic};

struct Args {
    lines: usize,
    samples: usize,
    budget_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        lines: 10_000,
        samples: 30,
        budget_p95_ns: 16_000_000,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let n: u64 = value
            .parse()
            .map_err(|_| format!("bad number for {flag}: {value}"))?;
        match flag.as_str() {
            "--lines" => a.lines = usize::try_from(n).unwrap_or(usize::MAX),
            "--samples" => a.samples = usize::try_from(n).unwrap_or(usize::MAX),
            "--budget-p95-ns" => a.budget_p95_ns = n,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.lines == 0 || a.samples == 0 {
        return Err("need lines > 0 and samples > 0".to_owned());
    }
    Ok(a)
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn nanos(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Runs `f` `samples` times and returns the sorted times.
fn sample(samples: usize, mut f: impl FnMut()) -> Vec<u64> {
    let mut v = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        f();
        v.push(nanos(start));
    }
    v.sort_unstable();
    v
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    let text = synthetic::script(a.lines);
    let dialect = Dialect::Postgres;

    let build = sample(a.samples, || {
        black_box(BracketIndex::build(Some(dialect), black_box(&text)));
    });

    let index = BracketIndex::build(Some(dialect), &text);
    // Cursors spread over the text, each moved onto a bracket where there is one.
    let cursors: Vec<usize> = (0..a.samples.max(1))
        .map(|i| {
            let at = text.len() / a.samples.max(1) * i;
            text[at..].find('(').map_or(at, |p| at + p)
        })
        .collect();
    let mut k = 0;
    let lookup = sample(a.samples, || {
        black_box(index.pair_at(&text, cursors[k % cursors.len()]));
        k += 1;
    });

    let buffer = EditorBuffer::new(text.clone(), 0);
    let options = FormatOptions::default();
    let mut failed = None;
    let format = sample(a.samples, || {
        match format_edit(dialect, black_box(&buffer), &options) {
            Ok(edit) => {
                black_box(edit);
            }
            Err(e) => failed = Some(e.to_string()),
        }
    });
    if let Some(e) = failed {
        return Err((1, format!("format failed: {e}")));
    }

    let occurrence = sample(a.samples, || {
        black_box(next_occurrence(black_box(&text), "other", 0));
        black_box(next_occurrence(black_box(&text), "no_such_word_here", 0));
    });

    let (build_p95, lookup_p95) = (percentile(&build, 95), percentile(&lookup, 95));
    let (format_p95, occurrence_p95) = (percentile(&format, 95), percentile(&occurrence, 95));
    print!(
        "bench=editor_buffer\nos={}-{}\nlines={}\nbytes={}\nsamples={}\n\
         bracket_build_p50_ns={}\nbracket_build_p95_ns={build_p95}\nbracket_lookup_p95_ns={lookup_p95}\n\
         format_p50_ns={}\nformat_p95_ns={format_p95}\noccurrence_p95_ns={occurrence_p95}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.lines,
        text.len(),
        a.samples,
        percentile(&build, 50),
        percentile(&format, 50),
    );

    let mut pass = true;
    for (label, p95) in [("bracket build", build_p95), ("format", format_p95)] {
        if p95 > a.budget_p95_ns {
            eprintln!(
                "FAIL: {label} p95 {p95} ns exceeds the budget of {} ns",
                a.budget_p95_ns
            );
            pass = false;
        }
    }
    Ok(if pass {
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
