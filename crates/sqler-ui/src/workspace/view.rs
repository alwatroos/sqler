// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` root view. It only renders `Workspace` state; every click becomes a
//! `WorkspaceAction` that goes through `Workspace::dispatch`, and the returned `Effect` is
//! carried out here.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::{ActiveTheme as _, Root};
use sqler_driver_api::DriverRegistry;
use sqler_driver_api::sqler_core::ConnectionConfig;

use super::action::WorkspaceAction;
use super::bridge::SessionBridge;
use super::state::{Effect, Workspace};
use crate::sidebar::{self, SidebarModel};
use crate::tabs;

/// Everything `run` needs from the binary.
pub struct Startup {
    /// The saved connections, or why they could not be loaded.
    pub connections: Result<Vec<ConnectionConfig>, String>,
    /// The drivers, wired by the binary.
    pub registry: Arc<DriverRegistry>,
    /// A handle to the tokio runtime that runs database work.
    pub runtime: tokio::runtime::Handle,
    /// Exit with code 0 once the first frame is presented (cold-start measurement).
    pub exit_after_first_frame: bool,
}

impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Startup")
            .field("connections", &self.connections)
            .field("registry", &self.registry)
            .field("exit_after_first_frame", &self.exit_after_first_frame)
            .finish_non_exhaustive()
    }
}

/// Initialises the component library. Later tasks hook the theme in here.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
}

/// Opens the main window and runs the application; returns when the app quits.
///
/// On macOS the platform run loop may end the process instead of returning.
pub fn run(startup: Startup) {
    gpui_platform::application().run(move |cx: &mut App| {
        init(cx);
        cx.activate(true);
        // Closing the main window quits the app on every OS.
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|_| WorkspaceView::new(startup, window));
                cx.new(|cx| Root::new(view, window, cx))
            },
        );
        if let Err(err) = opened {
            eprintln!("error: could not open the window: {err:#}");
            cx.quit();
        }
    });
}

/// The root view: sidebar on the left, tab strip and tab body on the right.
pub(crate) struct WorkspaceView {
    ws: Workspace,
    bridge: SessionBridge,
}

impl WorkspaceView {
    fn new(startup: Startup, window: &Window) -> Self {
        let sidebar = match startup.connections {
            Ok(connections) => SidebarModel::from_connections(connections),
            Err(message) => SidebarModel::with_load_error(message),
        };
        if startup.exit_after_first_frame {
            window.on_next_frame(|_, _| std::process::exit(0));
        }
        Self {
            ws: Workspace::new(sidebar),
            bridge: SessionBridge::new(startup.registry, startup.runtime),
        }
    }

    /// Runs an action through the workspace and carries out the effect.
    pub(crate) fn dispatch(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch(action);
        self.apply(effect, cx);
        cx.notify();
    }

    fn apply(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::None => {}
            Effect::Connect { request, config } => {
                // The attempt runs on tokio; the UI thread only awaits the oneshot.
                let pending = self.bridge.connect(config);
                cx.spawn(async move |this, cx| {
                    let result = pending.await;
                    this.update(cx, |view, cx| {
                        view.ws
                            .connect_finished(request, result.map_err(|e| e.to_string()));
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Effect::CloseSession(session) => self.bridge.close(session),
            Effect::Quit => cx.quit(),
        }
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg) = (theme.background, theme.foreground);
        let sidebar = sidebar::view::sidebar(self.ws.sidebar(), cx);
        let strip = tabs::view::tab_strip(self.ws.tabs(), cx);
        let body = tabs::view::tab_body(self.ws.tabs(), cx);
        div()
            .id("workspace")
            .size_full()
            .flex()
            .flex_row()
            .bg(bg)
            .text_color(fg)
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(strip)
                    .child(body),
            )
    }
}
