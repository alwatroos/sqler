// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin export formats without a built guest: the source and its listing rules, and a WAT guest that lacks the
//! `exporter` export. The writer blocks its own thread on the host's runtime, so these tests give the host a
//! multi-thread runtime and call the writer from the test thread.

#![cfg(feature = "cl")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqler_export::{
    ExportControl, ExportError, ExportOptions, Exporter, Format, FormatSource, Registry,
};
use sqler_plugin_api::PluginId;
use sqler_plugin_host::{PluginExporters, PluginHost};

use common::{Grants, Scratch, discover_all, host, test_limits, wat_component};

/// A sink that shares its bytes.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A plugin that declares `[[exporters]]` over the WAT component `ok`, which has no `exporter` export.
fn write_plugin(root: &Path, id: &str, name: &str, exporters: &[(&str, &str, &str)]) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    let mut manifest = format!(
        "[plugin]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = []\n\n[entry]\ncomponent = \"plugin.wasm\"\n"
    );
    for (eid, label, ext) in exporters {
        let _ = write!(
            manifest,
            "\n[[exporters]]\nid = \"{eid}\"\nlabel = \"{label}\"\nextension = \"{ext}\"\n"
        );
    }
    fs::write(dir.join("sqler.toml"), manifest).unwrap();
    fs::write(dir.join("plugin.wasm"), wat_component("ok")).unwrap();
    dir
}

struct Rig {
    _scratch: Scratch,
    _rt: tokio::runtime::Runtime,
    host: PluginHost,
    source: Arc<PluginExporters>,
}

/// A plugin: id, name and its exporters (id, label, extension).
type Spec<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str, &'a str)]);

fn rig(test: &str, plugins: &[Spec<'_>]) -> Rig {
    let scratch = Scratch::new(&format!("exporters-{test}"));
    let dirs: Vec<PathBuf> = plugins
        .iter()
        .map(|(id, name, exporters)| write_plugin(scratch.path(), id, name, exporters))
        .collect();
    let d = discover_all(&dirs);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let host = host(&rt, grants, &test_limits(Duration::from_secs(30)));
    assert_eq!(host.load(d).len(), 0);
    let source = PluginExporters::new();
    source.attach(&host);
    Rig {
        _scratch: scratch,
        _rt: rt,
        host,
        source,
    }
}

fn registry(r: &Rig) -> Registry {
    Registry::with_sources(&[Arc::clone(&r.source) as Arc<dyn FormatSource>])
}

fn builtin_ids() -> Vec<&'static str> {
    Registry::with_builtin().formats().map(Format::id).collect()
}

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

#[test]
fn without_a_host_there_are_no_formats() {
    let source = PluginExporters::new();
    assert!(source.formats().is_empty());
    let r = Registry::with_sources(&[source as Arc<dyn FormatSource>]);
    assert_eq!(
        r.formats().map(Format::id).collect::<Vec<_>>(),
        builtin_ids()
    );
}

#[test]
fn formats_come_after_the_builtins_with_the_plugin_form() {
    let r = rig(
        "listing",
        &[
            (
                "acme",
                "Acme",
                &[("tsv", "Tab-separated", "tsv"), ("psv", "Pipes", "psv")],
            ),
            ("plain", "Plain", &[]),
            ("beta", "Beta", &[("x", "X", "x")]),
        ],
    );
    let reg = registry(&r);
    let ids: Vec<_> = reg.formats().map(Format::id).collect();
    let builtin = builtin_ids();
    assert_eq!(&ids[..builtin.len()], &builtin[..]);
    assert_eq!(
        &ids[builtin.len()..],
        ["plugin-acme:tsv", "plugin-acme:psv", "plugin-beta:x"]
    );
    let tsv = reg.get("plugin-acme:tsv").unwrap();
    assert_eq!(tsv.label(), "Acme (plugin): Tab-separated");
    assert_eq!(tsv.extension(), "tsv");
    // The same strings are reused when the list is built again.
    let again = registry(&r);
    assert!(std::ptr::eq(
        again.get("plugin-acme:tsv").unwrap().id(),
        tsv.id()
    ));
    r.host.shutdown();
}

#[test]
fn a_disabled_plugin_is_not_listed_and_comes_back_when_enabled() {
    let r = rig(
        "disabled",
        &[
            ("acme", "Acme", &[("tsv", "Tab-separated", "tsv")]),
            ("beta", "Beta", &[("x", "X", "x")]),
        ],
    );
    r.host.disable(&id("acme"));
    let ids: Vec<_> = registry(&r).formats().map(Format::id).collect();
    assert!(!ids.contains(&"plugin-acme:tsv"));
    assert!(ids.contains(&"plugin-beta:x"));
    r.host.enable(&id("acme"));
    assert!(registry(&r).get("plugin-acme:tsv").is_some());
    r.host.shutdown();
}

#[test]
fn a_component_without_the_exporter_export_is_not_available_and_writes_nothing() {
    let r = rig(
        "no-export",
        &[("acme", "Acme", &[("tsv", "Tab-separated", "tsv")])],
    );
    let reg = registry(&r);
    let format = reg.get("plugin-acme:tsv").unwrap();
    let sink = Shared::default();
    let mut exporter = Exporter::new(
        format,
        &ExportOptions::default(),
        Box::new(sink.clone()),
        ExportControl::default(),
    )
    .unwrap();
    let err = exporter
        .begin(&[Column::new("n", DataType::Integer)])
        .unwrap_err();
    assert!(
        matches!(&err, ExportError::Plugin(t) if t
            == "the export format `Acme (plugin): Tab-separated` of plugin `acme` is not available"),
        "{err:?}"
    );
    // A batch after the failed begin is refused by the driver of the writer, and nothing reaches the sink.
    let again = exporter.write_batch(&[Row(vec![Value::Int(1)])]);
    assert!(again.is_err());
    drop(exporter);
    assert!(sink.0.lock().unwrap().is_empty());
    r.host.shutdown();
}
