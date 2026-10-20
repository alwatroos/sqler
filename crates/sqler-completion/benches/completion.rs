// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the completion engine on a synthetic schema with thousands of tables.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! `complete_p95_ns` against a saved baseline. The measurement loop runs `--reps` times (3) in
//! process and the repetition with the lowest `complete_p95_ns` is reported and compared, so a
//! noisy neighbour in one repetition does not fail the gate. Exit codes: 0 ok, 1 budget or regression failure,
//! 2 bad arguments or unreadable baseline. See `docs/src/benchmarks.md` for the method and rule.

use std::fmt::Write as _;
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use sqler_completion::{CompletionRequest, MemorySchema, complete};
use sqler_core::{ColumnInfo, DataType, ObjectKind};
use sqler_sql::Dialect;

struct Args {
    tables: usize,
    columns: usize,
    samples: usize,
    reps: usize,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    budget_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5000,
        columns: 8,
        samples: 2000,
        reps: 3,
        save: None,
        compare: None,
        threshold_pct: 25,
        // 1/64 of the budget: a real 2x regression still fails, runner noise does not.
        slack_ns: 250_000,
        budget_p95_ns: 16_000_000,
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
            "--tables" => a.tables = to_usize(num(value()?)?),
            "--columns" => a.columns = to_usize(num(value()?)?),
            "--samples" => a.samples = to_usize(num(value()?)?),
            "--reps" => a.reps = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--budget-p95-ns" => a.budget_p95_ns = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.reps == 0 || a.tables == 0 || a.columns == 0 {
        return Err("need samples, reps, tables and columns > 0".to_owned());
    }
    Ok(a)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn nanos(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// `public` with `tables` tables plus 20 more schemas of 10 tables each.
fn build_schema(tables: usize, columns: usize) -> MemorySchema {
    let mut s = MemorySchema::new(["public"]);
    let cols = |n: usize| -> Vec<ColumnInfo> {
        (0..n)
            .map(|c| {
                ColumnInfo::new(format!("col_{c}"), DataType::Integer).with_native_type("integer")
            })
            .collect()
    };
    for t in 0..tables {
        s.add_table(
            "public",
            &format!("table_{t:05}"),
            ObjectKind::Table,
            Some(cols(columns)),
        );
    }
    for i in 0..20 {
        for t in 0..10 {
            s.add_table(
                &format!("schema_{i:02}"),
                &format!("rel_{t:02}"),
                ObjectKind::Table,
                Some(cols(3)),
            );
        }
    }
    s
}

/// A scenario: name, text and the cursor offset.
struct Scenario {
    name: &'static str,
    text: String,
    offset: usize,
}

/// Text with `|` marking the cursor.
fn at_marker(name: &'static str, sql: &str) -> Scenario {
    let offset = sql.find('|').unwrap_or(sql.len());
    Scenario {
        name,
        text: sql.replacen('|', "", 1),
        offset,
    }
}

fn scenarios(tables: usize) -> Vec<Scenario> {
    let t = |i: usize| format!("table_{:05}", i % tables);
    let join5 = (2..=5).fold(format!("SELECT | FROM {} a1", t(1)), |acc, i| {
        format!("{acc} JOIN {} a{i} ON a{i}.col_0 = a{}.col_0", t(i), i - 1)
    });
    let mut script = String::new();
    for i in 0..2000 {
        let _ = writeln!(script, "SELECT col_0 FROM {} WHERE col_1 = {i};", t(i));
    }
    let mid = script
        .match_indices('\n')
        .nth(1000)
        .map_or(script.len(), |(i, _)| i + 1);
    script.insert_str(
        mid,
        &format!("SELECT co| FROM {} t WHERE t.col_1 = 1;\n", t(500)),
    );
    vec![
        at_marker("from_empty", "SELECT * FROM |"),
        at_marker("from_prefix", "SELECT * FROM table_00|"),
        at_marker("from_fuzzy", "SELECT * FROM t_4|"),
        at_marker("select_list_join5", &join5),
        at_marker("alias_dot", &format!("SELECT a.| FROM {} a", t(1))),
        at_marker(
            "where_prefix",
            &format!(
                "SELECT * FROM {} a JOIN {} b ON a.col_0 = b.col_0 WHERE col_|",
                t(1),
                t(2)
            ),
        ),
        at_marker("insert_columns", &format!("INSERT INTO {} (|", t(7))),
        at_marker("big_script", &script),
    ]
}

fn base_p95(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("complete_p95_ns="))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("no complete_p95_ns in {path}"))
}

/// The figures of one repetition of the measurement loop.
struct Measured {
    lines: String,
    p50: u64,
    p95: u64,
    worst: u64,
}

/// One repetition: warm-up and `samples` timed requests for every scenario.
fn measure(a: &Args, schema: &MemorySchema, scs: &[Scenario]) -> Result<Measured, String> {
    let mut all: Vec<u64> = Vec::new();
    let mut lines = String::new();
    let mut worst = 0u64;
    for sc in scs {
        for _ in 0..20 {
            let req = CompletionRequest::new(&sc.text, sc.offset, Dialect::Postgres);
            let done = complete(black_box(&req), schema);
            if done.items.is_empty() {
                return Err(format!("scenario {} returned no items", sc.name));
            }
            black_box(done);
        }
        let mut times = Vec::with_capacity(a.samples);
        for _ in 0..a.samples {
            let req = CompletionRequest::new(&sc.text, sc.offset, Dialect::Postgres);
            let start = Instant::now();
            let done = complete(black_box(&req), schema);
            let ns = nanos(start.elapsed());
            black_box(done);
            times.push(ns);
        }
        all.extend_from_slice(&times);
        times.sort_unstable();
        let (p50, p95) = (percentile(&times, 50), percentile(&times, 95));
        worst = worst.max(p95);
        let _ = writeln!(lines, "{}_p50_ns={p50}\n{}_p95_ns={p95}", sc.name, sc.name);
    }
    all.sort_unstable();
    Ok(Measured {
        lines,
        p50: percentile(&all, 50),
        p95: percentile(&all, 95),
        worst,
    })
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(base_p95(path).map_err(|e| (2, e))?),
        None => None,
    };

    let schema = build_schema(a.tables, a.columns);
    let scs = scenarios(a.tables);
    let mut best: Option<Measured> = None;
    for _ in 0..a.reps {
        let m = measure(&a, &schema, &scs).map_err(|e| (1, e))?;
        if best.as_ref().is_none_or(|b| m.p95 < b.p95) {
            best = Some(m);
        }
    }
    let Some(Measured {
        lines,
        p50,
        p95: complete_p95,
        worst,
    }) = best
    else {
        return Err((2, "no repetition ran".to_owned()));
    };
    let report = format!(
        "bench=completion\nos={}-{}\ntables={}\ncolumns={}\nsamples={}\nreps={}\n{lines}complete_p50_ns={p50}\ncomplete_p95_ns={complete_p95}\nworst_p95_ns={worst}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.tables,
        a.columns,
        a.samples,
        a.reps,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut ok = true;
    for (name, v) in [("complete", complete_p95), ("worst scenario", worst)] {
        if v > a.budget_p95_ns {
            eprintln!(
                "FAIL: {name} p95 {v} ns exceeds the budget of {} ns",
                a.budget_p95_ns
            );
            ok = false;
        }
    }
    if let Some(base) = baseline {
        let limit =
            u128::from(base) * (u128::from(a.threshold_pct) + 100) / 100 + u128::from(a.slack_ns);
        if u128::from(complete_p95) > limit {
            eprintln!(
                "FAIL: complete p95 {complete_p95} ns regressed against baseline {base} ns (limit {limit} ns)"
            );
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
