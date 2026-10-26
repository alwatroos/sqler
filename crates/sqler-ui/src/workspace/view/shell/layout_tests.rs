// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Headless layout regression tests: the SQL editor fills its tab and follows every resize.

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel counts in a test

use std::sync::Arc;
use std::time::Duration;

use gpui::{Bounds, Entity, Pixels, TestAppContext, VisualTestContext, px, size};
use sqler_config::{ConnectionSpec, ConnectionStore, KeybindingOverrides, MemoryStore};
use sqler_driver_api::sqler_core::DriverId;

use crate::ai::{AiStartup, NoProviders};
use crate::command::CommandId;
use crate::plugins::PluginsStartup;
use crate::query::view::editor_selector;
use crate::settings::SettingsModel;
use crate::tabs::TabId;
use crate::theme::Appearance;
use crate::theme::apply::apply;
use crate::workbench::WorkbenchAction;
use crate::workspace::testing::FakeConnector;
use crate::workspace::view::{Startup, WorkspaceView};

/// Covers the border and padding insets of the Input root (about 7 to 9 px per side).
const SLACK: Pixels = px(20.);

struct Rig {
    view: Entity<WorkspaceView>,
    // Keeps the runtime the background work runs on alive.
    _runtime: tokio::runtime::Runtime,
}

fn open(cx: &mut TestAppContext) -> (Rig, &mut VisualTestContext) {
    cx.update(crate::workspace::init);
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut store = ConnectionStore::new();
    let spec = ConnectionSpec::new("dev", DriverId::new("fake").unwrap(), ":memory:");
    store.add_connection(spec, None).unwrap();
    let startup = Startup {
        connections: Ok(store),
        config_dir: None,
        connector: Arc::new(FakeConnector::new()),
        secrets: Arc::new(MemoryStore::new()),
        runtime: runtime.handle().clone(),
        exit_after_first_frame: false,
        settings: SettingsModel::default(),
        keybindings: Ok(KeybindingOverrides::default()),
        history_dir: None,
        workbench: crate::workbench::WorkbenchStartup::default(),
        ai: AiStartup {
            config: Ok(sqler_config::AiConfig::default()),
            dir: None,
            factory: Arc::new(NoProviders),
        },
        plugins: PluginsStartup::unavailable(),
        export_sources: Vec::new(),
        update: crate::update::UpdateStartup::unavailable(),
    };
    let (view, cx) = cx.add_window_view(move |window, cx| WorkspaceView::new(startup, window, cx));
    cx.update(|_, cx| apply(Appearance::Dark, 14, cx));
    (
        Rig {
            view,
            _runtime: runtime,
        },
        cx,
    )
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
    }
    cx.run_until_parked();
}

fn settle_until(
    rig: &Rig,
    cx: &mut VisualTestContext,
    what: &str,
    done: impl Fn(&WorkspaceView) -> bool,
) {
    for _ in 0..2000 {
        settle(cx);
        if rig.view.read_with(cx, |view, _| done(view)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn open_editor(rig: &Rig, cx: &mut VisualTestContext) -> TabId {
    let before = rig.view.read_with(cx, |v, _| v.ws.tabs().len());
    rig.view.update(cx, |view, cx| {
        let id = view.ws.sidebar().entries()[0].id().clone();
        let effect = view.ws.open_editor_on(&id, Some("select 1".into()));
        view.apply(effect, cx);
    });
    settle_until(rig, cx, "the editor tab", |v| v.ws.tabs().len() > before);
    let tab = rig
        .view
        .read_with(cx, |v, _| v.ws.tabs().iter().last().unwrap().id());
    settle_until(rig, cx, "the editor", |v| {
        v.ws.has_session(tab) && v.editors.contains(tab)
    });
    tab
}

fn command(rig: &Rig, cx: &mut VisualTestContext, id: &str) {
    let id = CommandId::new(id).unwrap();
    cx.update(|window, app| {
        rig.view
            .update(app, |view, cx| view.run_command(&id, window, cx));
    });
    settle(cx);
}

fn workbench(rig: &Rig, cx: &mut VisualTestContext, action: WorkbenchAction) {
    rig.view
        .update(cx, |view, cx| view.dispatch_workbench(action, cx));
    settle(cx);
}

/// The container of the editor area and the editor's own laid-out bounds.
fn measure(rig: &Rig, cx: &mut VisualTestContext, tab: TabId) -> (Bounds<Pixels>, Bounds<Pixels>) {
    settle(cx);
    // `debug_bounds` wants a `'static` name; a test may leak a few bytes.
    let selector: &'static str = Box::leak(editor_selector(tab).into_boxed_str());
    let container = cx
        .debug_bounds(selector)
        .expect("the editor area was drawn");
    let editor = rig
        .view
        .read_with(cx, |v, app| v.editors.input_bounds(tab, app))
        .expect("the editor exists");
    assert!(
        editor.size.height > px(0.) && editor.size.width > px(0.),
        "{editor:?}"
    );
    (container, editor)
}

fn near(a: Pixels, b: Pixels) -> bool {
    (a - b).abs() <= SLACK
}

fn assert_fills(container: Bounds<Pixels>, editor: Bounds<Pixels>, what: &str) {
    assert!(
        near(container.top(), editor.top())
            && near(container.bottom(), editor.bottom())
            && near(container.left(), editor.left())
            && near(container.right(), editor.right()),
        "{what}: editor {editor:?} does not fill {container:?}"
    );
    assert!(editor.size.height >= px(200.), "{what}: only {editor:?}");
}

fn near_f(a: Pixels, b: Pixels, delta: f32) -> bool {
    near(a + px(delta), b)
}

#[gpui::test]
fn the_editor_fills_its_tab_and_follows_window_and_panel_resizes(cx: &mut TestAppContext) {
    let (rig, cx) = open(cx);
    cx.simulate_resize(size(px(1280.), px(800.)));
    let tab = open_editor(&rig, cx);
    let (c0, e0) = measure(&rig, cx, tab);
    assert_fills(c0, e0, "default layout");
    let body_selector: &'static str =
        Box::leak(format!("query-editor-body-{tab:?}").into_boxed_str());
    let body = cx.debug_bounds(body_selector).unwrap();
    assert!(near(body.bottom(), c0.bottom()), "{body:?} vs {c0:?}");

    cx.simulate_resize(size(px(1280.), px(1000.)));
    let (c1, e1) = measure(&rig, cx, tab);
    assert_fills(c1, e1, "taller window");
    assert!(
        near_f(e0.size.height, e1.size.height, 200.),
        "{e0:?} {e1:?}"
    );
    cx.simulate_resize(size(px(1000.), px(700.)));
    let (c2, e2) = measure(&rig, cx, tab);
    assert_fills(c2, e2, "smaller window");
    assert!(e2.size.width < e1.size.width);

    cx.simulate_resize(size(px(1280.), px(800.)));
    if !rig.view.read_with(cx, |v, _| v.ws.layout().panel_visible()) {
        command(&rig, cx, "workbench:toggle-bottom-panel");
    }
    let (_, base) = measure(&rig, cx, tab);
    command(&rig, cx, "workbench:toggle-bottom-panel");
    let (c3, e3) = measure(&rig, cx, tab);
    assert_fills(c3, e3, "bottom panel hidden");
    assert!(e3.size.height > base.size.height);
    command(&rig, cx, "workbench:toggle-bottom-panel");
    let (c4, e4) = measure(&rig, cx, tab);
    assert_fills(c4, e4, "bottom panel shown again");
    assert!(near(e4.size.height, base.size.height));

    let h0 = rig.view.read_with(cx, |v, _| v.ws.layout().panel_height());
    workbench(
        &rig,
        cx,
        WorkbenchAction::SetPanelHeight((h0 + 100.) as u16),
    );
    let (c5, e5) = measure(&rig, cx, tab);
    assert_fills(c5, e5, "taller bottom panel");
    assert!(
        near_f(base.size.height, e5.size.height, -100.),
        "{e5:?} {base:?}"
    );
}

#[gpui::test]
fn the_editor_follows_the_side_panel_and_editor_splits(cx: &mut TestAppContext) {
    let (rig, cx) = open(cx);
    cx.simulate_resize(size(px(1280.), px(800.)));
    let tab = open_editor(&rig, cx);
    let (_, base) = measure(&rig, cx, tab);

    command(&rig, cx, "workbench:toggle-side-panel");
    let (c1, e1) = measure(&rig, cx, tab);
    assert_fills(c1, e1, "side panel hidden");
    assert!(e1.size.width > base.size.width);
    command(&rig, cx, "workbench:toggle-side-panel");
    let w0 = rig.view.read_with(cx, |v, _| v.ws.layout().side_width());
    workbench(&rig, cx, WorkbenchAction::SetSideWidth((w0 + 80.) as u16));
    let (c2, e2) = measure(&rig, cx, tab);
    assert_fills(c2, e2, "wider side panel");
    assert!(
        near_f(base.size.width, e2.size.width, -80.),
        "{e2:?} {base:?}"
    );
    workbench(&rig, cx, WorkbenchAction::SetSideWidth(w0 as u16));

    let second = open_editor(&rig, cx);
    command(&rig, cx, "pane:split-right");
    let groups = rig.view.read_with(cx, |v, _| v.ws.tabs().groups().len());
    assert_eq!(groups, 2);
    for t in [tab, second] {
        let (c, e) = measure(&rig, cx, t);
        assert_fills(c, e, "split group");
        assert!(e.size.width < base.size.width * 0.6, "{e:?} {base:?}");
        assert!(near(e.size.height, base.size.height));
    }
}

#[gpui::test]
fn the_editor_fills_the_rest_with_the_chat_side_and_bottom_panels_open(cx: &mut TestAppContext) {
    let (rig, cx) = open(cx);
    cx.simulate_resize(size(px(1280.), px(800.)));
    let tab = open_editor(&rig, cx);
    if !rig.view.read_with(cx, |v, _| v.ws.layout().panel_visible()) {
        command(&rig, cx, "workbench:toggle-bottom-panel");
    }
    let (_, base) = measure(&rig, cx, tab);

    workbench(&rig, cx, WorkbenchAction::ToggleChatPanel);
    assert!(rig.view.read_with(cx, |v, _| v.ws.layout().chat_visible()));
    let chat = rig.view.read_with(cx, |v, _| v.ws.layout().chat_width());
    let (c1, e1) = measure(&rig, cx, tab);
    assert_fills(c1, e1, "chat, side and bottom panels at 1280x800");
    assert!(
        near_f(base.size.width, e1.size.width, -chat),
        "{e1:?} {base:?}"
    );
    assert!(near(e1.size.height, base.size.height));

    cx.simulate_resize(size(px(1000.), px(700.)));
    let (c2, e2) = measure(&rig, cx, tab);
    assert_fills(c2, e2, "chat, side and bottom panels at 1000x700");
    assert!(e2.size.width < e1.size.width && e2.size.height < e1.size.height);
    assert!(
        e2.size.width >= px(200.),
        "editor squeezed to {:?}",
        e2.size
    );

    cx.simulate_resize(size(px(1400.), px(900.)));
    let (c3, e3) = measure(&rig, cx, tab);
    assert_fills(c3, e3, "chat, side and bottom panels at 1400x900");
    assert!(e3.size.width > e1.size.width && e3.size.height > e1.size.height);

    workbench(&rig, cx, WorkbenchAction::ToggleChatPanel);
    let (c4, e4) = measure(&rig, cx, tab);
    assert_fills(c4, e4, "chat closed again");
    assert!(near_f(e3.size.width, e4.size.width, chat));
}
