// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Helpers for the runtime tests: scratch directories, WAT plugins, a grant source, a current-thread runtime with a
//! hang guard, and a collector for the `tracing` events plugins produce.
//!
//! Tests run on a current-thread runtime so every task polls on the test thread: `tracing::subscriber::with_default`
//! then sees the host's events, and nothing depends on scheduling.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqler_plugin_api::{CapabilitySet, GrantKey, Limits};
use sqler_plugin_host::{
    Discovery, GrantSource, HostOptions, MemorySettings, NoEgress, PluginDirs, PluginHost,
    Services, discover,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// Fresh per-test directory, removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!("sqler-runtime-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The bytes of `tests/fixtures/wat/<name>.wat` as a binary component.
pub fn wat_component(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wat")
        .join(format!("{name}.wat"));
    wat::parse_file(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Writes `<root>/<id>/{sqler.toml, plugin.wasm}` and returns the directory.
pub fn write_plugin(root: &Path, id: &str, wat: &str, capabilities: &[&str]) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    let caps = capabilities.iter().fold(String::new(), |mut s, c| {
        let _ = write!(s, "\"{c}\", ");
        s
    });
    fs::write(
        dir.join("sqler.toml"),
        format!(
            "[plugin]\nid = \"{id}\"\nname = \"Test\"\nversion = \"1.0.0\"\napi = \"0.1\"\ncapabilities = [{caps}]\n\n[entry]\ncomponent = \"plugin.wasm\"\n"
        ),
    )
    .unwrap();
    fs::write(dir.join("plugin.wasm"), wat_component(wat)).unwrap();
    dir
}

/// Discovers every plugin directory given as a development path.
pub fn discover_all(dirs: &[PathBuf]) -> Discovery {
    let d = discover(&PluginDirs {
        dev: dirs.to_vec(),
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    d
}

/// Approves what a test says, per exact grant key.
#[derive(Default)]
pub struct Grants(Mutex<HashMap<GrantKey, CapabilitySet>>);

impl Grants {
    pub fn approve(&self, key: &GrantKey, set: CapabilitySet) {
        self.0.lock().unwrap().insert(key.clone(), set);
    }

    /// Approves everything each discovered plugin declares.
    pub fn approve_declared(&self, d: &Discovery) {
        for p in &d.plugins {
            self.approve(&p.key, p.manifest.capabilities().clone());
        }
    }

    pub fn revoke(&self, key: &GrantKey) {
        self.0.lock().unwrap().remove(key);
    }
}

impl GrantSource for Grants {
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet> {
        self.0.lock().unwrap().get(key).cloned()
    }
}

/// A current-thread runtime with timers.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Blocks on `fut`; a test that hangs panics after 30 s instead of stalling CI.
pub fn block_on<F: Future>(rt: &tokio::runtime::Runtime, fut: F) -> F::Output {
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .expect("hang guard: the test did not finish in 30 s")
    })
}

/// Limits for tests: the policy defaults with short init deadlines.
pub fn test_limits(init: Duration) -> Limits {
    let mut l = Limits::DEFAULT;
    l.deadlines.init = init;
    l
}

/// A host over `rt` with in-memory settings and no egress.
pub fn host(
    rt: &tokio::runtime::Runtime,
    grants: Arc<dyn GrantSource>,
    limits: &Limits,
) -> PluginHost {
    PluginHost::new(HostOptions {
        runtime: rt.handle().clone(),
        grants,
        settings: Arc::new(MemorySettings::default()),
        egress: Arc::new(NoEgress),
        limits: *limits,
        services: Services::new(),
    })
}

/// One `tracing` event.
#[derive(Debug, Clone)]
pub struct Line {
    pub level: tracing::Level,
    pub target: String,
    pub plugin: Option<String>,
    pub message: String,
}

#[derive(Default)]
struct Fields {
    plugin: Option<String>,
    message: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "plugin" => self.plugin = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

struct Collector(Arc<Mutex<Vec<Line>>>);

impl Subscriber for Collector {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, e: &Event<'_>) {
        let mut f = Fields::default();
        e.record(&mut f);
        self.0.lock().unwrap().push(Line {
            level: *e.metadata().level(),
            target: e.metadata().target().to_owned(),
            plugin: f.plugin,
            message: f.message,
        });
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

/// Runs `f` and returns what it returned plus every event emitted on this thread meanwhile.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Line>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let r = tracing::subscriber::with_default(Collector(Arc::clone(&lines)), f);
    let out = lines.lock().unwrap().clone();
    (r, out)
}

/// Only the events a plugin wrote (target `sqler_plugin`).
pub fn plugin_lines(lines: &[Line]) -> Vec<&Line> {
    lines
        .iter()
        .filter(|l| l.target == "sqler_plugin")
        .collect()
}
