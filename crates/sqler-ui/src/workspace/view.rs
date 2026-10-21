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
    App, AppContext as _, Bounds, ClipboardItem, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ListState, ParentElement as _, Render,
    Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_component::input::{InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{ActiveTheme as _, Root, WindowExt as _};
use sqler_catalog::{CacheKey, RefreshEvent};
use sqler_config::{
    ConfigDir, ConnectionId, ConnectionStore, HistoryOptions, KeybindingOverrides, Modifiers,
    Platform, SecretStore,
};
use tokio::sync::oneshot;

use super::action::WorkspaceAction;
use super::bridge::SessionBridge;
use super::browser::CatalogResult;
use super::drop_dialog::drop_overlay;
use super::state::{Effect, RequestId, Workspace};
use crate::browser::{self, BrowserAction, TreeNodeId};
use crate::command::{CommandId, CommandRegistry};
use crate::connections::FormField;
use crate::connections::view::{DialogInputs, dialog_overlay};
use crate::connections::{self, ConnectionAction, Connector, Dialog};
use crate::editor::{self, EditorAction, view::EditorViews};
use crate::export::view::{export_bar, export_overlay};
use crate::grid::view::{
    GridProbe, GridViews, Table, action_for_command as grid_action_for, apply_action,
};
use crate::grid::{
    CopyFormat, GATHER_SLICE_CELLS, GridAction, InsertTarget, SYNC_CELLS, copy_key_format,
    grid_key_action, success_notice,
};
use crate::history::view::HistoryViews;
use crate::history::{self, HistoryCommand, HistoryService};
use crate::keymap::input::key_input;
use crate::keymap::{KeyDispatcher, KeyOutcome, action_for, chord_from_input, route_key};
use crate::palette::{self, OPEN_PALETTE, PaletteState};
use crate::query::view::{OverlayChange, QueryViews, query_overlay, query_tab_body};
use crate::query::{self, ParamInput, ParamKind, QueryCommand, RunEvent};
use crate::settings::{self, SettingsModel};
use crate::sidebar::view::{ListEntry, build_entries, new_list_state, sync_list};
use crate::sidebar::{self, SidebarModel, key_command};
use crate::tabs;
use crate::tabs::TabId;
use crate::theme::apply::{appearance_of, apply};
use crate::theme::resolve;

mod export;

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
    /// Where the query history and the saved queries live; `None` without a config directory.
    pub history_dir: Option<ConfigDir>,
}

impl std::fmt::Debug for Startup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Startup")
            .field("connections", &self.connections.as_ref().map(|_| ()))
            .field("config_dir", &self.config_dir)
            .field("exit_after_first_frame", &self.exit_after_first_frame)
            .field("settings", &self.settings)
            .field("keybindings", &self.keybindings)
            .field("history_dir", &self.history_dir)
            .finish_non_exhaustive()
    }
}

/// Initialises the component library. The theme is applied afterwards, from the settings.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    crate::grid::view::init_keys(cx);
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
    /// The editor of every open SQL editor tab.
    pub(crate) editors: EditorViews,
    /// The results and prompts of the query tabs.
    pub(crate) queries: QueryViews,
    /// The running connect attempts; dropping a task cancels the attempt.
    tasks: HashMap<RequestId, Task<()>>,
    /// The text inputs of the dialog on screen.
    inputs: Option<DialogInputs>,
    /// Takes the keys while the sidebar has the focus.
    sidebar_focus: FocusHandle,
    /// The virtualized sidebar list and the lines it shows.
    sidebar_list: ListState,
    list_entries: Vec<ListEntry>,
    /// The running catalog workers; quitting waits for them (they close the browser sessions).
    catalog_runs: Vec<oneshot::Receiver<()>>,
    /// Messages to show as notifications at the next render.
    notices: Vec<String>,
    /// The copy of a large selection in progress; dropping the task cancels it.
    copy_task: Option<Task<()>>,
    /// The blocking store behind the history and the saved queries.
    history: HistoryService,
    /// The inputs of the history tabs and the history dialogs.
    pub(crate) history_views: HistoryViews,
    /// The text inputs of the export dialog on screen.
    export_inputs: Option<export::ExportInputs>,
    /// Keeps the OS appearance observer alive.
    _appearance: Subscription,
    /// Keeps the quit hook (closes sessions when the app quits any way) alive.
    _quit: Subscription,
}

impl WorkspaceView {
    #[allow(clippy::too_many_lines)] // registers every command set and builds the state
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
        if let Err(err) = crate::grid::register_copy_commands(&mut commands) {
            eprintln!("sqler: cannot register the copy commands: {err}");
        }
        if let Err(err) = crate::grid::register_commands(&mut commands) {
            eprintln!("sqler: could not register the grid commands: {err}");
        }
        if let Err(err) = crate::export::register_commands(&mut commands) {
            eprintln!("sqler: could not register the export commands: {err}");
        }
        if let Err(err) = connections::register_commands(&mut commands) {
            eprintln!("sqler: could not register the connection commands: {err}");
        }
        if let Err(err) = browser::register_commands(&mut commands) {
            eprintln!("sqler: could not register the object browser commands: {err}");
        }
        if let Err(err) = editor::register_commands(&mut commands) {
            eprintln!("sqler: could not register the editor commands: {err}");
        }
        if let Err(err) = query::register_commands(&mut commands) {
            eprintln!("sqler: could not register the query commands: {err}");
        }
        if let Err(err) = history::register_commands(&mut commands) {
            eprintln!("sqler: could not register the history commands: {err}");
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
            view.ws.cancel_all_exports();
            for handle in view.ws.cancel_all_queries() {
                drop(view.bridge.spawn(async move {
                    let _ = handle.cancel().await;
                }));
            }
            let sessions = view.ws.take_all_sessions();
            view.ws.shutdown_browsers();
            let closed = view.bridge.close_all(sessions, QUIT_HOOK_LIMIT);
            let runs = view.take_catalog_runs(QUIT_HOOK_LIMIT);
            async move {
                let _ = closed.await;
                let _ = runs.await;
            }
        });
        let history = HistoryService::new(startup.history_dir, HistoryOptions::default());
        let mut view = Self {
            ws,
            bridge: SessionBridge::new(startup.connector, startup.secrets, startup.runtime),
            commands,
            keys,
            palette: PaletteState::default(),
            palette_input,
            focus,
            grids: GridViews::default(),
            editors: EditorViews::default(),
            queries: QueryViews::default(),
            tasks: HashMap::new(),
            inputs: None,
            sidebar_focus: cx.focus_handle(),
            sidebar_list: new_list_state(),
            list_entries: Vec::new(),
            catalog_runs: Vec::new(),
            notices: Vec::new(),
            copy_task: None,
            history,
            history_views: HistoryViews::default(),
            export_inputs: None,
            _appearance: appearance,
            _quit: quit,
        };
        GridProbe::start_if_requested(window, cx);
        view.sync_sidebar();
        view.load_history(cx);
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

    /// The table the grid commands act on: the table of the active grid tab, else the shown
    /// result of the active query tab.
    fn active_table(&self) -> Option<Table> {
        if let Some(tab) = self.ws_active_grid() {
            return self.grids.table(tab).cloned();
        }
        let tab = self.ws_active_query()?;
        self.queries.shown_table(&self.ws, tab)
    }

    /// The active tab, if it is a query tab.
    fn ws_active_query(&self) -> Option<crate::tabs::TabId> {
        self.ws
            .tabs()
            .active()
            .filter(|t| t.kind() == crate::tabs::TabKind::Query)
            .map(crate::tabs::Tab::id)
    }

    /// The active tab, if it is a SQL editor with an editor on screen.
    fn ws_active_editor(&self) -> Option<crate::tabs::TabId> {
        self.ws
            .tabs()
            .active()
            .map(crate::tabs::Tab::id)
            .filter(|tab| self.editors.contains(*tab))
    }

    /// Runs an editor command with a copy of the active editor's text, and carries out the effect.
    fn dispatch_editor(
        &mut self,
        action: EditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self
            .ws_active_editor()
            .and_then(|tab| self.editors.buffer(tab, cx));
        let effect = self.ws.dispatch_editor(action, buffer);
        if let Some(effect) = self.editors.apply(effect, window, cx) {
            self.apply(effect, cx);
        }
        self.settle(cx);
    }

    /// Routes a key press before the focused element sees it.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.is_held && self.ws.drop_dialog().is_some() {
            // A held key (auto-repeat) must never confirm or cancel a DROP step.
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "menu" && self.sidebar_key(event, window, cx) {
            // The Menu key is not in the chord grammar.
            cx.stop_propagation();
            return;
        }
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
        if !was_open
            && self.ws.drop_dialog().is_some()
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
        {
            // Escape cancels the DROP confirmation (refused once the statement runs).
            cx.stop_propagation();
            self.dispatch_browser(BrowserAction::CancelDrop, cx);
            return;
        }
        if !was_open
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
            && let Some(tab) = self.query_overlay_tab()
        {
            // Escape closes the parameter prompt or the confirmation; nothing runs.
            cx.stop_propagation();
            self.query_dismiss(tab, cx);
            return;
        }
        if !was_open
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
            && self.ws.export_dialog().is_some()
        {
            // Escape closes the export dialog; nothing is exported.
            cx.stop_propagation();
            self.export_dismiss(cx);
            return;
        }
        if !was_open
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
            && self.history_overlay_open()
        {
            // Escape closes the history dialog or the saved-query picker; nothing changes.
            cx.stop_propagation();
            self.history_escape(cx);
            return;
        }
        if self.sidebar_key(event, window, cx) || self.grid_key(event, window, cx) {
            cx.stop_propagation();
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

    /// Runs the object command a key stands for while the sidebar has the focus. `true` if the
    /// key was used.
    fn sidebar_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.palette.is_open()
            || self.ws.dialog().is_some()
            || self.ws.drop_dialog().is_some()
            || !self.sidebar_focus.contains_focused(window, cx)
        {
            return false;
        }
        let input = key_input(&event.keystroke);
        let other = input.ctrl || input.alt || input.cmd;
        let Some(id) =
            key_command(&input.key, input.shift, other).and_then(|id| CommandId::new(id).ok())
        else {
            return false;
        };
        self.run_command(&id, window, cx);
        true
    }

    /// Runs the grid action a key stands for while the data table has the focus. `true` if the
    /// key was used. It runs before the chord router, so that Ctrl+Space is "select column"
    /// in the grid; any key the grid does not claim still reaches the router.
    fn grid_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(table) = self.active_table() else {
            return false;
        };
        let focused = table.focus_handle(cx).contains_focused(window, cx);
        let overlay = self.palette.is_open()
            || self.ws.dialog().is_some()
            || self.ws.drop_dialog().is_some()
            || self.query_overlay_tab().is_some()
            || self.history_overlay_open()
            || self.ws.export_dialog().is_some();
        let input = key_input(&event.keystroke);
        if let Some(format) = copy_key_format(&input, Platform::current(), focused, overlay) {
            self.copy_grid(format, window, cx);
            return true;
        }
        let Some(action) = grid_key_action(&input, Platform::current(), focused, overlay, |m| {
            table.read(cx).delegate().motion_moves(m)
        }) else {
            return false;
        };
        table.update(cx, |state, cx| apply_action(state, action, window, cx));
        cx.notify();
        true
    }

    /// Moves the palette selection for the arrow keys. The input binds `up` and `down` to its
    /// own actions, which run before key-down listeners, so they are intercepted here.
    fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.ws.saved_picker().is_some() && !self.palette.is_open() {
            self.ws.move_picker(delta);
            cx.stop_propagation();
            cx.notify();
            return;
        }
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
        let mut reopen_with = None;
        let mut editor_focus = false;
        let mut grid_focus: Option<Table> = None;
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
        } else if let Some(action) = browser::action_for(id) {
            self.palette.close();
            if action == BrowserAction::ShowMenu {
                reopen_with = self.show_object_menu(window, cx);
            } else {
                self.dispatch_browser(action, cx);
                self.reveal_selection();
            }
        } else if let Some(action) = editor::action_for(id) {
            self.palette.close();
            self.dispatch_editor(action, window, cx);
            editor_focus = true;
        } else if let Some(command) = query::action_for(id) {
            self.palette.close();
            self.run_query_command(command, window, cx);
        } else if let Some(command) = history::action_for(id) {
            self.palette.close();
            self.history_command(command, cx);
        } else if let Some(command) = crate::export::action_for(id) {
            self.palette.close();
            self.run_export_command(command, window, cx);
        } else if let Some(format) = CopyFormat::from_command_id(id) {
            self.palette.close();
            self.copy_grid(format, window, cx);
            grid_focus = self.active_table();
        } else if let Some(grid_action) = grid_action_for(id) {
            self.palette.close();
            if grid_action == GridAction::FetchMore {
                let fetched = match (self.ws_active_grid(), self.ws_active_query()) {
                    (Some(tab), _) => self.grids.run_action(tab, grid_action, window, cx),
                    (None, Some(tab)) => self.queries.fetch_more(tab, cx),
                    (None, None) => {
                        window.push_notification("Open a results grid first.", cx);
                        true
                    }
                };
                if !fetched {
                    window.push_notification("No more rows to fetch.", cx);
                }
            } else {
                match self.active_table() {
                    Some(table) => {
                        table.update(cx, |state, cx| {
                            apply_action(state, grid_action, window, cx);
                        });
                        grid_focus = Some(table);
                    }
                    None => window.push_notification("Open a results grid first.", cx),
                }
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
        if editor_focus && let Some(tab) = self.ws_active_editor() {
            // The palette gave the focus back to the root; the editor takes it.
            self.editors.focus(tab, window, cx);
        }
        if let Some(table) = grid_focus {
            // The palette gave the focus back to the root; the grid takes it.
            window.focus(&table.focus_handle(cx), cx);
        }
        if let Some(query) = reopen_with {
            self.palette_input
                .update(cx, |input, cx| input.set_value(query, window, cx));
        }
        cx.notify();
    }

    /// Copies the selection of the shown grid to the clipboard in `format`.
    ///
    /// A small selection is gathered and rendered at once. A large one is gathered in slices on
    /// the UI thread (one per turn) and rendered on the background executor; a new copy replaces
    /// the one in progress. The copied text only goes to the clipboard: it is never logged or
    /// shown in a notice.
    fn copy_grid(&mut self, format: CopyFormat, window: &mut Window, cx: &mut Context<Self>) {
        // A copy still running must not overwrite the clipboard after this one is refused.
        self.copy_task = None;
        let Some(table) = self.active_table() else {
            window.push_notification("Open a results grid first.", cx);
            return;
        };
        let target = self
            .ws_active_grid()
            .or_else(|| self.ws_active_query())
            .and_then(|tab| self.ws.insert_target(tab))
            .unwrap_or_else(|| InsertTarget::placeholder(None));
        let mut job = match table.read(cx).delegate().copy_job(format, target) {
            Ok(job) => job,
            Err(err) => {
                window.push_notification(err.to_string(), cx);
                return;
            }
        };
        let cells = job.cells();
        if cells <= SYNC_CELLS {
            table.read(cx).delegate().gather_copy(&mut job, usize::MAX);
            match job.render() {
                Ok(out) => {
                    if let Some(notice) = success_notice(&out, cells) {
                        window.push_notification(notice, cx);
                    }
                    self.apply(Effect::Copy(out.text), cx);
                }
                Err(err) => window.push_notification(err.to_string(), cx),
            }
            return;
        }
        let weak = table.downgrade();
        self.copy_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let gathered = weak.update(cx, |state, _| {
                    state.delegate().gather_copy(&mut job, GATHER_SLICE_CELLS)
                });
                match gathered {
                    Ok(true) => break,
                    Ok(false) => {}
                    // The grid was closed: nothing to copy any more.
                    Err(_) => return,
                }
                cx.background_executor().timer(Duration::ZERO).await;
            }
            let result = cx
                .background_executor()
                .spawn(async move { job.render() })
                .await;
            this.update(cx, |view, cx| {
                match result {
                    Ok(out) => {
                        let notice = success_notice(&out, cells);
                        view.apply(Effect::Copy(out.text), cx);
                        view.notices.extend(notice);
                    }
                    Err(err) => view.notices.push(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The context menu cannot open from a key press, so the palette opens on the object
    /// commands instead. Returns the text to put in its input.
    fn show_object_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<&'static str> {
        if self.ws.sidebar().selected_id().is_none() {
            window.push_notification("Select an object in the sidebar first.", cx);
            return None;
        }
        self.palette.open();
        Some("Object: ")
    }

    /// Scrolls the sidebar so that the selected line is in view.
    fn reveal_selection(&self) {
        let items = self.ws.sidebar_items();
        if let Some(ix) = self.ws.selected_item(&items) {
            self.sidebar_list.scroll_to_reveal_item(ix);
        }
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

    /// Runs an object-browser command on the selected node and carries out the effect.
    pub(crate) fn dispatch_browser(&mut self, action: BrowserAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch_browser(action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Runs an object-browser action on a node of a connection's tree.
    pub(crate) fn dispatch_object(
        &mut self,
        conn: &ConnectionId,
        node: TreeNodeId,
        action: BrowserAction,
        cx: &mut Context<Self>,
    ) {
        let effect = self.ws.dispatch_object(conn, node, action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Selects a tree node and gives the sidebar the focus.
    pub(crate) fn select_object(
        &mut self,
        conn: &ConnectionId,
        node: TreeNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.sidebar_focus, cx);
        self.ws.select_object(conn, node);
        self.settle(cx);
    }

    /// A click on a tree row: selects it; a double click opens the data or toggles the node.
    pub(crate) fn click_object(
        &mut self,
        conn: &ConnectionId,
        node: TreeNodeId,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_object(conn, node, window, cx);
        if double {
            let action = self.ws.default_action(conn, node);
            self.dispatch_object(conn, node, action, cx);
        }
    }

    /// Gives the sidebar the focus, so the arrow keys move its selection.
    pub(crate) fn focus_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.sidebar_focus, cx);
    }

    /// The workspace state, for the views.
    pub(crate) fn workspace(&self) -> &Workspace {
        &self.ws
    }

    /// The focus handle of the sidebar.
    pub(crate) fn sidebar_focus(&self) -> &FocusHandle {
        &self.sidebar_focus
    }

    /// The state of the virtualized sidebar list.
    pub(crate) fn sidebar_list(&self) -> &ListState {
        &self.sidebar_list
    }

    /// The lines the sidebar list shows.
    pub(crate) fn list_entries(&self) -> &[ListEntry] {
        &self.list_entries
    }

    /// Makes the list match the workspace; only the lines that changed are remeasured.
    fn sync_sidebar(&mut self) {
        let entries = build_entries(&self.ws);
        sync_list(&mut self.list_entries, &self.sidebar_list, entries);
    }

    /// Stops waiting for the catalog workers after `limit`; the receiver fires when they are
    /// done.
    fn take_catalog_runs(&mut self, limit: Duration) -> oneshot::Receiver<()> {
        let runs = std::mem::take(&mut self.catalog_runs);
        self.bridge.wait_all(runs, limit)
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
        self.editors.retain(&self.ws);
        self.queries.retain(self.ws.tabs());
        self.history_views.retain(&self.ws);
        self.sync_sidebar();
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

    pub(crate) fn apply(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            // `run_command` opens the palette for the object menu; it needs the window.
            Effect::None | Effect::ShowObjectMenu => {}
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
            Effect::OpenGrid { tab, grid } => {
                self.grids.insert(tab, grid);
                self.ws.result_replaced(tab);
            }
            Effect::CloseSession(session) => self.bridge.close(session),
            Effect::CloseSessions(sessions) => {
                let closed = self.bridge.close_all(sessions, CLOSE_LIMIT);
                let runs = self.take_catalog_runs(CLOSE_LIMIT);
                cx.spawn(async move |_, cx| {
                    let _ = closed.await;
                    let _ = runs.await;
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
            Effect::RunQuery { job, grids, events } => self.start_query_job(job, grids, events, cx),
            Effect::RecordHistory(_)
            | Effect::SearchHistory(_)
            | Effect::ClearHistory
            | Effect::SaveSavedQueries { .. } => self.apply_history(effect, cx),
            Effect::PickExportPath { .. } | Effect::RunExport(_) => self.apply_export(effect, cx),
            Effect::CancelQuery(handle) => {
                // The answer does not matter: the job reports how the statement ended.
                drop(self.bridge.spawn(async move {
                    let _ = handle.cancel().await;
                }));
            }
            other => self.apply_browser(other, cx),
        }
    }

    /// Carries out the history effects: each runs on a blocking thread and reports back.
    fn apply_history(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::RecordHistory(entry) => {
                let store = self.history.clone();
                let done = self.bridge.blocking(move || store.record(entry));
                Self::finish_history(done, Workspace::history_recorded, cx);
            }
            Effect::SearchHistory(search) => {
                let store = self.history.clone();
                let (tab, request) = (search.tab, search.request);
                let done = self.bridge.blocking(move || store.search(&search));
                cx.spawn(async move |this, cx| {
                    let Ok(entries) = done.await else { return };
                    this.update(cx, |view, cx| {
                        view.ws.history_results(tab, request, entries);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Effect::ClearHistory => {
                let store = self.history.clone();
                let done = self.bridge.blocking(move || store.clear());
                Self::finish_history(done, Workspace::history_cleared, cx);
            }
            Effect::SaveSavedQueries {
                generation,
                queries,
            } => {
                let store = self.history.clone();
                let done = self
                    .bridge
                    .blocking(move || store.save_saved(generation, &queries));
                Self::finish_history(
                    done,
                    move |ws, result| {
                        ws.saved_queries_written(generation, result);
                        Effect::None
                    },
                    cx,
                );
            }
            _ => {}
        }
    }

    /// Runs a query job on tokio and follows it: every event goes to the workspace, and when the
    /// job ends the session goes back to its tab.
    fn start_query_job(
        &mut self,
        job: query::QueryJob,
        grids: Vec<crate::grid::GridTab>,
        mut events: tokio::sync::mpsc::UnboundedReceiver<RunEvent>,
        cx: &mut Context<Self>,
    ) {
        let (tab, run) = (job.tab, job.run);
        self.queries.insert_grids(tab, grids);
        self.ws.remember_run(tab, &job.statements);
        let done = self.bridge.spawn(job.run());
        cx.spawn(async move |this, cx| {
            // The channel closes when the job ends, so every event arrives before the end.
            while let Some(event) = events.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        let effect = view.ws.query_event(tab, run, event);
                        view.apply(effect, cx);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            let result = done.await;
            this.update(cx, |view, cx| {
                let effect = if let Ok(done) = result {
                    view.ws.query_finished(done)
                } else {
                    view.ws.query_lost(tab, run);
                    Effect::None
                };
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Waits for a blocking history call and hands its result to the workspace; the effect that
    /// comes back is carried out.
    fn finish_history(
        done: oneshot::Receiver<Result<(), String>>,
        handle: impl FnOnce(&mut Workspace, Result<(), String>) -> Effect + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = done
                .await
                .unwrap_or_else(|_| Err("the history task was lost".to_owned()));
            this.update(cx, |view, cx| {
                let effect = handle(&mut view.ws, result);
                view.apply(effect, cx);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Opens the store and loads the saved queries off the UI thread.
    fn load_history(&mut self, cx: &mut Context<Self>) {
        let store = self.history.clone();
        let done = self
            .bridge
            .blocking(move || (store.open(), store.load_saved()));
        cx.spawn(async move |this, cx| {
            let Ok((opened, saved)) = done.await else {
                return;
            };
            this.update(cx, |view, cx| {
                match opened {
                    Ok(notices) => view.notices.extend(notices),
                    Err(err) => view.notices.push(format!("History: {err}")),
                }
                view.ws.saved_queries_loaded(saved);
                view.settle(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Runs a history or saved-query command with a copy of the active editor's text.
    pub(crate) fn history_command(&mut self, command: HistoryCommand, cx: &mut Context<Self>) {
        let buffer = self
            .ws_active_editor()
            .and_then(|tab| self.editors.buffer(tab, cx));
        let effect = self.ws.dispatch_history(command, buffer);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Runs a workspace history method that returns an effect, and carries it out.
    pub(crate) fn history_do(
        &mut self,
        f: impl FnOnce(&mut Workspace) -> Effect,
        cx: &mut Context<Self>,
    ) {
        let effect = f(&mut self.ws);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Whether a history dialog or the saved-query picker is on screen.
    fn history_overlay_open(&self) -> bool {
        self.ws.history_dialog().is_some() || self.ws.saved_picker().is_some()
    }

    /// Escape: closes the history dialog or the picker.
    fn history_escape(&mut self, cx: &mut Context<Self>) {
        self.ws.cancel_history_dialog();
        self.ws.close_picker();
        self.settle(cx);
    }

    /// The active tab, if it is a query tab.
    fn active_query_tab(&self) -> Option<TabId> {
        if self.ws.settings_tab_active() {
            return None;
        }
        self.ws
            .tabs()
            .active()
            .filter(|t| t.kind() == crate::tabs::TabKind::Query)
            .map(crate::tabs::Tab::id)
    }

    /// The active query tab while its parameter prompt or confirmation is open.
    fn query_overlay_tab(&self) -> Option<TabId> {
        let tab = self.active_query_tab()?;
        matches!(
            self.ws.query_tab(tab)?.phase(),
            query::Phase::Prompting(_) | query::Phase::Confirming(_)
        )
        .then_some(tab)
    }

    /// Carries out a query command on the active query tab.
    fn run_query_command(
        &mut self,
        command: QueryCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_query_tab() else {
            window.push_notification("Open a query tab first.", cx);
            return;
        };
        let effect = match command {
            QueryCommand::Run(mode) => {
                let buffer = self.editors.buffer(tab, cx);
                self.ws
                    .run_editor(&mut editor::QueryRunPort, editor::run_mode(mode), buffer)
            }
            QueryCommand::Cancel => self.ws.cancel_editor_run(&mut editor::QueryRunPort),
        };
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// The Cancel button of a running query.
    pub(crate) fn query_cancel(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let effect = self.ws.cancel_query(tab);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Closes the parameter prompt or the confirmation of a tab; nothing runs.
    pub(crate) fn query_dismiss(&mut self, tab: TabId, cx: &mut Context<Self>) {
        self.ws.dismiss_query_prompt(tab);
        self.settle(cx);
    }

    /// Answers the parameter prompt with the typed values.
    pub(crate) fn query_submit(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let texts = self.queries.param_texts(tab, cx);
        let inputs: Vec<ParamInput> = match self.ws.query_tab(tab).map(query::QueryTab::phase) {
            Some(query::Phase::Prompting(form)) => form
                .rows()
                .iter()
                .enumerate()
                .map(|(i, row)| ParamInput {
                    key: row.key.clone(),
                    kind: row.kind,
                    text: texts.get(i).cloned().unwrap_or_else(|| row.text.clone()),
                })
                .collect(),
            _ => return,
        };
        let effect = self.ws.submit_query_params(tab, &inputs);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Accepts the production confirmation of a tab.
    pub(crate) fn query_confirm(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let effect = self.ws.confirm_query(tab);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// Changes how a parameter's text is read; the typed text is kept.
    pub(crate) fn query_set_kind(
        &mut self,
        tab: TabId,
        index: usize,
        kind: ParamKind,
        cx: &mut Context<Self>,
    ) {
        self.ws.set_query_param_kind(tab, index, kind);
        cx.notify();
    }

    /// Carries out the effects of the object browser.
    fn apply_browser(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::Batch(effects) => {
                for effect in effects {
                    self.apply(effect, cx);
                }
            }
            Effect::StartCatalog { worker, .. } => {
                let done = self.bridge.run_catalog(worker);
                // Forget the workers that already stopped, so the list does not grow.
                self.catalog_runs.retain_mut(|run| {
                    matches!(run.try_recv(), Err(oneshot::error::TryRecvError::Empty))
                });
                self.catalog_runs.push(done);
            }
            Effect::LoadCatalog {
                conn,
                generation,
                keys,
            } => self.load_catalog(&conn, generation, keys, cx),
            Effect::WatchRefresh {
                conn,
                generation,
                node,
                run,
                mut handle,
            } => {
                cx.spawn(async move |this, cx| {
                    while let Some(event) = handle.next_event().await {
                        let last = matches!(event, RefreshEvent::Finished(_));
                        let alive = this
                            .update(cx, |view, cx| {
                                let effect =
                                    view.ws.refresh_event(&conn, generation, node, run, event);
                                view.apply(effect, cx);
                                view.settle(cx);
                            })
                            .is_ok();
                        if last || !alive {
                            break;
                        }
                    }
                })
                .detach();
            }
            Effect::RunJob(job) => {
                let lost = job.lost();
                let done = self.bridge.spawn(job.run(self.bridge.connector()));
                cx.spawn(async move |this, cx| {
                    // A panicked task drops the sender; the job still has to end, or a
                    // DROP overlay would stay up for good.
                    let result = done.await.unwrap_or(lost);
                    this.update(cx, |view, cx| {
                        let effect = view.ws.job_finished(result);
                        view.apply(effect, cx);
                        view.settle(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Effect::OpenData { tab, grid, job } => {
                self.grids.insert(tab, grid);
                self.ws.remember_open_data(tab, &job.sql);
                // The job reports failures to the grid itself; nobody waits for it.
                drop(self.bridge.spawn(job.run(self.bridge.connector())));
            }
            Effect::Copy(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            // `run_command` opens the palette, which needs the window.
            _ => {}
        }
    }

    /// Loads the keys through the catalog; each answer goes back to the workspace.
    fn load_catalog(
        &mut self,
        conn: &ConnectionId,
        generation: u64,
        keys: Vec<CacheKey>,
        cx: &mut Context<Self>,
    ) {
        let Some(catalog) = self.ws.catalog(conn, generation) else {
            return;
        };
        for key in keys {
            let (catalog, conn) = (catalog.clone(), conn.clone());
            cx.spawn(async move |this, cx| {
                let result = match &key {
                    CacheKey::Children(node) => {
                        CatalogResult::Children(catalog.children(node.clone()).await)
                    }
                    CacheKey::Table(obj) => CatalogResult::Table(catalog.table(obj.clone()).await),
                    _ => return,
                };
                this.update(cx, |view, cx| {
                    let effect = view.ws.catalog_loaded(&conn, generation, key, result);
                    view.apply(effect, cx);
                    view.settle(cx);
                })
                .ok();
            })
            .detach();
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
    #[allow(clippy::too_many_lines)] // one declarative element tree
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.grids.materialize(window, cx);
        self.editors.materialize(&mut self.ws, window, cx);
        if self.queries.materialize(&self.ws, window, cx) == OverlayChange::Closed
            && let Some(tab) = self.active_query_tab()
        {
            self.editors.focus(tab, window, cx);
        }
        self.sync_dialog_inputs(window, cx);
        self.sync_export_inputs(window, cx);
        if self.history_views.materialize(&self.ws, window, cx) {
            // A history dialog or the picker closed: the root takes the focus back.
            window.focus(&self.focus, cx);
        }
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
        let sidebar = sidebar::view::sidebar(self, cx);
        let settings_active = self.ws.settings_tab_active();
        let strip = tabs::view::tab_strip(self.ws.tabs(), !settings_active, cx);
        let strip_end = settings::view::strip_end(&self.ws, cx);
        let body = if settings_active {
            settings::view::page(&self.ws, cx).into_any_element()
        } else if let Some((tab, editor)) = self
            .active_query_tab()
            .and_then(|t| self.editors.body(t).map(|e| (t, e.into_any_element())))
        {
            query_tab_body(editor, tab, self, cx)
        } else if let Some(tab) = self
            .ws
            .tabs()
            .active()
            .map(crate::tabs::Tab::id)
            .filter(|id| self.ws.history_tab(*id).is_some())
        {
            history::view::history_body(self, tab, cx)
        } else if let Some(grid) = self.ws_active_grid() {
            self.grids.body(grid, cx).into_any_element()
        } else if let Some(tab) = self
            .ws
            .tabs()
            .active()
            .and_then(|tab| self.ws.object_tab(tab.id()))
        {
            tabs::view::object_tab_body(tab, cx).into_any_element()
        } else {
            tabs::view::tab_body(self.ws.tabs(), cx).into_any_element()
        };
        let dialog = dialog_overlay(&self.ws, self.inputs.as_ref(), cx);
        let drop_dialog = drop_overlay(self.ws.drop_dialog(), cx);
        let query_dialog = self
            .query_overlay_tab()
            .and_then(|tab| query_overlay(self, tab, cx));
        let history_dialog = history::view::history_overlay(self, cx);
        let export_dialog = export_overlay(self, cx);
        let export_progress = self
            .ws
            .tabs()
            .active()
            .and_then(|tab| export_bar(self, tab.id(), cx));
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
                    .children(export_progress)
                    .child(body),
            )
            .children(dialog)
            .children(drop_dialog)
            .children(query_dialog)
            .children(history_dialog)
            .children(export_dialog)
            .children(palette)
    }
}
