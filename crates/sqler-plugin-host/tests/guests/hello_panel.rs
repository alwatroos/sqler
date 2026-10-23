// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The example plugin `plugins/hello-panel` (plugin id `hello-panel`): granted, ungranted and undeclared capability.

use std::fs;
use std::path::PathBuf;

use sqler_plugin_api::{Widget, WidgetEvent, WidgetTree};
use sqler_plugin_host::{CallError, ErrorCode, PluginDirs, discover};

use super::common::{Scratch, block_on};
use super::{Approve, Opts, World, cid, with_sink, world};

/// The staged example, or `None` (after saying why) when the test must be skipped.
fn staged(test: &str) -> Option<PathBuf> {
    let fixtures = super::fixtures(test)?;
    if fixtures.join("hello-panel/hello-panel.wasm").is_file() {
        return Some(fixtures);
    }
    assert!(
        std::env::var("SQLER_TEST_REQUIRE_PLUGINS").map_or(true, |v| v != "1"),
        "{test}: the hello-panel example is not built (run `cargo run -p xtask -- plugins build`)"
    );
    eprintln!("skipped: {test}: the hello-panel example is not staged");
    None
}

fn text_of(tree: &WidgetTree, node: usize) -> &str {
    match &tree.nodes()[node].widget {
        Widget::Text(t) => &t.content,
        other => panic!("node {node} is {other:?}"),
    }
}

fn render(w: &World) -> Result<Result<WidgetTree, sqler_plugin_api::WidgetError>, CallError> {
    block_on(&w.rt, w.host.render_panel(&w.id, &cid("hello")))
}

fn click(w: &World, id: &str) -> Result<Option<WidgetTree>, CallError> {
    let r = block_on(
        &w.rt,
        w.host
            .panel_event(&w.id, &cid("hello"), WidgetEvent::Clicked(id.to_owned())),
    )?;
    Ok(r.map(Result::unwrap))
}

fn assert_denied<T: std::fmt::Debug>(r: Result<T, CallError>) {
    match r {
        Err(CallError::Guest(e)) => assert_eq!(e.code, ErrorCode::CapabilityDenied),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_granted_panel_renders_counts_clicks_and_pushes_the_status() {
    let Some(fx) = staged("hello_granted") else {
        return;
    };
    let (opts, pushed) = with_sink();
    let w = world("hello-granted", &fx, "hello-panel", &opts);

    let tree = render(&w).unwrap().unwrap();
    assert_eq!(tree.nodes().len(), 4);
    assert_eq!(text_of(&tree, 1), "Hello from a plugin");
    assert_eq!(text_of(&tree, 2), "Nobody has clicked yet.");

    let tree = click(&w, "click").unwrap().expect("a click gives a tree");
    assert_eq!(text_of(&tree, 2), "Clicked once.");
    assert_eq!(pushed.count("status hello-panel/clicks Clicks: 1"), 1);
    assert_eq!(w.status("clicks"), "Clicks: 1");

    let tree = click(&w, "click").unwrap().unwrap();
    assert_eq!(text_of(&tree, 2), "Clicked 2 times.");
    assert!(click(&w, "other").unwrap().is_none());

    w.command("say-hello").unwrap();
    assert_eq!(pushed.count("refresh hello-panel/hello"), 1);
    assert_eq!(pushed.count("notify "), 1);
    w.done();
}

#[test]
fn an_unapproved_panel_is_denied_and_its_refresh_is_dropped() {
    let Some(fx) = staged("hello_ungranted") else {
        return;
    };
    let (mut opts, pushed) = with_sink();
    opts.approve = Approve::Nothing;
    let w = world("hello-ungranted", &fx, "hello-panel", &opts);
    assert!(w.host.plugins()[0].effective.is_empty());
    assert_denied(render(&w));
    assert_denied(click(&w, "click"));
    w.command("say-hello").unwrap();
    assert_eq!(pushed.count("refresh "), 0);
    w.done();
}

#[test]
fn a_manifest_with_a_panel_but_no_ui_panel_is_refused() {
    let Some(fx) = staged("hello_undeclared") else {
        return;
    };
    // A scratch copy of the staged plugin whose manifest declares no capability. A panel needs `ui.panel`, so the
    // host refuses the plugin when it reads the manifest, whatever the user would approve.
    let scratch = Scratch::new("hello-undeclared-fixtures");
    let dir = scratch.path().join("hello-panel");
    fs::create_dir_all(&dir).unwrap();
    for file in ["sqler.toml", "hello-panel.wasm"] {
        fs::copy(fx.join("hello-panel").join(file), dir.join(file)).unwrap();
    }
    let manifest = dir.join("sqler.toml");
    let text = fs::read_to_string(&manifest).unwrap();
    let patched = text.replace("capabilities = [\"ui.panel\"]", "capabilities = []");
    assert_ne!(text, patched, "the hello-panel manifest changed shape");
    fs::write(&manifest, patched).unwrap();

    let d = discover(&PluginDirs {
        dev: vec![dir],
        ..PluginDirs::default()
    });
    assert!(d.plugins.is_empty());
    assert_eq!(d.problems.len(), 1, "{:?}", d.problems);
    let error = format!("{:?}", d.problems[0].error);
    assert!(
        error.contains("MissingCapability") && error.contains("ui.panel"),
        "{error}"
    );
}

#[test]
fn a_command_the_manifest_does_not_declare_never_reaches_the_guest() {
    let Some(fx) = staged("hello_unknown_command") else {
        return;
    };
    let w = world("hello-unknown", &fx, "hello-panel", &Opts::default());
    let r = w.command("nope");
    assert!(matches!(r, Err(CallError::Undeclared { .. })), "{r:?}");
    w.done();
}
