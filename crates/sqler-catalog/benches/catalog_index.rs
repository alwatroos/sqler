// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the catalog snapshot and name index on a synthetic schema with thousands of tables.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! `prefix_p95_ns` against a saved baseline. Exit codes: 0 ok, 1 budget or regression failure, 2
//! bad arguments or unreadable baseline. See `docs/src/benchmarks.md` for the method and the rule.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

use sqler_catalog::{
    Catalog, CatalogError, IntrospectSource, NameQuery, NodeKey, RefreshEvent, RefreshHandle,
    RefreshOutcome, RefreshScope,
};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, Listing, ObjectKind, ObjectPath, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};

struct Args {
    tables: usize,
    columns: usize,
    loaded: usize,
    samples: usize,
    lookup_samples: usize,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    budget_snapshot_p95_ns: u64,
    budget_prefix_p95_ns: u64,
    budget_fuzzy_p95_ns: u64,
    budget_index_bytes: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5000,
        columns: 8,
        loaded: 50,
        samples: 200,
        lookup_samples: 2000,
        save: None,
        compare: None,
        threshold_pct: 25,
        slack_ns: 50,
        budget_snapshot_p95_ns: 10_000,
        budget_prefix_p95_ns: 1_000_000,
        budget_fuzzy_p95_ns: 5_000_000,
        budget_index_bytes: 4_194_304,
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
            "--loaded" => a.loaded = to_usize(num(value()?)?),
            "--samples" => a.samples = to_usize(num(value()?)?),
            "--lookup-samples" => a.lookup_samples = to_usize(num(value()?)?),
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--budget-snapshot-p95-ns" => a.budget_snapshot_p95_ns = num(value()?)?,
            "--budget-prefix-p95-ns" => a.budget_prefix_p95_ns = num(value()?)?,
            "--budget-fuzzy-p95-ns" => a.budget_fuzzy_p95_ns = num(value()?)?,
            "--budget-index-bytes" => a.budget_index_bytes = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.lookup_samples == 0 || a.tables == 0 || a.columns == 0 {
        return Err("need samples, lookup-samples, tables and columns > 0".to_owned());
    }
    if a.loaded > a.tables {
        return Err("loaded must not exceed tables".to_owned());
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

fn main_path() -> ObjectPath {
    ObjectPath::root().in_schema("main")
}

fn listed(kind: ObjectKind) -> NodeKey {
    NodeKey::new(main_path(), kind)
}

/// Schema `main` with `tables` tables, `tables / 10` views and `tables / 10` functions.
struct Synthetic {
    tables: usize,
    columns: usize,
}

impl Introspect for Synthetic {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::Function
        )
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let named = |prefix: &str, n: usize| {
            Listing::Objects(
                (0..n)
                    .map(|i| DbObject::new(kind, main_path(), format!("{prefix}_{i:05}")))
                    .collect(),
            )
        };
        let listing = if parent.schema.as_deref() == Some("main") {
            match kind {
                ObjectKind::Table => named("table", self.tables),
                ObjectKind::View => named("view", self.tables / 10),
                ObjectKind::Function => named("func", self.tables / 10),
                _ => Listing::Unsupported,
            }
        } else {
            Listing::Objects(Vec::new())
        };
        Box::pin(std::future::ready(Ok(listing)))
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        let columns = (0..self.columns)
            .map(|i| ColumnInfo::new(format!("col_{i}"), DataType::Integer))
            .collect();
        let t = Table::new(table.clone()).with_columns(columns);
        Box::pin(std::future::ready(Ok(Some(t))))
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

fn err(e: &CatalogError) -> String {
    e.to_string()
}

async fn finish(handle: &mut RefreshHandle) -> Result<(), String> {
    while let Some(event) = handle.next_event().await {
        match event {
            RefreshEvent::Finished(RefreshOutcome::Applied { failed: 0, .. }) => return Ok(()),
            RefreshEvent::Finished(other) => return Err(format!("refresh ended with {other:?}")),
            _ => {}
        }
    }
    Err("refresh ended without Finished".to_owned())
}

struct Stats {
    snapshot: Vec<u64>,
    prefix: Vec<u64>,
    fuzzy: Vec<u64>,
    load_tables_ms: f64,
    publish_ms: f64,
    index_bytes: usize,
}

const PREFIX_NEEDLES: [&str; 4] = ["t", "table_0", "table_049", "col_"];
const FUZZY_NEEDLES: [&str; 2] = ["tbl49", "vw1"];

async fn measure(a: &Args, catalog: &Catalog) -> Result<Stats, String> {
    for kind in [ObjectKind::Table, ObjectKind::View, ObjectKind::Function] {
        catalog.children(listed(kind)).await.map_err(|e| err(&e))?;
    }
    let Some(listing) = catalog.cached(&listed(ObjectKind::Table)) else {
        return Err("tables listing missing".to_owned());
    };
    let Listing::Objects(all) = &*listing else {
        return Err("tables listing unsupported".to_owned());
    };
    if all.len() != a.tables {
        return Err(format!("expected {} tables, got {}", a.tables, all.len()));
    }
    let wanted: Vec<DbObject> = all.iter().take(a.loaded).cloned().collect();
    let start = Instant::now();
    let loaded = catalog.load_tables(wanted).await;
    let load_tables_ms = start.elapsed().as_secs_f64() * 1000.0;
    if loaded.iter().any(|(_, r)| !matches!(r, Ok(Some(_)))) {
        return Err("a table failed to load".to_owned());
    }

    let mut snapshot = Vec::with_capacity(a.lookup_samples);
    for _ in 0..a.lookup_samples {
        let start = Instant::now();
        let s = catalog.snapshot();
        let ns = nanos(start.elapsed());
        black_box(s);
        snapshot.push(ns);
    }

    let snap = catalog.snapshot();
    let mut prefix = Vec::with_capacity(a.lookup_samples);
    for i in 0..a.lookup_samples {
        let q = NameQuery::new(PREFIX_NEEDLES[i % PREFIX_NEEDLES.len()]);
        let start = Instant::now();
        let hits = snap.lookup(&q);
        let ns = nanos(start.elapsed());
        black_box(hits);
        prefix.push(ns);
    }
    let mut fuzzy = Vec::with_capacity(a.lookup_samples);
    for i in 0..a.lookup_samples {
        let q = NameQuery::new(FUZZY_NEEDLES[i % FUZZY_NEEDLES.len()]).fuzzy();
        let start = Instant::now();
        let hits = snap.lookup(&q);
        let ns = nanos(start.elapsed());
        black_box(hits);
        fuzzy.push(ns);
    }

    // Refresh everything cached with nothing changed: the publish reuses every entry.
    let mut publish = Vec::with_capacity(a.samples);
    for _ in 0..a.samples {
        let start = Instant::now();
        let mut handle = catalog.refresh(RefreshScope::All);
        finish(&mut handle).await?;
        publish.push(nanos(start.elapsed()));
    }
    publish.sort_unstable();
    #[allow(clippy::cast_precision_loss)] // a duration in ns is far below 2^52
    let publish_ms = percentile(&publish, 50) as f64 / 1_000_000.0;

    Ok(Stats {
        snapshot,
        prefix,
        fuzzy,
        load_tables_ms,
        publish_ms,
        index_bytes: snap.index_bytes(),
    })
}

fn base_prefix_p95(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("prefix_p95_ns="))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("no prefix_p95_ns in {path}"))
}

fn percentiles(name: &str, mut v: Vec<u64>) -> (String, u64) {
    v.sort_unstable();
    let (p50, p95) = (percentile(&v, 50), percentile(&v, 95));
    (format!("{name}_p50_ns={p50}\n{name}_p95_ns={p95}\n"), p95)
}

fn check_budget(ok: &mut bool, what: &str, value: u64, budget: u64, unit: &str) {
    if value > budget {
        eprintln!("FAIL: {what} {value} {unit} exceeds the budget of {budget} {unit}");
        *ok = false;
    }
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(base_prefix_p95(path).map_err(|e| (2, e))?),
        None => None,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| (1, e.to_string()))?;
    let stats = rt
        .block_on(async {
            let source = IntrospectSource::Introspect(Box::new(Synthetic {
                tables: a.tables,
                columns: a.columns,
            }));
            let (catalog, worker) = Catalog::new(source);
            let join = tokio::spawn(worker.run());
            let result = measure(&a, &catalog).await;
            catalog.shutdown();
            join.await.map_err(|e| e.to_string())?;
            result
        })
        .map_err(|e| (1, e))?;

    let (snapshot, snapshot_p95) = percentiles("snapshot", stats.snapshot);
    let (prefix, prefix_p95) = percentiles("prefix", stats.prefix);
    let (fuzzy, fuzzy_p95) = percentiles("fuzzy", stats.fuzzy);
    let report = format!(
        "bench=catalog_index\nos={}-{}\ntables={}\ncolumns={}\nloaded={}\nsamples={}\nlookup_samples={}\n\
         {snapshot}{prefix}{fuzzy}load_tables_ms={:.3}\npublish_ms={:.3}\nindex_bytes={}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        a.tables,
        a.columns,
        a.loaded,
        a.samples,
        a.lookup_samples,
        stats.load_tables_ms,
        stats.publish_ms,
        stats.index_bytes,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut ok = true;
    check_budget(
        &mut ok,
        "snapshot p95",
        snapshot_p95,
        a.budget_snapshot_p95_ns,
        "ns",
    );
    check_budget(
        &mut ok,
        "prefix p95",
        prefix_p95,
        a.budget_prefix_p95_ns,
        "ns",
    );
    check_budget(&mut ok, "fuzzy p95", fuzzy_p95, a.budget_fuzzy_p95_ns, "ns");
    check_budget(
        &mut ok,
        "index size",
        u64::try_from(stats.index_bytes).unwrap_or(u64::MAX),
        a.budget_index_bytes,
        "bytes",
    );
    if let Some(base) = baseline {
        let limit =
            u128::from(base) * (u128::from(a.threshold_pct) + 100) / 100 + u128::from(a.slack_ns);
        if u128::from(prefix_p95) > limit {
            eprintln!(
                "FAIL: prefix p95 {prefix_p95} ns regressed against baseline {base} ns (limit {limit} ns)"
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
