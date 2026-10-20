// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Benchmark of the catalog cache on a synthetic schema with thousands of tables.
//!
//! Std-only (`harness = false`). Prints `key=value` lines, optionally saves them, and can compare
//! `hit_p95_ns` against a saved baseline. Exit codes: 0 ok, 1 budget or regression failure, 2 bad
//! arguments or unreadable baseline. See `docs/src/benchmarks.md` for the method and the rule.

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sqler_catalog::{
    CacheKey, Catalog, CatalogError, IntrospectSource, NodeKey, RefreshEvent, RefreshOutcome,
    RefreshScope,
};
use sqler_driver_api::sqler_core::{
    ColumnInfo, ConnectionConfig, DataType, DbObject, DriverId, Listing, ObjectKind, ObjectPath,
    Query, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, DriverRegistry, Introspect, Session, collect};
use sqler_driver_sqlite::{DRIVER_ID, SqliteDriver};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Fake,
    Sqlite,
}

struct Args {
    tables: usize,
    columns: usize,
    samples: usize,
    hit_samples: usize,
    source: SourceKind,
    save: Option<String>,
    compare: Option<String>,
    threshold_pct: u64,
    slack_ns: u64,
    budget_hit_p95_ns: u64,
    budget_expand_p95_ns: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        tables: 5000,
        columns: 8,
        samples: 200,
        hit_samples: 100_000,
        source: SourceKind::Fake,
        save: None,
        compare: None,
        threshold_pct: 25,
        slack_ns: 150,
        budget_hit_p95_ns: 10_000,
        budget_expand_p95_ns: 16_000_000,
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
            "--hit-samples" => a.hit_samples = to_usize(num(value()?)?),
            "--source" => {
                a.source = match value()?.as_str() {
                    "fake" => SourceKind::Fake,
                    "sqlite" => SourceKind::Sqlite,
                    other => return Err(format!("unknown source: {other} (fake or sqlite)")),
                };
            }
            "--save" => a.save = Some(value()?),
            "--compare" => a.compare = Some(value()?),
            "--threshold-pct" => a.threshold_pct = num(value()?)?,
            "--slack-ns" => a.slack_ns = num(value()?)?,
            "--budget-hit-p95-ns" => a.budget_hit_p95_ns = num(value()?)?,
            "--budget-expand-p95-ns" => a.budget_expand_p95_ns = num(value()?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if a.samples == 0 || a.hit_samples == 0 || a.tables == 0 || a.columns == 0 {
        return Err("need samples, hit-samples, tables and columns > 0".to_owned());
    }
    Ok(a)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn nanos(d: Duration) -> u64 {
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

fn schemas_key() -> NodeKey {
    NodeKey::new(ObjectPath::root(), ObjectKind::Schema)
}

fn tables_key() -> NodeKey {
    NodeKey::new(main_path(), ObjectKind::Table)
}

/// One schema `main` with `tables` tables; every call returns a ready future.
struct Synthetic {
    tables: usize,
    columns: usize,
}

impl Introspect for Synthetic {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(kind, ObjectKind::Schema | ObjectKind::Table)
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let listing = match kind {
            ObjectKind::Schema => Listing::Objects(vec![DbObject::new(
                ObjectKind::Schema,
                ObjectPath::root(),
                "main",
            )]),
            ObjectKind::Table if parent.schema.as_deref() == Some("main") => Listing::Objects(
                (0..self.tables)
                    .map(|i| DbObject::new(ObjectKind::Table, main_path(), format!("table_{i:05}")))
                    .collect(),
            ),
            ObjectKind::Table => Listing::Objects(Vec::new()),
            _ => Listing::Unsupported,
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

async fn build_sqlite(tables: usize, columns: usize) -> Result<IntrospectSource, String> {
    let mut registry = DriverRegistry::new();
    registry
        .register(Box::new(SqliteDriver::new()))
        .map_err(|e| e.to_string())?;
    let id = DriverId::new(DRIVER_ID).map_err(|e| e.to_string())?;
    let mut session: Box<dyn Session> = registry
        .connect(&ConnectionConfig::new("bench", id, ":memory:"))
        .await
        .map_err(|e| e.to_string())?;
    let cols: Vec<String> = (0..columns).map(|c| format!("col_{c} INTEGER")).collect();
    let mut sql = vec!["BEGIN".to_owned()];
    sql.extend((0..tables).map(|t| format!("CREATE TABLE table_{t:05} ({})", cols.join(", "))));
    sql.push("COMMIT".to_owned());
    for text in sql {
        let query = Query::new(text);
        let mut stream = session.execute(&query).await.map_err(|e| e.to_string())?;
        collect(&mut *stream).await.map_err(|e| e.to_string())?;
    }
    Ok(IntrospectSource::Session(session))
}

/// Runs `body` against a fresh catalog on `source`, then takes the source back.
async fn with_catalog<T, F, Fut>(
    source: IntrospectSource,
    body: F,
) -> Result<(IntrospectSource, T), String>
where
    F: FnOnce(Catalog) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let (catalog, worker) = Catalog::new(source);
    let join = tokio::spawn(worker.run());
    let result = body(catalog.clone()).await;
    catalog.shutdown();
    let source = join.await.map_err(|e| e.to_string())?;
    result.map(|value| (source, value))
}

fn err(e: &CatalogError) -> String {
    e.to_string()
}

async fn finish(handle: &mut sqler_catalog::RefreshHandle) -> Result<(), String> {
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
    samples: usize,
    schemas: Vec<u64>,
    expand: Vec<u64>,
    hit: Vec<u64>,
    refresh_schema: Vec<u64>,
    refresh_all: Duration,
    refresh_all_nodes: usize,
}

async fn measure(a: &Args, mut source: IntrospectSource) -> Result<Stats, String> {
    let samples = if a.source == SourceKind::Sqlite {
        a.samples.min(20)
    } else {
        a.samples
    };
    let mut stats = Stats {
        samples,
        schemas: Vec::with_capacity(samples),
        expand: Vec::with_capacity(samples),
        hit: Vec::with_capacity(a.hit_samples),
        refresh_schema: Vec::with_capacity(samples),
        refresh_all: Duration::ZERO,
        refresh_all_nodes: 0,
    };

    for _ in 0..samples {
        let (next, ns) = with_catalog(source, |c| async move {
            let start = Instant::now();
            let listing = c.children(schemas_key()).await.map_err(|e| err(&e))?;
            let ns = nanos(start.elapsed());
            black_box(listing);
            Ok(ns)
        })
        .await?;
        source = next;
        stats.schemas.push(ns);
    }

    for _ in 0..samples {
        let (next, ns) = with_catalog(source, |c| async move {
            let start = Instant::now();
            let listing = c.children(tables_key()).await.map_err(|e| err(&e))?;
            let ns = nanos(start.elapsed());
            black_box(listing);
            Ok(ns)
        })
        .await?;
        source = next;
        stats.expand.push(ns);
    }

    // One catalog for the hit, refresh-schema and refresh-all figures.
    let (catalog, worker) = Catalog::new(source);
    let join = tokio::spawn(worker.run());
    let result = async {
        catalog.children(schemas_key()).await.map_err(|e| err(&e))?;
        let Listing::Objects(tables) =
            &*catalog.children(tables_key()).await.map_err(|e| err(&e))?
        else {
            return Err("tables listing unsupported".to_owned());
        };
        let tables = tables.clone();
        if tables.len() != a.tables {
            return Err(format!(
                "expected {} tables, got {}",
                a.tables,
                tables.len()
            ));
        }
        let key = tables_key();
        for _ in 0..a.hit_samples {
            let start = Instant::now();
            let hit = catalog.cached(&key);
            let ns = nanos(start.elapsed());
            black_box(hit);
            stats.hit.push(ns);
        }
        for _ in 0..samples {
            let start = Instant::now();
            let mut handle = catalog.refresh(RefreshScope::Subtree(main_path()));
            finish(&mut handle).await?;
            stats.refresh_schema.push(nanos(start.elapsed()));
        }
        for obj in tables {
            catalog.table(obj).await.map_err(|e| err(&e))?;
        }
        stats.refresh_all_nodes = catalog.cached_len();
        let start = Instant::now();
        let mut handle = catalog.refresh(RefreshScope::All);
        finish(&mut handle).await?;
        stats.refresh_all = start.elapsed();
        black_box(catalog.node_error(&CacheKey::Children(key)));
        Ok(())
    }
    .await;
    catalog.shutdown();
    join.await.map_err(|e| e.to_string())?;
    result.map(|()| stats)
}

fn base_hit_p95(path: &str) -> Result<u64, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("hit_p95_ns="))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("no hit_p95_ns in {path}"))
}

fn percentiles(name: &str, mut v: Vec<u64>) -> (String, u64) {
    v.sort_unstable();
    let (p50, p95) = (percentile(&v, 50), percentile(&v, 95));
    (format!("{name}_p50_ns={p50}\n{name}_p95_ns={p95}\n"), p95)
}

fn run() -> Result<ExitCode, (u8, String)> {
    let a = parse_args().map_err(|e| (2, e))?;
    // The baseline is read first so a bad path fails before the long run.
    let baseline = match &a.compare {
        Some(path) => Some(base_hit_p95(path).map_err(|e| (2, e))?),
        None => None,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| (1, e.to_string()))?;
    let stats = rt
        .block_on(async {
            let source = match a.source {
                SourceKind::Fake => IntrospectSource::Introspect(Box::new(Synthetic {
                    tables: a.tables,
                    columns: a.columns,
                })),
                SourceKind::Sqlite => build_sqlite(a.tables, a.columns).await?,
            };
            measure(&a, source).await
        })
        .map_err(|e| (1, e))?;

    let (schemas, _) = percentiles("schemas_cold", stats.schemas);
    let (expand, expand_p95) = percentiles("expand_cold", stats.expand);
    let (hit, hit_p95) = percentiles("hit", stats.hit);
    let (refresh_schema, _) = percentiles("refresh_schema", stats.refresh_schema);
    let report = format!(
        "bench=catalog_tree\nos={}-{}\nsource={}\ntables={}\ncolumns={}\nsamples={}\nhit_samples={}\n\
         {schemas}{expand}{hit}{refresh_schema}refresh_all_nodes={}\nrefresh_all_ms={:.3}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if a.source == SourceKind::Fake {
            "fake"
        } else {
            "sqlite"
        },
        a.tables,
        a.columns,
        stats.samples,
        a.hit_samples,
        stats.refresh_all_nodes,
        stats.refresh_all.as_secs_f64() * 1000.0,
    );
    print!("{report}");
    if let Some(path) = &a.save {
        std::fs::write(path, &report).map_err(|e| (2, format!("cannot write {path}: {e}")))?;
    }

    let mut ok = true;
    if a.source == SourceKind::Fake {
        if hit_p95 > a.budget_hit_p95_ns {
            eprintln!(
                "FAIL: hit p95 {hit_p95} ns exceeds the budget of {} ns",
                a.budget_hit_p95_ns
            );
            ok = false;
        }
        if expand_p95 > a.budget_expand_p95_ns {
            eprintln!(
                "FAIL: expand p95 {expand_p95} ns exceeds the budget of {} ns",
                a.budget_expand_p95_ns
            );
            ok = false;
        }
    }
    if let Some(base) = baseline {
        let limit =
            u128::from(base) * (u128::from(a.threshold_pct) + 100) / 100 + u128::from(a.slack_ns);
        if u128::from(hit_p95) > limit {
            eprintln!(
                "FAIL: hit p95 {hit_p95} ns regressed against baseline {base} ns (limit {limit} ns)"
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
