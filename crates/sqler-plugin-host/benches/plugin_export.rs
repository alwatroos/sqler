// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Throughput of a plugin export format on synthetic rows.
//!
//! Std-only (`harness = false`). Needs the staged fixture plugins: `SQLER_PLUGIN_FIXTURES` names the directory
//! (`target/plugins/<profile>`, a relative path is taken from the workspace root) that holds `exporter-fixture/`.
//! Streams `--rows N` (default 1,000,000) rows of five columns (integer, text, float, null, boolean) in batches of
//! `--batch N` (1,000) through the fixture's `tsv` format into a sink that discards the bytes, and prints
//! `plugin_tsv_rows_per_sec=` and `plugin_tsv_mib_per_sec=`. A `--bench` argument (added by `cargo bench`) is
//! ignored. Exit codes: 0 ok, 1 writer error, 2 usage error or missing fixture.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqler_export::{ExportControl, ExportOptions, Exporter, FormatSource, Registry};
use sqler_plugin_api::{CapabilitySet, GrantKey, Limits};
use sqler_plugin_host::{
    GrantSource, HostOptions, MemorySettings, NoEgress, PluginDirs, PluginExporters, PluginHost,
    Services, discover,
};

/// The format id of the fixture's only exporter.
const FORMAT: &str = "plugin-exporter-fixture:tsv";

/// Discards everything.
struct Discard;

impl Write for Discard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Approves what the manifests declare.
struct DeclaredGrants(Vec<(GrantKey, CapabilitySet)>);

impl GrantSource for DeclaredGrants {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, s)| s.clone())
    }
}

fn columns() -> Vec<Column> {
    vec![
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
        Column::new("score", DataType::Float),
        Column::new("note", DataType::Text),
        Column::new("flag", DataType::Boolean),
    ]
}

/// Fills `row` with the synthetic values of row number `n`.
fn fill(row: &mut Row, n: i64) {
    row.0.clear();
    row.0.push(Value::Int(n));
    row.0.push(Value::Text(format!("name {n}")));
    #[allow(clippy::cast_precision_loss)]
    row.0.push(Value::Float(n as f64 * 0.25));
    row.0.push(Value::Null);
    row.0.push(Value::Bool(n % 2 == 0));
}

/// The directory of the staged `exporter-fixture` plugin.
fn fixture_dir() -> Result<PathBuf, String> {
    let staged = std::env::var_os("SQLER_PLUGIN_FIXTURES")
        .map(PathBuf::from)
        .ok_or(
            "SQLER_PLUGIN_FIXTURES is not set (run `cargo run -p xtask -- plugins build` first)",
        )?;
    let staged = if staged.is_absolute() {
        staged
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(staged)
    };
    let dir = staged.join("exporter-fixture");
    if dir.join("exporter.wasm").is_file() && dir.join("sqler.toml").is_file() {
        Ok(dir)
    } else {
        Err("the exporter fixture is not staged (run `cargo run -p xtask -- plugins build`)".into())
    }
}

enum Failure {
    /// Usage or environment: exit 2.
    Setup(String),
    /// The writer failed: exit 1.
    Writer(String),
}

fn run(rows: i64, batch: usize) -> Result<(f64, f64), Failure> {
    let dir = fixture_dir().map_err(Failure::Setup)?;
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    if let Some(p) = d.problems.first() {
        return Err(Failure::Setup(format!("{}: {}", p.dir.display(), p.error)));
    }
    let grants = DeclaredGrants(
        d.plugins
            .iter()
            .map(|p| (p.key.clone(), p.manifest.capabilities().clone()))
            .collect(),
    );
    // A plugin export blocks its own thread, so the host runs on a runtime of its own.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .map_err(|e| Failure::Setup(e.to_string()))?;
    let host = PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants: Arc::new(grants),
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits: Limits::DEFAULT,
        services: Services::new(),
    });
    if let Some(p) = host.load(d).first() {
        return Err(Failure::Setup(format!("{}: {}", p.dir.display(), p.error)));
    }
    let source = PluginExporters::new();
    source.attach(&host);
    let registry = Registry::with_sources(&[source as Arc<dyn FormatSource>]);
    let format = registry
        .get(FORMAT)
        .ok_or_else(|| Failure::Setup(format!("the fixture offers no format {FORMAT}")))?;

    let writer = |e: sqler_export::ExportError| Failure::Writer(e.to_string());
    let mut exporter = Exporter::new(
        format,
        &ExportOptions::default(),
        Box::new(Discard),
        ExportControl::default(),
    )
    .map_err(writer)?;
    exporter.begin(&columns()).map_err(writer)?;
    let mut buffer: Vec<Row> = (0..batch).map(|_| Row(Vec::new())).collect();
    let start = Instant::now();
    let mut n = 0_i64;
    while n < rows {
        let len = usize::try_from((rows - n).min(i64::try_from(batch).unwrap_or(1))).unwrap_or(1);
        for row in &mut buffer[..len] {
            fill(row, n);
            n += 1;
        }
        exporter.write_batch(&buffer[..len]).map_err(writer)?;
    }
    let summary = exporter.finish().map_err(writer)?;
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    host.shutdown();
    #[allow(clippy::cast_precision_loss)]
    let (rps, mibps) = (
        summary.rows as f64 / secs,
        summary.bytes as f64 / secs / (1024.0 * 1024.0),
    );
    Ok((rps, mibps))
}

fn main() -> ExitCode {
    let mut rows = 1_000_000_i64;
    let mut batch = 1_000_usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => {}
            "--rows" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) if v > 0 => rows = v,
                _ => return usage(),
            },
            "--batch" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) if v > 0 => batch = v,
                _ => return usage(),
            },
            _ => return usage(),
        }
    }
    match run(rows, batch) {
        Ok((rps, mibps)) => {
            println!("plugin_tsv_rows_per_sec={rps:.0}");
            println!("plugin_tsv_mib_per_sec={mibps:.1}");
            ExitCode::SUCCESS
        }
        Err(Failure::Setup(e)) => {
            eprintln!("plugin_export: {e}");
            ExitCode::from(2)
        }
        Err(Failure::Writer(e)) => {
            eprintln!("plugin_export: {e}");
            ExitCode::from(1)
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: plugin_export [--rows N] [--batch N]   (needs SQLER_PLUGIN_FIXTURES)");
    ExitCode::from(2)
}
