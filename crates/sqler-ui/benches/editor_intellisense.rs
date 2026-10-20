// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the headless editor intellisense on a large synthetic schema.
//!
//! Std-only (`harness = false`). Prints `key=value` lines. Exit codes: 0 ok, 1 when the worst p95 exceeds the
//! budget (or the setup fails), 2 on bad arguments.
//!
//! Every sample takes a fresh snapshot of the catalog, builds the schema source and runs one operation,
//! as the editor does per keystroke (ADR-0022). Metrics (nanoseconds, nearest-rank percentile over
//! `--samples` runs, `--tables` tables with `--columns` columns each):
//! - `statement_start_p95_ns`: complete at an empty statement.
//! - `from_prefix_p95_ns`: complete `SELECT * FROM t_04|`.
//! - `qualified_column_p95_ns`: complete `SELECT u.| FROM t_00010 u`.
//! - `alias_join_p95_ns`: complete a column behind an alias of a join.
//! - `hover_p95_ns`: hover over a column with a foreign key.
//! - `diagnose_p95_ns`: diagnose a script of several statements.
//! - `worst_p95_ns`: the largest of these.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use sqler_catalog::CacheKey;
use sqler_sql::Dialect;
use sqler_ui::editor::intellisense::{
    CatalogSchema, bootstrap_keys, complete_popup, hover_markdown, load, prefetch_keys,
    synthetic::synthetic_catalog,
};

struct Args {
    tables: usize,
    columns: usize,
    samples: usize,
    budget_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5_000,
        columns: 8,
        samples: 2_000,
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
        let size = usize::try_from(n).unwrap_or(usize::MAX);
        match flag.as_str() {
            "--tables" => a.tables = size,
            "--columns" => a.columns = size,
            "--samples" => a.samples = size,
            "--budget-p95-ns" => a.budget_p95_ns = n,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.tables < 20 || a.samples == 0 {
        return Err("need tables >= 20 and samples > 0".to_owned());
    }
    Ok(a)
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted.get(rank - 1).copied().unwrap_or(0)
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

/// A text with a `|` marking the cursor, split into the text and the cursor offset.
fn cursor(marked: &str) -> (String, usize) {
    let at = marked.find('|').unwrap_or(marked.len());
    (marked.replacen('|', "", 1), at)
}

const SCRIPT: &str = "SELECT a.id, a.c_1, b.c_2 FROM t_00010 a JOIN t_00011 b ON a.c_1 = b.id WHERE a.c_3 IS NOT NULL;\n\
SELECT id, c_1 FROM t_00012 WHERE c_2 = 'x' ORDER BY c_1;\n\
INSERT INTO t_00013 (id, c_1) VALUES (1, 'a');\n\
UPDATE t_00014 SET c_1 = 'b' WHERE id = 2;\n\
SELECT x.nope, y.id FROM t_00015 x, t_00016 y;\n";

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    let dialect = Dialect::Postgres;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .map_err(|e| (1, format!("runtime: {e}")))?;
    let (catalog, worker) = synthetic_catalog(a.tables, a.columns);
    rt.spawn(worker.run());
    rt.block_on(load(&catalog, bootstrap_keys()));

    let scenarios = [
        cursor("|"),
        cursor("SELECT * FROM t_04|"),
        cursor("SELECT u.| FROM t_00010 u"),
        cursor("SELECT a.c_1, b.| FROM t_00010 a JOIN t_00011 b ON a.c_1 = b.id"),
        cursor("SELECT u.c_|1 FROM t_00010 u"),
    ];
    // Load the columns of every table that the scenarios and the script name.
    let probe = CatalogSchema::new(catalog.snapshot(), dialect);
    let mut keys: Vec<CacheKey> = Vec::new();
    for (text, _) in &scenarios {
        keys.extend(prefetch_keys(text, dialect, &probe, 64));
    }
    keys.extend(prefetch_keys(SCRIPT, dialect, &probe, 64));
    if keys.is_empty() {
        return Err((1, "setup loaded no tables".to_owned()));
    }
    rt.block_on(load(&catalog, keys));

    let popup = |index: usize| {
        let (text, at) = &scenarios[index];
        sample(a.samples, || {
            let schema = CatalogSchema::new(catalog.snapshot(), dialect);
            black_box(complete_popup(black_box(text), *at, dialect, &schema));
        })
    };
    let start = popup(0);
    let from = popup(1);
    let qualified = popup(2);
    let alias = popup(3);

    let (htext, hat) = &scenarios[4];
    let hover = sample(a.samples, || {
        let schema = CatalogSchema::new(catalog.snapshot(), dialect);
        black_box(hover_markdown(black_box(htext), *hat, dialect, &schema));
    });
    let diagnose = sample(a.samples, || {
        let schema = CatalogSchema::new(catalog.snapshot(), dialect);
        black_box(sqler_completion::diagnose(
            black_box(SCRIPT),
            dialect,
            &schema,
        ));
    });

    // Sanity: the scenarios must produce results, or the numbers mean nothing.
    let schema = CatalogSchema::new(catalog.snapshot(), dialect);
    let (qtext, qat) = &scenarios[2];
    let check = complete_popup(qtext, *qat, dialect, &schema);
    if !check.items.iter().any(|i| i.label == "c_1") || !check.loads.is_empty() {
        return Err((
            1,
            "the qualified scenario did not list the loaded columns".to_owned(),
        ));
    }
    if hover_markdown(htext, *hat, dialect, &schema).is_none() {
        return Err((1, "the hover scenario found nothing".to_owned()));
    }

    let metrics = [
        ("statement_start", &start),
        ("from_prefix", &from),
        ("qualified_column", &qualified),
        ("alias_join", &alias),
        ("hover", &hover),
        ("diagnose", &diagnose),
    ];
    let worst = metrics
        .iter()
        .map(|(_, v)| percentile(v, 95))
        .max()
        .unwrap_or(0);
    print!(
        "bench=editor_intellisense\nos={}-{}\ntables={}\ncolumns={}\nsamples={}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.tables,
        a.columns,
        a.samples,
    );
    for (name, v) in metrics {
        println!("{name}_p50_ns={}", percentile(v, 50));
        println!("{name}_p95_ns={}", percentile(v, 95));
    }
    println!("worst_p95_ns={worst}");

    if worst > a.budget_p95_ns {
        eprintln!(
            "FAIL: worst p95 {worst} ns exceeds the budget of {} ns",
            a.budget_p95_ns
        );
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
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
