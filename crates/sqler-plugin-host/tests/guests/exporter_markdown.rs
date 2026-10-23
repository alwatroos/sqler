// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The example plugin `plugins/exporter-markdown` (plugin id `exporter-markdown`, format
//! `plugin-exporter-markdown:markdown`), through `Registry::with_sources` and the `Exporter` only.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqler_export::{ExportControl, ExportOptions, Exporter, FormatSource, Registry, Summary};
use sqler_plugin_host::{PluginDirs, PluginExporters, PluginHost, discover};

use super::Approve;
use super::common::{Grants, Scratch, host, test_limits};

const FORMAT: &str = "plugin-exporter-markdown:markdown";
const LABEL: &str = "Exporter Markdown (plugin): Markdown table";

/// A sink that shares its bytes.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Rig {
    _scratch: Scratch,
    _rt: tokio::runtime::Runtime,
    host: PluginHost,
    registry: Registry,
}

/// The staged example, or `None` (after saying why) when the test must be skipped.
fn staged(test: &str) -> Option<PathBuf> {
    let fixtures = super::fixtures(test)?;
    let dir = fixtures.join("exporter-markdown");
    if dir.join("exporter-markdown.wasm").is_file() {
        return Some(dir);
    }
    assert!(
        std::env::var("SQLER_TEST_REQUIRE_PLUGINS").map_or(true, |v| v != "1"),
        "{test}: the exporter-markdown example is not built (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!("skipped: {test}: the exporter-markdown example is not staged");
    None
}

fn rig(test: &str, approve: &Approve) -> Option<Rig> {
    let staged = staged(test)?;
    let scratch = Scratch::new(&format!("exporter-markdown-{test}"));
    let dir = scratch.path().join("exporter-markdown");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "exporter-markdown.wasm"] {
        fs::copy(staged.join(file), dir.join(file)).unwrap();
    }
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Arc::new(Grants::default());
    match approve {
        Approve::Declared => grants.approve_declared(&d),
        Approve::Nothing => {}
        Approve::Only(list) => {
            let set: sqler_plugin_api::CapabilitySet = list
                .iter()
                .map(|c| c.parse::<sqler_plugin_api::Capability>().unwrap())
                .collect();
            grants.approve(&d.plugins[0].key, set);
        }
    }
    // The writer blocks its own thread, so the host needs a runtime that runs on other threads.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let host = host(&rt, grants, &test_limits(Duration::from_secs(30)));
    assert_eq!(host.load(d).len(), 0);
    let source = PluginExporters::new();
    source.attach(&host);
    let registry = Registry::with_sources(&[source as Arc<dyn FormatSource>]);
    Some(Rig {
        _scratch: scratch,
        _rt: rt,
        host,
        registry,
    })
}

fn columns() -> Vec<Column> {
    vec![
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
        Column::new("note", DataType::Text),
    ]
}

const HEADER: &str = "| id | name | note |\n| ---: | --- | --- |\n";

/// Exports `batches` of rows with the given columns.
fn export(r: &Rig, columns: &[Column], batches: &[Vec<Row>]) -> (Shared, Summary) {
    let format = r.registry.get(FORMAT).unwrap();
    let sink = Shared::default();
    let mut exporter = Exporter::new(
        format,
        &ExportOptions::default(),
        Box::new(sink.clone()),
        ExportControl::default(),
    )
    .unwrap();
    exporter.begin(columns).unwrap();
    for batch in batches {
        exporter.write_batch(batch).unwrap();
    }
    let summary = exporter.finish().unwrap();
    (sink, summary)
}

fn the_example_row() -> Row {
    Row(vec![
        Value::Int(1),
        Value::Text("a|b".to_owned()),
        Value::Null,
    ])
}

/// What the example export must give, whatever the grants are.
fn assert_example_export(r: &Rig) {
    let (sink, summary) = export(r, &columns(), &[vec![the_example_row()]]);
    assert_eq!(sink.text(), format!("{HEADER}| 1 | a\\|b |  |\n"));
    assert_eq!(summary.rows, 1);
}

#[test]
fn the_format_is_listed_next_to_the_built_in_one() {
    let Some(r) = rig("listing", &Approve::Declared) else {
        return;
    };
    let format = r.registry.get(FORMAT).unwrap();
    assert_eq!(format.label(), LABEL);
    assert_eq!(format.extension(), "md");
    assert_eq!(
        r.registry.get("markdown").unwrap().label(),
        "Markdown table"
    );
    r.host.shutdown();
}

#[test]
fn the_export_is_exactly_the_aligned_table() {
    let Some(r) = rig("exact", &Approve::Declared) else {
        return;
    };
    assert_example_export(&r);

    // Every cell kind, one float column.
    let cols = vec![
        Column::new("n", DataType::Integer),
        Column::new("x", DataType::Float),
        Column::new("flag", DataType::Boolean),
        Column::new("b", DataType::Binary),
        Column::new("t|\\", DataType::Text),
    ];
    let row = Row(vec![
        Value::Int(-7),
        Value::Float(2.5),
        Value::Bool(true),
        Value::Bytes(vec![0, 255, 16]),
        Value::Text("l1\r\nl2\nl3\rl4".to_owned()),
    ]);
    let (sink, _) = export(&r, &cols, &[vec![row]]);
    assert_eq!(
        sink.text(),
        "| n | x | flag | b | t\\|\\\\ |\n| ---: | ---: | --- | --- | --- |\n\
         | -7 | 2.5 | true | 0x00ff10 | l1<br>l2<br>l3<br>l4 |\n"
    );

    // 3,000 rows in two batches, byte for byte.
    let make = |range: std::ops::Range<i64>| -> Vec<Row> {
        range
            .map(|n| {
                Row(vec![
                    Value::Int(n),
                    Value::Text(format!("row {n}")),
                    Value::Null,
                ])
            })
            .collect()
    };
    let (sink, summary) = export(&r, &columns(), &[make(0..1_500), make(1_500..3_000)]);
    let mut want = String::from(HEADER);
    for n in 0..3_000 {
        let _ = writeln!(want, "| {n} | row {n} |  |");
    }
    let got = sink.text();
    assert_eq!(got.len(), want.len());
    assert!(got == want, "the table differs");
    assert_eq!(summary.rows, 3_000);
    assert_eq!(summary.bytes, want.len() as u64);

    // No rows: exactly the header.
    let (sink, summary) = export(&r, &columns(), &[]);
    assert_eq!(sink.text(), HEADER);
    assert_eq!(summary.rows, 0);
    assert_eq!(summary.bytes, HEADER.len() as u64);
    r.host.shutdown();
}

#[test]
fn an_export_needs_no_capability() {
    let Some(r) = rig("ungranted", &Approve::Nothing) else {
        return;
    };
    assert!(r.host.plugins()[0].effective.is_empty());
    assert_example_export(&r);
    r.host.shutdown();
}

#[test]
fn capabilities_the_manifest_does_not_declare_are_never_effective() {
    let Some(r) = rig("undeclared", &Approve::Only(&["fs.read:data", "db.read"])) else {
        return;
    };
    assert!(r.host.plugins()[0].effective.is_empty());
    assert_example_export(&r);
    r.host.shutdown();
}
