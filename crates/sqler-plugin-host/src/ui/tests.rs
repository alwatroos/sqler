// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `ui` import: the gate (sink, declaration, `ui.panel`), the rate limit and the sanitising, against a recording
//! sink. No component is instantiated; the host trait delegates to the same functions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::assert_is_empty
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqler_plugin_api::{Capability, CapabilitySet, ContributionId, PluginId};

use super::{PluginUi, UiSink};
use crate::dirs::PluginDirs;
use crate::discovery::{DiscoveredPlugin, discover};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A discovered plugin `shower` that declares the panel `main` and the status item `env`, removed on drop.
struct Fixture {
    dir: PathBuf,
    plugin: Arc<DiscoveredPlugin>,
}

impl Fixture {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("sqler-plugin-host-ui-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join("shower")).unwrap();
        fs::write(
            dir.join("shower").join("sqler.toml"),
            "[plugin]\nid = \"shower\"\nname = \"Shower\"\nversion = \"1.0.0\"\napi = \"0.1\"\n\
             capabilities = [\"ui.panel\"]\n\n[entry]\ncomponent = \"shower.wasm\"\n\n[[panels]]\nid = \"main\"\ntitle = \"Main\"\n\n\
             [[status-items]]\nid = \"env\"\n",
        )
        .unwrap();
        // Discovery only reads and hashes the component; nothing here instantiates it.
        fs::write(dir.join("shower").join("shower.wasm"), b"not a component").unwrap();
        let found = discover(&PluginDirs {
            dev: vec![dir.join("shower")],
            ..PluginDirs::default()
        });
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        let plugin = Arc::new(found.plugins.into_iter().next().unwrap());
        Self { dir, plugin }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl UiSink for Recorder {
    fn refresh_panel(&self, plugin: &PluginId, panel: &ContributionId) {
        self.0
            .lock()
            .unwrap()
            .push(format!("refresh {plugin}/{panel}"));
    }

    fn set_status(&self, plugin: &PluginId, item: &ContributionId, text: String) {
        self.0
            .lock()
            .unwrap()
            .push(format!("status {plugin}/{item}={text}"));
    }

    fn notify(&self, plugin: &PluginId, message: String) {
        self.0
            .lock()
            .unwrap()
            .push(format!("notify {plugin}: {message}"));
    }
}

fn service() -> (PluginUi, Arc<Recorder>) {
    let rec = Arc::new(Recorder::default());
    (PluginUi::new(Arc::clone(&rec) as Arc<dyn UiSink>), rec)
}

fn panel_cap() -> CapabilitySet {
    let mut set = CapabilitySet::new();
    set.insert(Capability::UiPanel);
    set
}

#[test]
fn declared_items_are_forwarded_with_their_ids() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let now = Instant::now();
    ui.refresh_panel(&f.plugin, &panel_cap(), "main", now);
    ui.set_status(&f.plugin, "env", "ready", now);
    ui.notify(&f.plugin, "hello", now);
    assert_eq!(
        rec.seen(),
        vec![
            "refresh shower/main",
            "status shower/env=ready",
            "notify shower: hello"
        ]
    );
}

#[test]
fn undeclared_panel_and_item_are_dropped() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let now = Instant::now();
    ui.refresh_panel(&f.plugin, &panel_cap(), "other", now);
    ui.set_status(&f.plugin, "nope", "x", now);
    assert!(rec.seen().is_empty());
}

#[test]
fn refresh_without_ui_panel_is_dropped_but_status_is_not_gated() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let now = Instant::now();
    ui.refresh_panel(&f.plugin, &CapabilitySet::new(), "main", now);
    assert!(rec.seen().is_empty());
    ui.set_status(&f.plugin, "env", "still works", now);
    assert_eq!(rec.seen(), vec!["status shower/env=still works"]);
}

#[test]
fn status_floods_are_limited_and_recover_with_the_clock() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let t0 = Instant::now();
    for i in 0..100 {
        ui.set_status(&f.plugin, "env", &i.to_string(), t0);
    }
    assert_eq!(rec.seen().len(), 20);
    ui.set_status(&f.plugin, "env", "later", t0 + Duration::from_millis(100));
    assert_eq!(rec.seen().len(), 21);
}

#[test]
fn notifications_are_limited_to_three_in_a_burst() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let t0 = Instant::now();
    for i in 0..10 {
        ui.notify(&f.plugin, &format!("n{i}"), t0);
    }
    assert_eq!(rec.seen().len(), 3);
}

#[test]
fn text_is_sanitised_and_capped_before_the_sink() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let now = Instant::now();
    ui.set_status(&f.plugin, "env", "a\nb\u{1b}c", now);
    ui.set_status(&f.plugin, "env", &"x".repeat(1000), now);
    ui.set_status(&f.plugin, "env", "", now);
    ui.notify(&f.plugin, &"y".repeat(1000), now);
    ui.notify(&f.plugin, "\n\t ", now);
    let seen = rec.seen();
    assert_eq!(seen[0], "status shower/env=a bc");
    assert_eq!(seen[1].len(), "status shower/env=".len() + 256);
    // An empty status text is forwarded: it hides the item.
    assert_eq!(seen[2], "status shower/env=");
    assert_eq!(seen[3].len(), "notify shower: ".len() + 512);
    // A blank notification is dropped.
    assert_eq!(seen.len(), 4);
}

#[test]
fn limits_are_per_plugin_and_outlive_calls() {
    let f = Fixture::new();
    let (ui, rec) = service();
    let t0 = Instant::now();
    for _ in 0..3 {
        ui.notify(&f.plugin, "x", t0);
    }
    // The same service, a moment later: still spent.
    ui.notify(&f.plugin, "x", t0 + Duration::from_secs(1));
    assert_eq!(rec.seen().len(), 3);
}
