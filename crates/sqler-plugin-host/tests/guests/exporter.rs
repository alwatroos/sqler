// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The export scenarios against the real guest exporter `plugins/fixtures/exporter` (plugin id `exporter-fixture`,
//! format `plugin-exporter-fixture:tsv`), through `Registry::with_sources` and the `Exporter` only.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqler_export::{
    CancelToken, ExportControl, ExportError, ExportOptions, Exporter, FormatSource, Registry,
    Summary,
};
use sqler_plugin_host::{PluginDirs, PluginExporters, PluginHost, discover};

use super::common::{Grants, Scratch, host, test_limits};

const FORMAT: &str = "plugin-exporter-fixture:tsv";
const LABEL: &str = "Exporter fixture (plugin): Tab-separated";

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

/// The staged exporter fixture, or `None` (after saying why) when the test must be skipped.
fn staged(test: &str) -> Option<PathBuf> {
    let fixtures = super::fixtures(test)?;
    let dir = fixtures.join("exporter-fixture");
    if dir.join("exporter.wasm").is_file() {
        return Some(dir);
    }
    assert!(
        std::env::var("SQLER_TEST_REQUIRE_PLUGINS").map_or(true, |v| v != "1"),
        "{test}: the exporter fixture is not built (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!("skipped: {test}: the exporter fixture is not staged");
    None
}

fn rig(test: &str) -> Option<Rig> {
    let staged = staged(test)?;
    let scratch = Scratch::new(&format!("exporter-{test}"));
    let dir = scratch.path().join("exporter-fixture");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "exporter.wasm"] {
        fs::copy(staged.join(file), dir.join(file)).unwrap();
    }
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
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

impl Rig {
    fn exporter(&self, sink: &Shared, control: ExportControl) -> Exporter<'static> {
        let format = self.registry.get(FORMAT).unwrap();
        Exporter::new(
            format,
            &ExportOptions::default(),
            Box::new(sink.clone()),
            control,
        )
        .unwrap()
    }
}

fn columns() -> Vec<Column> {
    vec![
        Column::new("id", DataType::Integer),
        Column::new("name", DataType::Text),
        Column::new("note", DataType::Text),
    ]
}

fn row(n: i64) -> Row {
    Row(vec![
        Value::Int(n),
        Value::Text(format!("row {n}")),
        Value::Null,
    ])
}

fn rows(range: std::ops::Range<i64>) -> Vec<Row> {
    range.map(row).collect()
}

fn export_all(r: &Rig, count: i64) -> (Shared, Summary) {
    let sink = Shared::default();
    let mut exporter = r.exporter(&sink, ExportControl::default());
    exporter.begin(&columns()).unwrap();
    // Several batches, as the grid hands them over.
    let mut from = 0;
    while from < count {
        let to = (from + 2_500).min(count);
        exporter.write_batch(&rows(from..to)).unwrap();
        from = to;
    }
    let summary = exporter.finish().unwrap();
    (sink, summary)
}

fn plugin_text(e: &ExportError) -> &str {
    match e {
        ExportError::Plugin(t) => t,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_format_is_listed_with_the_manifest_label_and_extension() {
    let Some(r) = rig("listing") else { return };
    let format = r.registry.get(FORMAT).unwrap();
    assert_eq!(format.label(), LABEL);
    assert_eq!(format.extension(), "tsv");
    r.host.shutdown();
}

#[test]
fn ten_thousand_rows_give_exactly_the_expected_tsv() {
    let Some(r) = rig("rows") else { return };
    let (sink, summary) = export_all(&r, 10_000);
    let mut want = String::new();
    for n in 0..10_000 {
        let _ = writeln!(want, "{n}\trow {n}\t");
    }
    want.push_str("# end\n");
    let got = sink.text();
    assert_eq!(got.len(), want.len());
    assert!(got == want, "the TSV differs");
    assert_eq!(summary.rows, 10_000);
    assert_eq!(summary.bytes, want.len() as u64);
    r.host.shutdown();
}

#[test]
fn a_cancel_after_the_first_batch_is_cancelled_and_the_next_export_works() {
    let Some(r) = rig("cancel") else { return };
    let token = CancelToken::new();
    let seen = Arc::new(AtomicBool::new(false));
    let control = ExportControl {
        cancel: token.clone(),
        progress: Some(Box::new({
            let (token, seen) = (token.clone(), Arc::clone(&seen));
            move |p| {
                if p.rows >= 1_024 {
                    seen.store(true, Ordering::SeqCst);
                    token.cancel();
                }
            }
        })),
    };
    let sink = Shared::default();
    let mut exporter = r.exporter(&sink, control);
    exporter.begin(&columns()).unwrap();
    let err = exporter.write_batch(&rows(0..3_000)).unwrap_err();
    assert!(matches!(err, ExportError::Cancelled), "{err:?}");
    assert!(seen.load(Ordering::SeqCst));
    // Dropping the exporter frees the guest writer without waiting.
    drop(exporter);
    assert!(!sink.text().contains("# end"));

    let (again, summary) = export_all(&r, 100);
    assert_eq!(summary.rows, 100);
    assert!(again.text().ends_with("99\trow 99\t\n# end\n"));
    r.host.shutdown();
}

#[test]
fn a_trap_in_the_second_batch_is_a_fixed_text_and_the_trailer_is_not_written() {
    let Some(r) = rig("trap") else { return };
    let sink = Shared::default();
    let mut exporter = r.exporter(&sink, ExportControl::default());
    exporter.begin(&columns()).unwrap();
    exporter.write_batch(&rows(0..10)).unwrap();
    let bad = vec![Row(vec![
        Value::Int(1),
        Value::Text("!trap".into()),
        Value::Null,
    ])];
    let err = exporter.write_batch(&bad).unwrap_err();
    assert_eq!(
        plugin_text(&err),
        format!("the export format `{LABEL}` of plugin `exporter-fixture` crashed")
    );
    // The instance is gone: nothing else runs against it.
    let after = exporter.write_batch(&rows(0..1)).unwrap_err();
    assert!(plugin_text(&after).contains("stopped"), "{after:?}");
    drop(exporter);
    assert!(!sink.text().contains("# end"));
    // The next export instantiates the plugin again.
    let (_, summary) = export_all(&r, 5);
    assert_eq!(summary.rows, 5);
    r.host.shutdown();
}

#[test]
fn a_guest_error_is_one_line_without_bidi_marks() {
    let Some(r) = rig("fail") else { return };
    let sink = Shared::default();
    let mut exporter = r.exporter(&sink, ExportControl::default());
    exporter.begin(&columns()).unwrap();
    let bad = vec![Row(vec![
        Value::Int(1),
        Value::Text("!fail".into()),
        Value::Null,
    ])];
    let err = exporter.write_batch(&bad).unwrap_err();
    let text = plugin_text(&err);
    assert_eq!(
        text,
        format!("the export format `{LABEL}` of plugin `exporter-fixture` failed: bad rowhere")
    );
    assert!(!text.contains(['\n', '\u{202E}']));
    drop(exporter);
    assert!(!sink.text().contains("# end"));
    r.host.shutdown();
}
