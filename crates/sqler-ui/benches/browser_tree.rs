// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the object tree model on a schema with thousands of tables.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! `expand_p50_ns` (minimum over `--reps` repetitions, and over every `--compare` file on the
//! baseline side) against saved baselines with a relative `--threshold-pct`, an absolute
//! `--slack-ns` and a `--floor-ns`. `expand_p95_ns` is informational (a warning, never a failure)
//! apart from the absolute budget. `--pr FILE` (repeatable) judges previously saved runs instead of
//! measuring, which is how the workflow interleaves base and PR rounds. `--simulate-slowdown` makes
//! the timed expand do its work twice (a test-only 2x regression). Exit codes: 0 ok, 1 budget or
//! regression failure, 2 bad arguments or unreadable baseline. See `docs/src/benchmarks.md` for the
//! method and the rule.
//!
//! Metrics:
//! - `expand_p95_ns`: expand a schema, apply a listing of N tables and rebuild the rows.
//! - `folder_p95_ns`: then show the N tables of the folder (the rows are rebuilt again).
//! - `select_p95_ns`: `select_next` plus a read of the rows, with the N tables visible.

use std::hint::black_box;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use sqler_catalog::{CacheKey, CatalogError, NodeKey};
use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, ObjectPath};
use sqler_ui::browser::{ObjectTree, TreeNodeId};

struct Args {
    tables: usize,
    samples: usize,
    select_samples: usize,
    reps: usize,
    warmup: usize,
    save: Option<String>,
    compare: Vec<String>,
    pr: Vec<String>,
    simulate_slowdown: bool,
    threshold_pct: u64,
    slack_ns: u64,
    floor_ns: u64,
    budget_expand_p95_ns: u64,
    budget_select_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5000,
        samples: 200,
        select_samples: 20_000,
        reps: 3,
        warmup: 30,
        save: None,
        compare: Vec::new(),
        pr: Vec::new(),
        simulate_slowdown: false,
        threshold_pct: 25,
        slack_ns: 250_000,
        floor_ns: 1_000_000,
        budget_expand_p95_ns: 16_000_000,
        budget_select_p95_ns: 1_000_000,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--bench" {
            continue;
        }
        if flag == "--simulate-slowdown" {
            a.simulate_slowdown = true;
            continue;
        }
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let num = |s: String| {
            s.parse::<u64>()
                .map_err(|_| format!("bad number for {flag}: {s}"))
        };
        match flag.as_str() {
            "--tables" => a.tables = to_usize(num(value()?)?),
            "--samples" => a.samples = to_usize(num(value()?)?),
            "--select-samples" => a.select_samples = to_usize(num(value()?)?),
            "--reps" => a.reps = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--warmup" => a.warmup = to_usize(num(value()?)?),
            "--compare" => a.compare.push(value()?),
            "--pr" => a.pr.push(value()?),
            "--floor-ns" => a.floor_ns = num(value()?)?,
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--budget-expand-p95-ns" => a.budget_expand_p95_ns = num(value()?)?,
            "--budget-select-p95-ns" => a.budget_select_p95_ns = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.select_samples == 0 || a.reps == 0 || a.tables == 0 {
        return Err("need tables > 0, samples > 0, select-samples > 0 and reps > 0".to_owned());
    }
    Ok(a)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// Nearest-rank percentile of a sorted slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn nanos(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn field(text: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .and_then(|v| v.trim().parse().ok())
}

/// Reads a saved report; `expand_p50_ns` and `expand_p95_ns` are required, the rest default to 0.
fn read_stats(path: &str) -> Result<Stats, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let need = |key: &str| field(&text, key).ok_or_else(|| format!("no {key} in {path}"));
    Ok(Stats {
        tables: to_usize(field(&text, "tables").unwrap_or(0)),
        rows: to_usize(field(&text, "rows").unwrap_or(0)),
        expand_p50: need("expand_p50_ns")?,
        expand_p95: need("expand_p95_ns")?,
        folder_p95: field(&text, "folder_p95_ns").unwrap_or(0),
        select_p50: field(&text, "select_p50_ns").unwrap_or(0),
        select_p95: field(&text, "select_p95_ns").unwrap_or(0),
    })
}

/// Minimum of every metric over several runs: noise on a shared runner only adds time.
fn merge_min(all: Vec<Stats>) -> Option<Stats> {
    let mut it = all.into_iter();
    let mut best = it.next()?;
    for s in it {
        best.expand_p50 = best.expand_p50.min(s.expand_p50);
        best.expand_p95 = best.expand_p95.min(s.expand_p95);
        best.folder_p95 = best.folder_p95.min(s.folder_p95);
        best.select_p50 = best.select_p50.min(s.select_p50);
        best.select_p95 = best.select_p95.min(s.select_p95);
    }
    Some(best)
}

fn ok(label: &str, keys: &[CacheKey], expected: usize) -> Result<(), String> {
    if keys.len() == expected {
        Ok(())
    } else {
        Err(format!(
            "{label}: expected {expected} keys, got {}",
            keys.len()
        ))
    }
}

/// A tree with a loaded schema node that is not expanded yet, plus the listing of its tables.
struct Fixture {
    tree: ObjectTree,
    schema: TreeNodeId,
    tables_key: NodeKey,
    listing: Arc<Listing>,
}

fn fixture(tables: usize) -> Result<Fixture, String> {
    let mut tree = ObjectTree::new();
    let root = tree.root();
    let path = ObjectPath::root();
    ok("root", &tree.expand(root), 2)?;
    // A SQLite-like source: databases are unsupported, so the schemas come at the root.
    let follow = tree.children_loaded(
        &NodeKey::new(path.clone(), ObjectKind::Database),
        Ok(Arc::new(Listing::Unsupported)),
    );
    ok("fall-through", &follow, 2)?;
    let schema_obj = DbObject::new(ObjectKind::Schema, path.clone(), "main");
    let _ = tree.children_loaded(
        &NodeKey::new(path.clone(), ObjectKind::Schema),
        Ok(Arc::new(Listing::Objects(vec![schema_obj.clone()]))),
    );
    let schemas = tree
        .rows()
        .first()
        .map(|r| r.id)
        .ok_or("no schemas folder")?;
    tree.expand(schemas);
    let schema = tree.rows().get(1).map(|r| r.id).ok_or("no schema row")?;
    let table_path = schema_obj.child_path();
    let objects = (0..tables)
        .map(|i| {
            DbObject::new(
                ObjectKind::Table,
                table_path.clone(),
                format!("table_{i:05}"),
            )
        })
        .collect();
    Ok(Fixture {
        tree,
        schema,
        tables_key: NodeKey::new(table_path, ObjectKind::Table),
        listing: Arc::new(Listing::Objects(objects)),
    })
}

type Answer = Result<Arc<Listing>, CatalogError>;

/// Expands the schema and answers its table listing (the other kinds are unsupported).
fn expand_schema(f: &mut Fixture) -> Result<(), String> {
    let keys = f.tree.expand(f.schema);
    ok("schema", &keys, 7)?;
    let table = f
        .tree
        .children_loaded(&f.tables_key, Ok(Arc::clone(&f.listing)) as Answer);
    ok("tables", &table, 0)?;
    for key in keys {
        if let CacheKey::Children(k) = key
            && k != f.tables_key
        {
            f.tree
                .children_loaded(&k, Ok(Arc::new(Listing::Unsupported)));
        }
    }
    Ok(())
}

/// Percentiles of one repetition.
struct Stats {
    tables: usize,
    rows: usize,
    expand_p50: u64,
    expand_p95: u64,
    folder_p95: u64,
    select_p50: u64,
    select_p95: u64,
}

fn measure(a: &Args) -> Result<Stats, (u8, String)> {
    let mut expand_ns = Vec::with_capacity(a.samples);
    let mut folder_ns = Vec::with_capacity(a.samples);
    // The first iterations pay for cold caches and allocator growth; they are not recorded.
    for i in 0..a.warmup + a.samples {
        let mut f = fixture(a.tables).map_err(|e| (1, e))?;
        let mut twin = if a.simulate_slowdown {
            Some(fixture(a.tables).map_err(|e| (1, e))?)
        } else {
            None
        };
        let start = Instant::now();
        expand_schema(&mut f).map_err(|e| (1, e))?;
        if let Some(t) = twin.as_mut() {
            expand_schema(t).map_err(|e| (1, e))?;
            black_box(t.tree.rows());
        }
        black_box(f.tree.rows());
        let took = nanos(start);
        if i >= a.warmup {
            expand_ns.push(took);
        }
        let folder = f
            .tree
            .rows()
            .iter()
            .find(|r| r.label.starts_with("Tables"))
            .map(|r| r.id)
            .ok_or((1, "no tables folder".to_owned()))?;
        let start = Instant::now();
        f.tree.expand(folder);
        black_box(f.tree.rows());
        if i >= a.warmup {
            folder_ns.push(nanos(start));
        }
    }

    let mut f = fixture(a.tables).map_err(|e| (1, e))?;
    expand_schema(&mut f).map_err(|e| (1, e))?;
    let folder = f
        .tree
        .rows()
        .iter()
        .find(|r| r.label.starts_with("Tables"))
        .map(|r| r.id)
        .ok_or((1, "no tables folder".to_owned()))?;
    f.tree.expand(folder);
    let rows = f.tree.rows().len();
    let mut select_ns = Vec::with_capacity(a.select_samples);
    for i in 0..a.select_samples {
        if i % rows == 0 {
            f.tree.select(f.tree.root());
        }
        let start = Instant::now();
        f.tree.select_next();
        black_box(f.tree.rows().len());
        black_box(f.tree.selected());
        select_ns.push(nanos(start));
    }

    for v in [&mut expand_ns, &mut folder_ns, &mut select_ns] {
        v.sort_unstable();
    }
    let expand_p50 = percentile(&expand_ns, 50);
    let expand_p95 = percentile(&expand_ns, 95);
    let folder_p95 = percentile(&folder_ns, 95);
    let select_p50 = percentile(&select_ns, 50);
    let select_p95 = percentile(&select_ns, 95);
    Ok(Stats {
        tables: a.tables,
        rows,
        expand_p50,
        expand_p95,
        folder_p95,
        select_p50,
        select_p95,
    })
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baselines are read first so a bad path fails before the long run.
    let baseline = merge_min(
        a.compare
            .iter()
            .map(|p| read_stats(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| (2, e))?,
    );

    // The gate uses the minimum of each metric over the repetitions (or over the `--pr` files):
    // noise on a shared runner only adds time, so the fastest repetition is the closest to the
    // real cost.
    let best = if a.pr.is_empty() {
        let mut runs = Vec::with_capacity(a.reps);
        for _ in 0..a.reps {
            runs.push(measure(&a)?);
        }
        merge_min(runs)
    } else {
        merge_min(
            a.pr.iter()
                .map(|p| read_stats(p))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| (2, e))?,
        )
    }
    .ok_or((2, "nothing to measure".to_owned()))?;
    let Stats {
        tables,
        rows,
        expand_p50,
        expand_p95,
        folder_p95,
        select_p50,
        select_p95,
    } = best;

    let shape = if a.pr.is_empty() {
        format!(
            "samples={}\nreps={}\nwarmup={}\nselect_samples={}\n",
            a.samples, a.reps, a.warmup, a.select_samples
        )
    } else {
        format!("mode=judge\nrounds={}\n", a.pr.len())
    };
    let base_lines = match (&baseline, a.pr.is_empty()) {
        (Some(b), false) => format!(
            "base_expand_p50_ns={}\nbase_expand_p95_ns={}\n",
            b.expand_p50, b.expand_p95
        ),
        _ => String::new(),
    };
    let report = format!(
        "bench=browser_tree\nos={}-{}\ntables={}\nrows={rows}\n{shape}\
         expand_p50_ns={expand_p50}\nexpand_p95_ns={expand_p95}\nfolder_p95_ns={folder_p95}\n\
         select_p50_ns={select_p50}\nselect_p95_ns={select_p95}\n{base_lines}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        tables,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut pass = true;
    if expand_p95 > a.budget_expand_p95_ns {
        eprintln!(
            "FAIL: expand p95 {expand_p95} ns exceeds the budget of {} ns",
            a.budget_expand_p95_ns
        );
        pass = false;
    }
    if select_p95 > a.budget_select_p95_ns {
        eprintln!(
            "FAIL: select p95 {select_p95} ns exceeds the budget of {} ns",
            a.budget_select_p95_ns
        );
        pass = false;
    }
    if let Some(base) = baseline {
        let limit = (u128::from(base.expand_p50) * (u128::from(a.threshold_pct) + 100) / 100
            + u128::from(a.slack_ns))
        .max(u128::from(a.floor_ns));
        if u128::from(expand_p50) > limit {
            eprintln!(
                "FAIL: expand p50 {expand_p50} ns regressed against baseline {} ns (limit {limit} ns)",
                base.expand_p50
            );
            pass = false;
        }
        // p95 of a few hundred samples is dominated by single scheduler hiccups: informational.
        let p95_note = u128::from(base.expand_p95) * 2 + 2 * u128::from(a.slack_ns);
        if u128::from(expand_p95) > p95_note {
            eprintln!(
                "WARN: expand p95 {expand_p95} ns is above {p95_note} ns (baseline {} ns); informational, not gated",
                base.expand_p95
            );
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
