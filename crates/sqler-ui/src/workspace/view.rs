// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` root view. It only renders `Workspace` state; every click becomes a
//! `WorkspaceAction` that goes through `Workspace::dispatch`, and the returned `Effect` is
//! carried out here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::Focusable as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, Styled as _, Subscription, Task, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::input::{InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{ActiveTheme as _, Root, WindowExt as _};
use sqler_config::{
    ConfigDir, ConnectionStore, KeybindingOverrides, Modifiers, Platform, SecretStore,
};

use super::action::WorkspaceAction;
use super::bridge::SessionBridge;
use super::state::{Effect, RequestId, Workspace};
use crate::command::{CommandId, CommandRegistry};
use crate::connections::FormField;
use crate::connections::view::{DialogInputs, dialog_overlay};
use crate::connections::{self, ConnectionAction, Connector, Dialog};
use crate::grid::view::{GridProbe, GridViews, action_for_command as grid_action_for};
use crate::keymap::input::key_input;
use crate::keymap::{KeyDispatcher, KeyOutcome, action_for, chord_from_input, route_key};
use crate::palette::{self, OPEN_PALETTE, PaletteState};
use crate::settings::{self, SettingsModel};
use crate::sidebar::{self, SidebarModel};
use crate::tabs;
use crate::theme::apply::{appearance_of, apply};
use crate::theme::resolve;

/// How long quitting waits for open sessions to close.
const CLOSE_LIMIT: Duration = Duration::from_secs(2);
/// How long the platform quit hook waits (the platform allows a little over 200 ms).
const QUIT_HOOK_LIMIT: Duration = Duration::from_millis(150);

/// Everything `run` needs from the binary.
pub struct Startup {
    /// The saved connections, or why they could not be loaded.
    pub connections: Result<ConnectionStore, String>,
    /// Where to save the connections; ignored when `connections` is an error.
    pub config_dir: Option<ConfigDir>,
    /// The connect pipeline, wired by the binary.
    pub connector: Arc<dyn Connector>,
    /// The secret store the pipeline reads (the form's password writes go here).
    pub secrets: Arc<dyn SecretStore>,
    /// A handle to the tokio runtime that runs database work.
    pub runtime: tokio::runtime::Handle,
    /// Exit with code 0 once the first frame is presented (cold-start measurement).
    pub exit_after_first_frame: bool,
    /// The user settings, loaded by the binary.
    pub settings: SettingsModel,
    /// The user keybinding overrides, or why they could not be loaded.
    pub keybindings: Result<KeybindingOverrides, String>,
}

impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Startup")
            .field("connections", &self.connections.as_ref().map(|_| ()))
            .field("config_dir", &self.config_dir)
            .field("exit_after_first_frame", &self.exit_after_first_frame)
            .field("settings", &self.settings)
            .field("keybindings", &self.keybindings)
            .finish_non_exhaustive()
    }
}

/// Initialises the component library. The theme is applied afterwards, from the settings.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
}

/// Opens the main window and runs the application; returns when the app quits.
///
/// On macOS the platform run loop may end the process instead of returning.
pub fn run(startup: Startup) {
    gpui_platform::application().run(move |cx: &mut App| {
        init(cx);
        let system = appearance_of(cx.window_appearance());
        let settings = startup.settings.settings();
        apply(resolve(settings.theme, system), settings.ui_font_size, cx);
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
                let view = cx.new(|cx| WorkspaceView::new(startup, window, cx));
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
    commands: CommandRegistry,
    keys: KeyDispatcher,
    palette: PaletteState,
    palette_input: Entity<InputState>,
    focus: FocusHandle,
    /// The table of every open results-grid tab.
    pub(crate) grids: GridViews,
    /// The running connect attempts; dropping a task cancels the attempt.
    tasks: HashMap<RequestId, Task<()>>,
    /// The text inputs of the dialog on screen.
    inputs: Option<DialogInputs>,
    /// Messages to show as notifications at the next render.
    notices: Vec<String>,
    /// Keeps the OS appearance observer alive.
    _appearance: Subscription,
    /// Keeps the quit hook (closes sessions when the app quits any way) alive.
    _quit: Subscription,
}

impl WorkspaceView {
    fn new(startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = match startup.connections {
            Ok(store) => SidebarModel::from_store(store, startup.config_dir),
            Err(message) => SidebarModel::with_load_error(message),
        };
        if startup.exit_after_first_frame {
            window.on_next_frame(|_, _| std::process::exit(0));
        }
        let mut ws = Workspace::new(sidebar)
            .with_settings(startup.settings)
            .with_drivers(startup.connector.driver_ids());
        ws.set_system_appearance(appearance_of(window.appearance()));
        apply(ws.appearance(), ws.settings().settings().ui_font_size, cx);
        // Follow the OS while the preference is "follow system". Nothing changes in the
        // settings, so nothing is saved.
        let appearance = cx.observe_window_appearance(window, |view, window, cx| {
            if view
                .ws
                .set_system_appearance(appearance_of(window.appearance()))
            {
                apply(
                    view.ws.appearance(),
                    view.ws.settings().settings().ui_font_size,
                    cx,
                );
                cx.notify();
            }
        });
        let mut commands = CommandRegistry::new();
        if let Err(err) = super::register_commands(&mut commands) {
            eprintln!("sqler: could not register the workspace commands: {err}");
        }
        if let Err(err) = palette::register_commands(&mut commands) {
            eprintln!("sqler: could not register the palette command: {err}");
        }
        if let Err(err) = crate::grid::register_commands(&mut commands) {
            eprintln!("sqler: could not register the grid commands: {err}");
        }
        if let Err(err) = connections::register_commands(&mut commands) {
            eprintln!("sqler: could not register the connection commands: {err}");
        }
        let keys = KeyDispatcher::new(Platform::current(), startup.keybindings, &commands);
        for notice in keys.notices() {
            eprintln!("sqler: {notice}");
        }
        let notices = keys.notices().to_vec();
        if !notices.is_empty() {
            // The notification layer lives in `Root`, which does not exist yet.
            window.defer(cx, move |window, cx| {
                for notice in notices {
                    window.push_notification(notice, cx);
                }
            });
        }
        let palette_input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command"));
        cx.subscribe(&palette_input, |view, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value();
                view.palette.set_query(text.as_ref());
                cx.notify();
            }
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Any way of quitting (window close, the OS menu) closes the open sessions, within the
        // short time the platform allows. The `app:quit` command waits longer (`CLOSE_LIMIT`).
        let quit = cx.on_app_quit(|view, _| {
            let sessions = view.ws.take_all_sessions();
            let closed = view.bridge.close_all(sessions, QUIT_HOOK_LIMIT);
            async move {
                let _ = closed.await;
            }
        });
        let view = Self {
            ws,
            bridge: SessionBridge::new(startup.connector, startup.secrets, startup.runtime),
            commands,
            keys,
            palette: PaletteState::default(),
            palette_input,
            focus,
            grids: GridViews::default(),
            tasks: HashMap::new(),
            inputs: None,
            notices: Vec::new(),
            _appearance: appearance,
            _quit: quit,
        };
        GridProbe::start_if_requested(window, cx);
        view
    }

    /// The active tab, if it is a results grid.
    pub(crate) fn ws_active_grid(&self) -> Option<crate::tabs::TabId> {
        self.ws
            .tabs()
            .active()
            .filter(|t| t.kind() == crate::tabs::TabKind::Grid)
            .map(crate::tabs::Tab::id)
    }

    /// Routes a key press before the focused element sees it.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_input(&key_input(&event.keystroke)) else {
            return;
        };
        let was_open = self.palette.is_open();
        if !was_open
            && self.ws.dialog().is_some()
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
        {
            // Escape closes the dialog; a host-key prompt counts as "do not trust".
            cx.stop_propagation();
            self.dispatch_connection(ConnectionAction::Cancel, cx);
            return;
        }
        let outcome = route_key(&chord, &mut self.palette, &self.keys, &self.commands);
        if outcome != KeyOutcome::Ignored {
            cx.stop_propagation();
        }
        if let KeyOutcome::Run(id) = outcome {
            self.run_command(&id, window, cx);
        }
        self.sync_palette_focus(was_open, window, cx);
        cx.notify();
    }

    /// Moves the palette selection for the arrow keys. The input binds `up` and `down` to its
    /// own actions, which run before key-down listeners, so they are intercepted here.
    fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.palette.is_open() {
            cx.propagate();
            return;
        }
        let len = self.palette.items(&self.commands, &self.keys).len();
        self.palette.move_selection(delta, len);
        cx.stop_propagation();
        cx.notify();
    }

    /// Moves the focus to the palette input when it opens and back to the root when it closes.
    fn sync_palette_focus(&mut self, was_open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.palette.is_open();
        if open && !was_open {
            self.palette_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            window.focus(&self.palette_input.focus_handle(cx), cx);
        } else if !open && was_open {
            window.focus(&self.focus, cx);
        }
    }

    /// Carries out a command: the palette opener, a workspace action, or nothing known.
    pub(crate) fn run_command(
        &mut self,
        id: &CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_open = self.palette.is_open();
        if id.as_str() == OPEN_PALETTE {
            self.palette.open();
            if was_open {
                // Reopened from a click: the input still holds the old query.
                self.palette_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
        } else if let Some(action) = action_for(id, &self.ws) {
            self.palette.close();
            self.dispatch(action, cx);
        } else if let Some(action) = connections::action_for(id, &self.ws) {
            self.palette.close();
            self.dispatch_connection(action, cx);
        } else if let Some(grid_action) = grid_action_for(id) {
            self.palette.close();
            match self.ws_active_grid() {
                Some(tab) => self.grids.run_action(tab, grid_action, cx),
                None => window.push_notification("Open a results grid first.", cx),
            }
        } else if id.as_str() == "connection:open" {
            // `action_for` found no selected connection to open.
            self.palette.close();
            window.push_notification("Select a connection in the sidebar first.", cx);
        } else if id.category().starts_with("connection") {
            // A connection command that needs a selection or an open dialog.
            self.palette.close();
            window.push_notification(
                "Select a connection or group in the sidebar first (or open the dialog the command belongs to).",
                cx,
            );
        } else {
            self.palette.close();
            eprintln!("sqler: no handler for command {id}");
        }
        self.sync_palette_focus(was_open, window, cx);
        cx.notify();
    }

    /// Runs an action through the workspace and carries out the effect.
    pub(crate) fn dispatch(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch(action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Runs a connection-manager action through the workspace and carries out the effect.
    pub(crate) fn dispatch_connection(&mut self, action: ConnectionAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch_connection(action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Sets the text of a form input from outside (a colour swatch was clicked).
    pub(crate) fn set_form_input(
        &mut self,
        field: FormField,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.inputs.as_ref().and_then(|i| i.field(field)) {
            input.update(cx, |state, cx| state.set_value(text, window, cx));
        }
    }

    /// Housekeeping after the workspace changed: cancel attempts nobody waits for, forget
    /// closed grids, queue the workspace's message and repaint.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let ws = &self.ws;
        self.tasks.retain(|request, _| ws.is_request_live(*request));
        if let Some(notice) = self.ws.take_notice() {
            self.notices.push(notice);
        }
        self.grids.retain(self.ws.tabs());
        cx.notify();
    }

    /// Makes the dialog inputs match the dialog on screen.
    fn sync_dialog_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wants = matches!(
            self.ws.dialog(),
            Some(Dialog::Form(_) | Dialog::GroupName { .. } | Dialog::TrustHostKey { .. })
        );
        if !wants {
            self.inputs = None;
        } else if self
            .inputs
            .as_ref()
            .is_none_or(|i| i.generation() != self.ws.dialog_generation())
            && !matches!(self.ws.dialog(), Some(Dialog::TrustHostKey { .. }))
        {
            self.inputs = DialogInputs::build(&self.ws, window, cx);
        }
    }

    fn apply(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::None => {}
            Effect::Connect { request, connect } => {
                // The attempt runs on tokio; the UI thread only awaits the oneshot. Keeping
                // the task is what keeps the attempt alive: dropping it cancels.
                let pending = self.bridge.connect(connect);
                let task = cx.spawn(async move |this, cx| {
                    let result = pending.await;
                    this.update(cx, |view, cx| {
                        let effect = view.ws.connect_finished(request, result);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                });
                self.tasks.insert(request, task);
            }
            Effect::CancelConnect(request) => {
                self.tasks.remove(&request);
            }
            Effect::SaveConnections { secrets } => self.save_connections(secrets, cx),
            Effect::TrustHostKey { key, retry } => {
                let pending = self.bridge.trust(key);
                cx.spawn(async move |this, cx| {
                    let result = pending.await;
                    this.update(cx, |view, cx| {
                        let effect = view.ws.trust_finished(retry, result);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Effect::OpenGrid { tab, grid } => self.grids.insert(tab, grid),
            Effect::CloseSession(session) => self.bridge.close(session),
            Effect::CloseSessions(sessions) => {
                let closed = self.bridge.close_all(sessions, CLOSE_LIMIT);
                cx.spawn(async move |_, cx| {
                    let _ = closed.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
            Effect::Quit => cx.quit(),
            Effect::ApplySettings => {
                self.ws.save_settings();
                apply(
                    self.ws.appearance(),
                    self.ws.settings().settings().ui_font_size,
                    cx,
                );
            }
        }
    }

    /// Writes `connections.toml`, then applies the keychain changes; failures become notices.
    fn save_connections(&mut self, secrets: Vec<super::state::SecretOp>, cx: &mut Context<Self>) {
        let (secrets, notices) = self.ws.commit_connections(secrets);
        self.notices.extend(notices);
        if secrets.is_empty() {
            return;
        }
        let written = self.bridge.write_secrets(secrets);
        cx.spawn(async move |this, cx| {
            let failures = written.await.unwrap_or_default();
            if failures.is_empty() {
                return;
            }
            this.update(cx, |view, cx| {
                for failure in failures {
                    view.notices.push(format!("Keychain: {failure}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.grids.materialize(window, cx);
        self.sync_dialog_inputs(window, cx);
        if !self.notices.is_empty() {
            let notices = std::mem::take(&mut self.notices);
            window.defer(cx, move |window, cx| {
                for notice in notices {
                    window.push_notification(notice, cx);
                }
            });
        }
        let theme = cx.theme();
        let (bg, fg) = (theme.background, theme.foreground);
        let sidebar = sidebar::view::sidebar(self.ws.sidebar(), cx);
        let settings_active = self.ws.settings_tab_active();
        let strip = tabs::view::tab_strip(self.ws.tabs(), !settings_active, cx);
        let strip_end = settings::view::strip_end(&self.ws, cx);
        let body = if settings_active {
            settings::view::page(&self.ws, cx).into_any_element()
        } else if let Some(grid) = self.ws_active_grid() {
            self.grids.body(grid, cx).into_any_element()
        } else {
            tabs::view::tab_body(self.ws.tabs(), cx).into_any_element()
        };
        let dialog = dialog_overlay(&self.ws, self.inputs.as_ref(), cx);
        let palette = self.palette.is_open().then(|| {
            let items = self.palette.items(&self.commands, &self.keys);
            palette::view::palette_overlay(
                &self.palette,
                &items,
                &self.palette_input,
                WorkspaceView::run_command,
                cx,
            )
        });
        div()
            .id("workspace")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .capture_action(
                cx.listener(|this, _: &MoveUp, _, cx| this.move_palette_selection(-1, cx)),
            )
            .capture_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.move_palette_selection(1, cx)),
            )
            .relative()
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .w_full()
                            .child(div().flex_1().min_w_0().child(strip))
                            .child(strip_end),
                    )
                    .child(body),
            )
            .children(dialog)
            .children(palette)
    }
}
