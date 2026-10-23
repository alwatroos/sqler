// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `PluginHost::complete` against the real guest `plugins/fixtures/completer` (id `completer`, driver `sqlite`).

use std::fs;
use std::time::Duration;

use sqler_plugin_api::{
    CompletionItem, CompletionKind, CompletionRequest, MAX_COMPLETION_ITEMS,
    MAX_COMPLETION_LABEL_BYTES, PluginId,
};
use sqler_plugin_host::{
    CallError, DisableReason, ErrorCode, PluginDirs, PluginHost, PluginState, discover,
};

use super::common::{Grants, Scratch, block_on, host, runtime, test_limits};
use std::sync::Arc;

struct Rig {
    _scratch: Scratch,
    rt: tokio::runtime::Runtime,
    host: PluginHost,
    id: PluginId,
}

/// The staged completer fixture, or `None` (after saying why) when the test must be skipped.
fn staged(test: &str) -> Option<std::path::PathBuf> {
    let fixtures = super::fixtures(test)?;
    let dir = fixtures.join("completer");
    if dir.join("completer.wasm").is_file() {
        return Some(dir);
    }
    assert!(
        std::env::var("SQLER_TEST_REQUIRE_PLUGINS").map_or(true, |v| v != "1"),
        "{test}: the completer fixture is not built (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!("skipped: {test}: the completer fixture is not staged");
    None
}

fn rig(test: &str, deadline: Duration) -> Option<Rig> {
    let staged = staged(test)?;
    let scratch = Scratch::new(&format!("completion-{test}"));
    let dir = scratch.path().join("completer");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "completer.wasm"] {
        fs::copy(staged.join(file), dir.join(file)).unwrap();
    }
    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.problems.is_empty(), "{:?}", d.problems);
    let grants = Arc::new(Grants::default());
    grants.approve_declared(&d);
    let id = d.plugins[0].manifest.id().clone();
    let rt = runtime();
    let mut limits = test_limits(Duration::from_secs(30));
    limits.deadlines.completion = deadline;
    let host = host(&rt, grants, &limits);
    assert_eq!(host.load(d).len(), 0);
    Some(Rig {
        _scratch: scratch,
        rt,
        host,
        id,
    })
}

fn request(text: &str) -> CompletionRequest {
    CompletionRequest {
        driver: Some("sqlite".to_owned()),
        statement: text.to_owned(),
        cursor: text.len(),
    }
}

impl Rig {
    fn complete(&self, text: &str) -> Result<Vec<CompletionItem>, CallError> {
        block_on(&self.rt, self.host.complete(&self.id, request(text)))
    }
}

#[test]
fn the_default_items_arrive() {
    let Some(r) = rig("default", Duration::from_secs(30)) else {
        return;
    };
    let items = r.complete("select ").unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].label, "fixture_one");
    assert_eq!(items[0].kind, CompletionKind::Keyword);
    assert_eq!(items[0].detail, None);
    assert_eq!(items[1].kind, CompletionKind::Table);
    assert_eq!(items[1].detail.as_deref(), Some("from fixture"));
    r.host.shutdown();
}

#[test]
fn the_guest_sees_the_driver_and_no_connection() {
    let Some(r) = rig("echo", Duration::from_secs(30)) else {
        return;
    };
    let items = r.complete("echo").unwrap();
    let detail = items[0].detail.as_deref().unwrap();
    assert_eq!(
        detail,
        "driver=Some(\"sqlite\");connection=None;cursor=4;text=echo"
    );
    r.host.shutdown();
}

#[test]
fn a_flood_comes_back_capped_and_clean() {
    let Some(r) = rig("flood", Duration::from_secs(30)) else {
        return;
    };
    let items = r.complete("flood").unwrap();
    assert_eq!(items.len(), MAX_COMPLETION_ITEMS);
    for i in &items {
        assert!(i.label.len() <= MAX_COMPLETION_LABEL_BYTES);
        assert!(!i.label.chars().any(char::is_control), "{:?}", i.label);
        assert_eq!(i.insert_text, i.label);
        assert_eq!(i.detail.as_deref(), Some("a b  c"));
    }
    r.host.shutdown();
}

#[test]
fn providers_follow_the_driver() {
    let Some(r) = rig("filter", Duration::from_secs(30)) else {
        return;
    };
    assert_eq!(
        r.host.completion_providers(Some("sqlite")),
        vec![r.id.clone()]
    );
    assert_eq!(
        r.host.completion_providers(Some("postgres")),
        Vec::<PluginId>::new()
    );
    assert_eq!(r.host.completion_providers(None), Vec::<PluginId>::new());
    r.host.shutdown();
}

#[test]
fn a_bad_request_is_refused_before_the_guest_runs() {
    let Some(r) = rig("bad-request", Duration::from_secs(30)) else {
        return;
    };
    let bad = CompletionRequest {
        driver: None,
        statement: "abc".to_owned(),
        cursor: 9,
    };
    let e = block_on(&r.rt, r.host.complete(&r.id, bad)).unwrap_err();
    assert!(matches!(&e, CallError::Guest(g) if g.code == ErrorCode::InvalidArgument));
    assert!(!e.is_fault());
    let other = "nobody".parse::<PluginId>().unwrap();
    let e = block_on(&r.rt, r.host.complete(&other, request("x"))).unwrap_err();
    assert_eq!(e, CallError::NotFound);
    r.host.shutdown();
}

#[test]
fn a_spinning_guest_times_out() {
    let Some(r) = rig("spin", Duration::from_millis(300)) else {
        return;
    };
    let e = r.complete("spin").unwrap_err();
    assert!(matches!(e, CallError::Timeout { .. }), "{e:?}");
    assert!(e.is_fault());
    r.host.shutdown();
}

#[test]
fn three_traps_disable_the_plugin() {
    let Some(r) = rig("trap", Duration::from_secs(30)) else {
        return;
    };
    for _ in 0..3 {
        let e = r.complete("trap").unwrap_err();
        assert!(matches!(e, CallError::Trap { .. }), "{e:?}");
    }
    assert!(matches!(
        r.host.plugins()[0].state,
        PluginState::Disabled {
            reason: DisableReason::Faults { count: 3, .. }
        }
    ));
    assert_eq!(
        r.host.completion_providers(Some("sqlite")),
        Vec::<PluginId>::new()
    );
    assert_eq!(r.complete("select ").unwrap_err(), CallError::Disabled);
    r.host.shutdown();
}
