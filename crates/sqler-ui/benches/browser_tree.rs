// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the object tree model on a schema with thousands of tables.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! `expand_p95_ns` (minimum over `--reps` repetitions, plus an absolute `--slack-ns`) against a saved baseline. Exit codes: 0 ok, 1 budget or regression failure, 2
//! bad arguments or unreadable baseline. See `docs/src/benchmarks.md` for the method and the rule.
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
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    budget_expand_p95_ns: u64,
    budget_select_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5000,
        samples: 200,
        select_samples: 20_000,
        reps: 3,
        save: None,
        compare: None,
        threshold_pct: 25,
        slack_ns: 250_000,
        budget_expand_p95_ns: 16_000_000,
        budget_select_p95_ns: 1_000_000,
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
            "--samples" => a.samples = to_usize(num(value()?)?),
            "--select-samples" => a.select_samples = to_usize(num(value()?)?),
            "--reps" => a.reps = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
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

fn base_p95(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("expand_p95_ns="))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("no expand_p95_ns in {path}"))
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
    for _ in 0..a.samples {
        let mut f = fixture(a.tables).map_err(|e| (1, e))?;
        let start = Instant::now();
        expand_schema(&mut f).map_err(|e| (1, e))?;
        black_box(f.tree.rows());
        expand_ns.push(nanos(start));
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
        folder_ns.push(nanos(start));
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
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(base_p95(path).map_err(|e| (2, e))?),
        None => None,
    };

    // The gate uses the minimum of each metric over the repetitions: noise on a shared runner only
    // adds time, so the fastest repetition is the closest to the real cost.
    let mut best = measure(&a)?;
    for _ in 1..a.reps {
        let s = measure(&a)?;
        best.expand_p50 = best.expand_p50.min(s.expand_p50);
        best.expand_p95 = best.expand_p95.min(s.expand_p95);
        best.folder_p95 = best.folder_p95.min(s.folder_p95);
        best.select_p50 = best.select_p50.min(s.select_p50);
        best.select_p95 = best.select_p95.min(s.select_p95);
    }
    let Stats {
        rows,
        expand_p50,
        expand_p95,
        folder_p95,
        select_p50,
        select_p95,
    } = best;

    let report = format!(
        "bench=browser_tree\nos={}-{}\ntables={}\nrows={rows}\nsamples={}\nreps={}\nselect_samples={}\n\
         expand_p50_ns={expand_p50}\nexpand_p95_ns={expand_p95}\nfolder_p95_ns={folder_p95}\n\
         select_p50_ns={select_p50}\nselect_p95_ns={select_p95}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.tables,
        a.samples,
        a.reps,
        a.select_samples,
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
        let limit =
            u128::from(base) * (u128::from(a.threshold_pct) + 100) / 100 + u128::from(a.slack_ns);
        if u128::from(expand_p95) > limit {
            eprintln!(
                "FAIL: expand p95 {expand_p95} ns regressed against baseline {base} ns (limit {limit} ns)"
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
