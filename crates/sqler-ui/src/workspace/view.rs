// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `GPUI` root view. It only renders `Workspace` state; every click becomes a
//! `WorkspaceAction` that goes through `Workspace::dispatch`, and the returned `Effect` is
//! carried out here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::Focusable as _;
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ListState, ParentElement as _, Render,
    ScrollHandle, Styled as _, Subscription, Task, Window, WindowBounds, WindowOptions, div, px,
    size,
};
use gpui_component::input::{
    Enter as InputEnter, Escape as InputEscape, IndentInline, InputEvent, InputState, MoveDown,
    MoveUp, OutdentInline,
};
use gpui_component::resizable::ResizableState;
use gpui_component::{ActiveTheme as _, Root, WindowExt as _};
use sqler_catalog::{CacheKey, RefreshEvent};
use sqler_config::{
    ConfigDir, ConnectionId, ConnectionStore, HistoryOptions, KeybindingOverrides, Modifiers,
    Platform, SecretStore, SessionLoad, SessionState, WINDOW_SIZE_RANGE, WindowState,
    WorkbenchLoad, WorkbenchState,
};
use tokio::sync::oneshot;

mod agent;
mod ai;
mod ai_session;
mod assist;
mod audit;
mod edit_ui;
mod shell;
use shell::SESSION_CHECKPOINT;

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
use crate::export::view::export_overlay;
use crate::grid::edit::EditorKey;
use crate::grid::view::{
    GridProbe, GridViews, Table, action_for_command as grid_action_for, apply_action,
};
use crate::grid::{
    CopyFormat, GATHER_SLICE_CELLS, GridAction, InsertTarget, Motion, SYNC_CELLS, copy_key_format,
    grid_key_action, success_notice,
};
use crate::history::view::HistoryViews;
use crate::history::{self, HistoryCommand, HistoryService};
use crate::keymap::input::key_input;
use crate::keymap::{
    KeyDispatcher, KeyOutcome, action_for, chord_from_input, pane_action_for, route_key,
};
use crate::palette::{self, OPEN_PALETTE, PaletteState};
use crate::plugins::grant::GrantPrompt;
use crate::query::view::{OverlayChange, QueryViews, query_overlay};
use crate::query::{self, ParamInput, ParamKind, QueryCommand, RunEvent};
use crate::settings::SettingsModel;
use crate::sidebar::view::{ListEntry, build_entries, new_list_state, sync_list};
use crate::sidebar::{SidebarModel, key_command};
use crate::tabs::{GroupId, TabId};
use crate::theme::apply::{appearance_of, apply};
use crate::theme::icons::AppAssets;
use crate::theme::resolve;
use crate::workbench::view::Hints;
use crate::workbench::{self, ActivityItem, PanelView, RecentCommands, Region, WorkbenchAction};
use edit_ui::EditUi;
use plugin_ui::PluginUiViews;
use plugin_writes::WriteUi;
use plugins::PluginsUi;

mod export;
mod import;
mod plugin_ui;
mod plugin_writes;
mod plugins;
#[cfg(feature = "screenshots")]
pub mod scene;
mod update;

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
    /// Where the query history, the saved queries and the AI audit log (`ai-audit.jsonl`) live;
    /// `None` without a config directory.
    pub history_dir: Option<ConfigDir>,
    /// Workbench layout state, loaded by the binary.
    pub workbench: crate::workbench::WorkbenchStartup,
    /// The AI settings and the provider factory, loaded by the binary.
    pub ai: crate::ai::AiStartup,
    /// The plugin host and the book of grants; `PluginsStartup::unavailable()` in a build without plugins.
    pub plugins: crate::plugins::PluginsStartup,
    /// Sources of run-time export formats (plugin exporters); empty in a build without plugins.
    pub export_sources: Vec<Arc<dyn crate::export::FormatSource>>,
    /// The update check service and its events; `UpdateStartup::unavailable()` in a build without update checks.
    pub update: crate::update::UpdateStartup,
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
            .field("ai", &self.ai)
            .field("plugins", &self.plugins)
            .field("update", &self.update)
            .field("export_sources", &self.export_sources.len())
            .finish_non_exhaustive()
    }
}

/// Initialises the component library. The theme is applied afterwards, from the settings.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    crate::grid::view::init_keys(cx);
    crate::editor::view::init_keys(cx);
}

/// Opens the main window and runs the application; returns when the app quits.
///
/// On macOS the platform run loop may end the process instead of returning.
pub fn run(startup: Startup) {
    gpui_platform::application()
        .with_assets(AppAssets)
        .run(move |cx: &mut App| {
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
            let (width, height) = startup_window_size(&startup.workbench.load);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
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

/// The window size to open at: the remembered one when both sides are plausible, else 1280x800.
fn startup_window_size(load: &WorkbenchLoad) -> (f32, f32) {
    match load {
        WorkbenchLoad::Loaded(WorkbenchState {
            window: Some(w), ..
        }) if WINDOW_SIZE_RANGE.contains(&w.width) && WINDOW_SIZE_RANGE.contains(&w.height) => {
            (w.width, w.height)
        }
        _ => (1280., 800.),
    }
}

/// The root view: activity bar, side panel, editor groups, bottom panel and status bar.
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
    /// The text inputs of the AI section, while the settings page is shown.
    ai_inputs: Option<ai::AiInputs>,
    /// The composer and the Deny focus of each AI session tab.
    ai_session_inputs: ai_session::AiSessionInputs,
    /// The inputs, host-channel tasks and runs of the agent tabs (ADR-0051).
    agent_views: agent::AgentViews,
    /// The `ai.toml` saves: one in flight, the latest waiting.
    ai_saves: crate::workspace::ai::SaveQueue,
    /// The scroll position of the settings page.
    pub(crate) settings_scroll: ScrollHandle,
    /// Builds the AI providers compiled in (the binary supplies it).
    ai_factory: Arc<dyn crate::ai::AiProviderFactory>,
    /// The prompt-bar inputs of the query tabs (ADR-0052).
    assist_inputs: assist::AssistInputs,
    /// The cancel signals of the running assist requests, by request id.
    assist_cancels: HashMap<u64, oneshot::Sender<()>>,
    /// Editor edits from the AI actions, applied at the next frame (they need the window).
    assist_edits: Vec<Effect>,
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
    /// The AI audit log the Audit view reads; `None` without a config directory. Opening it
    /// does no I/O; reads run on the blocking pool.
    audit_log: Option<Arc<sqler_ai_tools::AuditLog>>,
    /// The inputs of the history tabs and the history dialogs.
    pub(crate) history_views: HistoryViews,
    /// The editing dialogs and the contexts pushed to the grids.
    edit_ui: EditUi,
    /// The text inputs of the export dialog on screen.
    export_inputs: Option<export::ExportInputs>,
    /// The focus, the typed-id field and the setting editors of the plugin manager.
    plugins_ui: PluginsUi,
    /// The focus of the plugin write approval dialog.
    plugin_writes_ui: WriteUi,
    /// The text editors of the plugin panel on screen.
    plugin_ui_views: PluginUiViews,
    /// The update service, if the build has one (ADR-0066); called only through [`Effect::Update`].
    update_service: Option<Arc<dyn crate::update::UpdateService>>,
    /// The keybinding overrides as loaded; the key dispatcher is rebuilt from them when plugin commands change.
    key_overrides: Result<KeybindingOverrides, String>,
    /// The resizable split of the side panel and the rest, and of the editors and the bottom panel.
    side_split: Entity<ResizableState>,
    panel_split: Entity<ResizableState>,
    /// The layout generation the splits were created for.
    split_generation: u64,
    /// Keeps the split subscriptions alive.
    split_subs: Vec<Subscription>,
    /// The focus handles of the activity bar, the bottom panel and the status bar.
    activity_focus: FocusHandle,
    /// One handle per activity bar item (Connections, Saved Queries, History, AI, Settings).
    activity_items: Vec<FocusHandle>,
    /// The first action item of the status bar.
    status_first: FocusHandle,
    panel_focus: FocusHandle,
    /// The AI chat panel on the right (ADR-0076).
    chat_focus: FocusHandle,
    status_focus: FocusHandle,
    /// The handle that had the focus when a region was left (`F6`).
    region_memory: HashMap<Region, FocusHandle>,
    /// The scroll position of the tab row of each group, and the tab that was active last.
    tab_scroll: HashMap<GroupId, ScrollHandle>,
    last_active: HashMap<GroupId, TabId>,
    /// The shortcut texts the chrome shows.
    hints: Hints,
    /// Where `workbench.toml` is written; the size of the window; what is on disk.
    workbench_dir: Option<ConfigDir>,
    window_size: Option<WindowState>,
    saved_state: Option<WorkbenchState>,
    save_task: Option<Task<()>>,
    save_failed: bool,
    /// What `session.json` held last (read at startup, so startup writes nothing).
    saved_session: Option<SessionState>,
    /// The save of `session.json` in flight, if any.
    session_save: Option<Task<()>>,
    /// Held while `session.json` is written, so a late background write cannot overwrite the quit write.
    session_write: Arc<Mutex<()>>,
    /// The checkpoint loop; dropping it stops it.
    session_task: Task<()>,
    /// A save failed or tabs were dropped and the user was told; tell once per run.
    session_failed: bool,
    session_trimmed: bool,
    /// Keeps the window size observer alive.
    _bounds: Subscription,
    /// Keeps the OS appearance observer alive.
    _appearance: Subscription,
    /// Keeps the quit hook (closes sessions when the app quits any way) alive.
    _quit: Subscription,
}

impl WorkspaceView {
    #[allow(clippy::too_many_lines)] // registers every command set and builds the state
    fn new(mut startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session_load = std::mem::replace(&mut startup.workbench.session, SessionLoad::Missing);
        let sidebar = match startup.connections {
            Ok(store) => SidebarModel::from_store(store, startup.config_dir),
            Err(message) => SidebarModel::with_load_error(message),
        };
        if startup.exit_after_first_frame {
            window.on_next_frame(|_, _| std::process::exit(0));
        }
        // The audit log opens before the workspace is built: the AI surfaces need its sink.
        let audit_log = startup
            .history_dir
            .as_ref()
            .map(|dir| Arc::new(sqler_ai_tools::AuditLog::open(dir)));
        let plugin_writes = startup.plugins.writes.take();
        let plugin_events = startup.plugins.ui_events.take();
        let update_events = startup.update.events.take();
        let update_service = startup.update.service.clone();
        let key_overrides = startup.keybindings.clone();
        let mut ws = Workspace::new(sidebar)
            .with_ai_audit(audit_log.clone().map(|log| {
                crate::ai::AiAudit::new(log).with_external_tools(startup.plugins.ai_tools.clone())
            }))
            .with_settings(startup.settings)
            .with_drivers(startup.connector.driver_ids())
            .with_workbench(&startup.workbench.load)
            .with_ai(crate::ai::AiState::from_startup(&startup.ai))
            .with_plugins(&startup.plugins)
            .with_update(&startup.update)
            .with_export_sources(startup.export_sources);
        // The chat history store (ADR-0077) opens on its own thread and never blocks here.
        let chat_store = startup
            .history_dir
            .as_ref()
            .map(|dir| sqler_config::ChatHistory::open(dir.chat_history_path()));
        if let Some(store) = chat_store.clone() {
            ws = ws.with_chat_history(store);
        }
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
        if let Err(err) = crate::ai::register_commands(&mut commands) {
            eprintln!("sqler: could not register the AI commands: {err}");
        }
        if let Err(err) = crate::plugins::register_commands(&mut commands) {
            eprintln!("sqler: could not register the plugin commands: {err}");
        }
        if let Err(err) = crate::update::register_commands(&mut commands) {
            eprintln!("sqler: could not register the update commands: {err}");
        }
        if let Err(err) = workbench::register_commands(&mut commands) {
            eprintln!("sqler: could not register the workbench commands: {err}");
        }
        if let Err(err) = workbench::register_pane_commands(&mut commands) {
            eprintln!("sqler: could not register the pane commands: {err}");
        }
        let keys = KeyDispatcher::new(Platform::current(), startup.keybindings, &commands);
        let hints = Hints::build(|id| keys.shortcut_display(id));
        let mut palette = PaletteState::default();
        let mut window_size = None;
        if let WorkbenchLoad::Loaded(state) = &startup.workbench.load {
            palette.set_recent(RecentCommands::from_strings(
                &state.recent_commands,
                &commands,
            ));
            window_size.clone_from(&state.window);
        }
        // What is on disk already; startup writes nothing.
        let saved_session = match &session_load {
            SessionLoad::Loaded(state) => Some(state.clone()),
            _ => None,
        };
        let saved_state = ws.workbench_state(palette.recent(), window_size.clone());
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
        let quit = cx.on_app_quit(|view, cx| {
            view.save_workbench_now();
            view.save_session_now(cx);
            view.ws.cancel_all_exports();
            for handle in view.ws.cancel_all_queries() {
                drop(view.bridge.spawn(async move {
                    let _ = handle.cancel().await;
                }));
            }
            // Every agent closes: its endpoint, its connection and its process.
            let (_, in_flight) = view.ws.stop_all_agents();
            for handle in in_flight {
                drop(view.bridge.spawn(async move {
                    let _ = handle.cancel().await;
                }));
            }
            let sessions = view.ws.take_all_sessions();
            view.ws.shutdown_browsers();
            let closed = view.bridge.close_all(sessions, QUIT_HOOK_LIMIT);
            let runs = view.take_catalog_runs(QUIT_HOOK_LIMIT);
            let agents = view.take_agent_runs(QUIT_HOOK_LIMIT);
            async move {
                let _ = closed.await;
                let _ = runs.await;
                let _ = agents.await;
            }
        });
        let history = HistoryService::new(startup.history_dir, HistoryOptions::default());
        let bounds =
            cx.observe_window_bounds(window, |view, window, cx| {
                let b = window.bounds().size;
                let (width, height) = (f32::from(b.width), f32::from(b.height));
                if view.window_size.as_ref().is_none_or(|w| {
                    (w.width - width).abs() > 0.5 || (w.height - height).abs() > 0.5
                }) {
                    view.window_size = Some(WindowState { width, height });
                    view.schedule_save(cx);
                }
            });
        let side_split = cx.new(|_| ResizableState::default());
        let panel_split = cx.new(|_| ResizableState::default());
        let split_generation = ws.layout().generation();
        let mut view = Self {
            ws,
            bridge: SessionBridge::new(startup.connector, startup.secrets, startup.runtime),
            commands,
            keys,
            palette,
            palette_input,
            focus,
            grids: GridViews::default(),
            editors: EditorViews::default(),
            queries: QueryViews::default(),
            tasks: HashMap::new(),
            inputs: None,
            ai_inputs: None,
            ai_session_inputs: ai_session::AiSessionInputs::default(),
            agent_views: agent::AgentViews::default(),
            ai_saves: crate::workspace::ai::SaveQueue::default(),
            settings_scroll: ScrollHandle::new(),
            ai_factory: startup.ai.factory,
            assist_inputs: assist::AssistInputs::default(),
            assist_cancels: HashMap::new(),
            assist_edits: Vec::new(),
            sidebar_focus: cx.focus_handle(),
            sidebar_list: new_list_state(),
            list_entries: Vec::new(),
            catalog_runs: Vec::new(),
            notices: Vec::new(),
            copy_task: None,
            history,
            audit_log,
            history_views: HistoryViews::default(),
            edit_ui: EditUi::new(cx),
            export_inputs: None,
            plugins_ui: PluginsUi::new(window, cx),
            plugin_writes_ui: WriteUi::new(cx),
            plugin_ui_views: PluginUiViews::default(),
            update_service,
            key_overrides,
            side_split,
            panel_split,
            split_generation,
            split_subs: Vec::new(),
            activity_focus: cx.focus_handle(),
            activity_items: (0..ActivityItem::COUNT)
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            status_first: cx.focus_handle().tab_stop(true),
            panel_focus: cx.focus_handle(),
            chat_focus: cx.focus_handle(),
            status_focus: cx.focus_handle(),
            region_memory: HashMap::new(),
            tab_scroll: HashMap::new(),
            last_active: HashMap::new(),
            hints,
            workbench_dir: startup.workbench.dir,
            window_size,
            saved_state,
            save_task: None,
            save_failed: false,
            saved_session,
            session_save: None,
            session_write: Arc::new(Mutex::new(())),
            session_task: Task::ready(()),
            session_failed: false,
            session_trimmed: false,
            _bounds: bounds,
            _appearance: appearance,
            _quit: quit,
        };
        let attach = view.ws.restore_tabs(&session_load);
        if let Some(notice) = view.ws.take_notice() {
            view.notices.push(notice);
        }
        view.apply(attach, cx);
        if let Some(store) = chat_store {
            Self::probe_chat_history(store, cx);
        }
        view.session_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SESSION_CHECKPOINT).await;
                if this.update(cx, Self::checkpoint_session).is_err() {
                    break;
                }
            }
        });
        GridProbe::start_if_requested(window, cx);
        if view.ws.plugins().available() {
            Self::schedule_plugins(window, cx);
        }
        if let Some(inbox) = plugin_writes {
            Self::drain_plugin_writes(inbox, cx);
        }
        if let Some(inbox) = plugin_events {
            Self::drain_plugin_ui(inbox, cx);
        }
        if let Some(inbox) = update_events {
            Self::drain_update_events(inbox, cx);
        }
        if view.ws.update().availability() == crate::update::Availability::Ready {
            Self::schedule_update(window, cx);
        }
        view.bind_splits(cx);
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
    #[allow(clippy::too_many_lines)] // one ordered chain of overlay checks
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.is_held
            && (self.ws.drop_dialog().is_some()
                || (self.ws.import_wizard().is_some()
                    && crate::connections::import::ImportWizard::swallows_held_key(
                        &event.keystroke.key,
                        self.inputs
                            .as_ref()
                            .is_some_and(|inputs| inputs.any_focused(window, cx)),
                    ))
                || self.edit_overlay_open()
                || self.plugin_dialog_open()
                || self.plugin_write_open())
        {
            // A held key (auto-repeat) must never confirm or cancel a DROP step, an edit
            // dialog (the production confirmation among them) or the permission dialog.
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
        if !was_open && self.ws.import_wizard().is_some() {
            // The wizard's keys: arrows, Space, Enter, Backspace. A focused password field keeps
            // them for typing.
            let input = key_input(&event.keystroke);
            let typing = self
                .inputs
                .as_ref()
                .is_some_and(|inputs| inputs.any_focused(window, cx));
            let other = input.ctrl || input.alt || input.cmd;
            if let Some(id) = self
                .import_key(&input.key, input.shift, other, typing)
                .and_then(|id| CommandId::new(id).ok())
            {
                cx.stop_propagation();
                self.run_command(&id, window, cx);
                return;
            }
        }
        if !was_open && self.plugin_write_key(&chord, cx) {
            cx.stop_propagation();
            return;
        }
        if !was_open
            && self.plugin_dialog_open()
            && let Some(command) = GrantPrompt::key_command(&chord)
        {
            // Escape is Deny, also from the typed-id field; no key is ever Approve.
            cx.stop_propagation();
            self.plugin_answer(command, cx);
            return;
        }
        if !was_open
            && self.edit_overlay_open()
            && chord.key() == "escape"
            && chord.modifiers() == Modifiers::default()
        {
            // Escape dismisses the preview, the confirmation, the warning or the close prompt;
            // nothing is written.
            cx.stop_propagation();
            self.edit_escape(cx);
            return;
        }
        if !was_open && self.cell_editor_key_event(event, window, cx) {
            // Enter, Tab, Shift+Tab and Escape belong to the cell editor while it is open.
            cx.stop_propagation();
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
        if !was_open && self.region_key(&chord, window, cx) {
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
            || self.edit_overlay_open()
            || self.ws.export_dialog().is_some();
        let editing = self.cell_editing(cx);
        let input = key_input(&event.keystroke);
        if let Some(format) =
            copy_key_format(&input, Platform::current(), focused, overlay, editing)
        {
            self.copy_grid(format, window, cx);
            return true;
        }
        let Some(action) = grid_key_action(
            &input,
            Platform::current(),
            focused,
            overlay,
            editing,
            |m| table.read(cx).delegate().motion_moves(m),
        ) else {
            return false;
        };
        if !self.edit_workspace_action(action, window, cx) {
            table.update(cx, |state, cx| apply_action(state, action, window, cx));
        }
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
    #[allow(clippy::too_many_lines)] // one routing chain
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
        } else if let Some(action) = workbench::action_for(id) {
            self.palette.close();
            self.run_workbench(action, window, cx);
        } else if let Some(action) = pane_action_for(id, &self.ws) {
            self.palette.close();
            self.dispatch(action, cx);
            editor_focus = true;
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
        } else if id.category() == "plugins" {
            self.palette.close();
            self.run_plugin_command(id, window, cx);
        } else if crate::plugins::contrib::is_plugin_category(id.category()) {
            // A command a plugin contributed; one that is gone (its plugin was switched off) does nothing.
            self.palette.close();
            if !self.run_plugin_contribution(id, window, cx) {
                window.push_notification("This plugin command is not available any more.", cx);
            }
        } else if let Some(command) =
            crate::update::action_for(id, self.ws.settings().settings().check_for_updates)
        {
            self.palette.close();
            self.run_update_command(command, cx);
        } else if let Some(action) = crate::ai::action_for(id, &self.ws) {
            self.palette.close();
            self.dispatch_ai(action, cx);
        } else if id.category() == "ai" {
            // An AI command that needs a provider kind or an open provider editor.
            self.palette.close();
            window.push_notification(
                "This AI command does not apply now. Open Settings > AI to add a provider.",
                cx,
            );
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
                        if !self.edit_workspace_action(grid_action, window, cx) {
                            table.update(cx, |state, cx| {
                                apply_action(state, grid_action, window, cx);
                            });
                        }
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
        if was_open && id.as_str() != OPEN_PALETTE {
            self.palette.record(id);
            self.schedule_save(cx);
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

    /// Runs a layout action through the workspace and carries out the effect.
    pub(crate) fn dispatch_workbench(&mut self, action: WorkbenchAction, cx: &mut Context<Self>) {
        let effect = self.ws.dispatch_workbench(action);
        self.apply(effect, cx);
        self.settle(cx);
    }

    /// The text of an editor changed for the first time since it was saved or opened.
    pub(crate) fn editor_changed(&mut self, tab: TabId, cx: &mut Context<Self>) {
        self.ws.editor_edited(tab);
        cx.notify();
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
        // A restored tab that was just activated connects now.
        let attach = self.ws.attach_active_restored();
        if !matches!(attach, Effect::None) {
            self.apply(attach, cx);
        }
        let ws = &self.ws;
        self.tasks.retain(|request, _| ws.is_request_live(*request));
        if let Some(notice) = self.ws.take_notice() {
            self.notices.push(notice);
        }
        self.sync_plugin_contributions(cx);
        self.grids.retain(self.ws.tabs());
        self.editors.retain(&self.ws);
        self.queries.retain(self.ws.tabs());
        self.history_views.retain(&self.ws);
        self.prune_agents();
        self.sync_sidebar();
        self.schedule_save(cx);
        cx.notify();
    }

    /// Makes the dialog inputs match the dialog on screen.
    fn sync_dialog_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wants = matches!(
            self.ws.dialog(),
            Some(
                Dialog::Form(_)
                    | Dialog::GroupName { .. }
                    | Dialog::TrustHostKey { .. }
                    | Dialog::Import(_)
            )
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

    #[allow(clippy::too_many_lines)] // one arm per effect
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
                let agents = self.take_agent_runs(CLOSE_LIMIT);
                cx.spawn(async move |_, cx| {
                    let _ = closed.await;
                    let _ = runs.await;
                    let _ = agents.await;
                    cx.update(|cx| cx.quit());
                })
                .detach();
            }
            Effect::Quit => {
                // An agent that is still being torn down (AI was turned off a moment ago) is
                // waited for too.
                if self.agents_running() {
                    let agents = self.take_agent_runs(CLOSE_LIMIT);
                    cx.spawn(async move |_, cx| {
                        let _ = agents.await;
                        cx.update(|cx| cx.quit());
                    })
                    .detach();
                } else {
                    cx.quit();
                }
            }
            Effect::ApplySettings => {
                self.ws.save_settings();
                apply(
                    self.ws.appearance(),
                    self.ws.settings().settings().ui_font_size,
                    cx,
                );
            }
            Effect::RunQuery { job, grids, events } => self.start_query_job(job, grids, events, cx),
            Effect::ApplyEdits(job) => self.start_apply_job(job, cx),
            Effect::RecordHistory(_)
            | Effect::SearchHistory(_)
            | Effect::ClearHistory
            | Effect::SaveSavedQueries { .. } => self.apply_history(effect, cx),
            Effect::ReadAudit(read) => self.read_audit(read, cx),
            Effect::PickExportPath { .. } | Effect::RunExport(_) => self.apply_export(effect, cx),
            Effect::Import(work) => self.apply_import(work, cx),
            Effect::SaveAiConfig { .. } | Effect::TestAiProvider { .. } => {
                self.apply_ai(effect, cx);
            }
            Effect::StartAiSession { .. }
            | Effect::RunAiTurn(_)
            | Effect::ListChats
            | Effect::LoadChat(_) => {
                self.apply_ai_session(effect, cx);
            }
            Effect::LoadAgentProfiles | Effect::StartAgent { .. } => self.apply_agent(effect, cx),
            Effect::Plugins(work) => self.apply_plugins(work, cx),
            Effect::PluginUi(work) => self.apply_plugin_ui(work, cx),
            Effect::Update(work) => self.apply_update(work, cx),
            edit @ Effect::EditorEdit { .. } => self.assist_edits.push(edit),
            Effect::RunAssist(job) => self.run_assist_effect(job, cx),
            Effect::CancelAssist(id) => self.cancel_assist_effect(id),
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
        self.dispatch_workbench(WorkbenchAction::ShowPanelView(PanelView::Results), cx);
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

    /// The active tab if it runs through the run gate: a query tab or an AI session tab.
    fn active_gate_tab(&self) -> Option<TabId> {
        if self.ws.settings_tab_active() {
            return None;
        }
        self.ws
            .tabs()
            .active()
            .filter(|t| t.kind().uses_run_gate())
            .map(crate::tabs::Tab::id)
    }

    /// The active query tab while its parameter prompt or confirmation is open.
    fn query_overlay_tab(&self) -> Option<TabId> {
        let tab = self.active_gate_tab()?;
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
            QueryCommand::Explain => match self.editors.buffer(tab, cx) {
                Some(buffer) => self.ws.explain_query(tab, editor::run_snapshot(buffer)),
                None => Effect::None,
            },
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
        self.sync_edit(window, cx);
        self.sync_dialog_inputs(window, cx);
        self.sync_ai_inputs(window, cx);
        self.sync_ai_session_inputs(window, cx);
        self.sync_agent_inputs(window, cx);
        self.sync_assist_inputs(window, cx);
        self.flush_assist_edits(window, cx);
        self.sync_export_inputs(window, cx);
        self.sync_plugin_dialog(window, cx);
        self.sync_plugin_writes(window, cx);
        self.sync_plugin_inputs(window, cx);
        self.sync_plugin_panel_inputs(window, cx);
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
        let shell = self.shell(window, cx);
        let dialog = dialog_overlay(&self.ws, self.inputs.as_ref(), cx);
        let drop_dialog = drop_overlay(self.ws.drop_dialog(), cx);
        let query_dialog = self
            .query_overlay_tab()
            .and_then(|tab| query_overlay(self, tab, cx));
        let history_dialog = history::view::history_overlay(self, cx);
        let edit_dialog = crate::grid::edit::view::edit_overlay(self, cx);
        let export_dialog = export_overlay(self, cx);
        let plugin_dialog = self.plugin_dialog_overlay(cx);
        let plugin_write_dialog = crate::plugins::approval::view::write_overlay(self, cx);
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
            .capture_action(cx.listener(|this, a: &InputEnter, window, cx| {
                if a.shift {
                    cx.propagate();
                } else {
                    this.cell_editor_action(EditorKey::Commit { then: None }, window, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &InputEscape, window, cx| {
                this.cell_editor_action(EditorKey::Cancel, window, cx);
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                let then = Some(Motion::Right);
                this.cell_editor_action(EditorKey::Commit { then }, window, cx);
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                let then = Some(Motion::Left);
                this.cell_editor_action(EditorKey::Commit { then }, window, cx);
            }))
            .capture_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.move_palette_selection(1, cx)),
            )
            .relative()
            .size_full()
            .flex()
            .flex_row()
            .bg(bg)
            .text_color(fg)
            .child(shell)
            .children(dialog)
            .children(drop_dialog)
            .children(query_dialog)
            .children(history_dialog)
            .children(edit_dialog)
            .children(export_dialog)
            .children(plugin_dialog)
            .children(plugin_write_dialog)
            .children(palette)
    }
}

#[cfg(test)]
mod tests {
    use sqler_config::{WindowState, WorkbenchLoad, WorkbenchState};

    use super::startup_window_size;

    fn loaded(width: f32, height: f32) -> WorkbenchLoad {
        WorkbenchLoad::Loaded(WorkbenchState {
            window: Some(WindowState { width, height }),
            ..WorkbenchState::default()
        })
    }

    #[test]
    fn the_window_opens_at_the_remembered_size_when_plausible() {
        assert_eq!(startup_window_size(&loaded(900., 700.)), (900., 700.));
        assert_eq!(startup_window_size(&loaded(10., 700.)), (1280., 800.));
        assert_eq!(startup_window_size(&loaded(900., 99_999.)), (1280., 800.));
        assert_eq!(startup_window_size(&WorkbenchLoad::Missing), (1280., 800.));
    }
}
