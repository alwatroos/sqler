// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Workspace`: sidebar, tab strip and the sessions behind the tabs.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

use sqler_catalog::{CacheKey, CatalogWorker, RefreshHandle};
use sqler_config::{
    AiConfig, ConfigDir, ConnectionId, ConnectionProfile, NewHistoryEntry, PanelState,
    ProviderConfig, SavedQueries, Secret, SecretRef, WINDOW_SIZE_RANGE, WORKBENCH_VERSION,
    WindowState, WorkbenchLoad, WorkbenchState,
};
use sqler_driver_api::sqler_core::{ConnectionConfig, DbObject, DriverId};
use sqler_driver_api::{CancelHandle, Session};
use sqler_sql::Dialect;

use super::action::WorkspaceAction;
use super::browser::{BrowserJob, BrowserState, OpenDataJob};
use super::edit::EditFlow;
use super::export::ExportCenter;
use super::history::HistoryWorkspace;
use super::query::ActiveRun;
use crate::ai::agent::{AgentMsg, AgentStart, AgentTabs};
use crate::ai::session::{AiSessionStart, AiTurnJob};
use crate::ai::{AiAudit, AuditPanel, AuditRead, NO_AUDIT};
use crate::ai::{AiSessions, AiState, AiStatus, AssistJob, EditorAssist, HostMsg, ai_status};
use crate::browser::{DropFlow, ObjectTab, TreeNodeId};
use crate::connections::{
    ConnectFailure, ConnectRequest, ConnectionAction, ConnectionForm, Dialog, FieldError, FormMode,
    GroupPurpose, HostKeyPrompt, SecretField, TestStatus, from_profile_secret,
};
use crate::editor::{EditorDocs, EditorKey, TextEdit};
use crate::export::ExportJob;
use crate::grid::edit::ApplyJob;
use crate::grid::{
    GridCancel, GridTab, InsertTarget, SYNTHETIC_BATCH, SYNTHETIC_COLUMNS, SYNTHETIC_ROWS,
};
use crate::history::HistorySearch;
use crate::plugins::manager::{PluginEffect, PluginsState};
use crate::query::{Phase, QueryJob, QueryTab, RunEvent, StatementStatus};
use crate::settings::SettingsModel;
use crate::sidebar::SidebarModel;
use crate::tabs::{Breadcrumbs, TabConnection, TabDecor, TabId, TabKind, TabStrip};
use crate::theme::{self, Appearance};
use crate::workbench::{
    PanelView, RecentCommands, RecentConnections, RowInfo, RunIndicator, StatusBarModel,
    WorkbenchAction, WorkbenchLayout, database_label, format_duration, format_rows,
};

/// Identifies one connect attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

/// Why a connect attempt was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Open a tab on this saved connection.
    Open(ConnectionId),
    /// The browser session of this connection (it becomes the catalog's session).
    Browse(ConnectionId),
    /// "Test connection" from the form; the session is closed again.
    Test,
}

/// A change to the secret store that follows a save or a delete.
#[derive(Debug, Clone)]
pub enum SecretOp {
    /// Store this secret under the reference.
    Set(SecretRef, Secret),
    /// Delete the entry of the reference.
    Delete(SecretRef),
}

/// What the view has to do after a dispatched action.
#[non_exhaustive]
pub enum Effect {
    /// Nothing.
    None,
    /// Start connecting; report back with [`Workspace::connect_finished`].
    Connect {
        /// The request to report back with.
        request: RequestId,
        /// What to connect to.
        connect: ConnectRequest,
    },
    /// Drop the connect attempt of this request (it was cancelled).
    CancelConnect(RequestId),
    /// The connections changed: call [`Workspace::save_connections`], then apply the secret
    /// changes to the secret store.
    SaveConnections {
        /// Keychain changes to make after the file is written.
        secrets: Vec<SecretOp>,
    },
    /// Add the host key to `known_hosts`, then report with [`Workspace::trust_finished`].
    TrustHostKey {
        /// The key the user confirmed.
        key: HostKeyPrompt,
        /// What to run again afterwards.
        retry: Purpose,
    },
    /// Close this session (its tab is gone).
    CloseSession(Box<dyn Session>),
    /// Close these sessions (the app is quitting), then quit.
    CloseSessions(Vec<Box<dyn Session>>),
    /// Show this grid in the already opened grid tab (it is bound to the active connection).
    OpenGrid {
        /// The grid tab.
        tab: TabId,
        /// The tab state; the view owns it from now on.
        grid: GridTab,
    },
    /// Quit the application now (no session is open).
    Quit,
    /// The settings changed: save them and re-apply the theme and font size.
    ApplySettings,
    /// Carry out these effects in order.
    Batch(Vec<Effect>),
    /// Run the catalog worker of a connection's browser (it owns the browser session).
    StartCatalog {
        /// The connection.
        conn: ConnectionId,
        /// The browser generation the worker belongs to.
        generation: u64,
        /// The worker; run it with `SessionBridge::run_catalog`.
        worker: CatalogWorker,
    },
    /// Load these keys through the catalog and report each with [`Workspace::catalog_loaded`].
    LoadCatalog {
        /// The connection.
        conn: ConnectionId,
        /// The browser generation to report back with.
        generation: u64,
        /// What to load.
        keys: Vec<CacheKey>,
    },
    /// Follow a refresh and report each event with [`Workspace::refresh_event`].
    WatchRefresh {
        /// The connection.
        conn: ConnectionId,
        /// The browser generation to report back with.
        generation: u64,
        /// The node the progress shows on.
        node: TreeNodeId,
        /// Tells this refresh from an earlier one on the same node.
        run: u64,
        /// The refresh to follow.
        handle: RefreshHandle,
    },
    /// Run a job on tokio and report it with [`Workspace::job_finished`].
    RunJob(BrowserJob),
    /// Show this grid in the already opened grid tab and fill it from the database.
    OpenData {
        /// The grid tab.
        tab: TabId,
        /// The tab state; the view owns it from now on.
        grid: GridTab,
        /// The query that feeds the grid.
        job: OpenDataJob,
    },
    /// Put this text on the clipboard.
    Copy(String),
    /// Show the commands of the selected object (the context menu cannot open from a key).
    ShowObjectMenu,
    /// Replace a range of the editor text as one undo step, then select `select_after`.
    EditorEdit {
        /// The editor tab.
        tab: TabId,
        /// The replacement.
        edit: TextEdit,
    },
    /// Make this range the only selection of the editor.
    EditorSelect {
        /// The editor tab.
        tab: TabId,
        /// The range to select.
        range: Range<usize>,
    },
    /// Add this range as one more selection of the editor.
    EditorAddSelection {
        /// The editor tab.
        tab: TabId,
        /// The range to add.
        range: Range<usize>,
    },
    /// Forward a key the editor handles itself.
    EditorKey {
        /// The editor tab.
        tab: TabId,
        /// The key.
        key: EditorKey,
    },
    /// Run a query job on tokio, show its grids in the query tab and follow its events with
    /// [`Workspace::query_event`]; when the events end, report with [`Workspace::query_finished`].
    RunQuery {
        /// The job; run it with `SessionBridge::spawn`.
        job: QueryJob,
        /// One grid per statement, in the job's order.
        grids: Vec<GridTab>,
        /// The progress of the job.
        events: tokio::sync::mpsc::UnboundedReceiver<RunEvent>,
    },
    /// Ask the database to cancel the running statement.
    CancelQuery(CancelHandle),
    /// Run an apply job on tokio (`SessionBridge::spawn` with `ApplyJob::run`), mark the grid
    /// with `GridEditor::begin_apply` first, then report with [`Workspace::apply_finished`].
    ApplyEdits(ApplyJob),
    /// Read a page of the AI audit log on the blocking pool, then report with
    /// [`Workspace::audit_loaded`].
    ReadAudit(AuditRead),
    /// Record this statement in the query history on a blocking thread, then report with
    /// [`Workspace::history_recorded`].
    RecordHistory(NewHistoryEntry),
    /// Search the query history on a blocking thread, then report the entries with
    /// [`Workspace::history_results`] (with the search's tab and request).
    SearchHistory(HistorySearch),
    /// Delete the whole query history on a blocking thread, then report with
    /// [`Workspace::history_cleared`].
    ClearHistory,
    /// Write the saved queries to `queries.toml` on a blocking thread (`HistoryService::save_saved`
    /// skips an older `generation`), then report with [`Workspace::saved_queries_written`].
    SaveSavedQueries {
        /// The number of the change this snapshot holds.
        generation: u64,
        /// The queries to write.
        queries: SavedQueries,
    },
    /// Ask for the target file of an export (the native save dialog), then report with
    /// [`Workspace::export_path_chosen`].
    PickExportPath {
        /// The folder to start in.
        directory: PathBuf,
        /// The suggested file name.
        suggested_name: String,
        /// The dialog this prompt belongs to; hand it back to `export_path_chosen`.
        dialog: u64,
    },
    /// Start this export; report with [`Workspace::export_progress`] and
    /// [`Workspace::export_finished`].
    RunExport(ExportJob),
    /// Write `ai.toml` (when there is a `dir`) on a blocking thread, then apply the keychain
    /// changes, then report the file's result with [`Workspace::ai_saved`]. The keychain is not
    /// touched when the file could not be written.
    SaveAiConfig {
        /// Where `ai.toml` lives; `None` keeps the change for this session only.
        dir: Option<ConfigDir>,
        /// The configuration to write.
        config: AiConfig,
        /// Keychain changes to make after the file is written.
        secrets: Vec<SecretOp>,
    },
    /// Test a provider: build it on the blocking pool, then list its models within 15 seconds on
    /// tokio, and report with [`Workspace::ai_test_finished`].
    TestAiProvider {
        /// Tells this test from an older one.
        request: u64,
        /// The provider to test.
        provider: ProviderConfig,
        /// A typed key that is not stored yet.
        key: Option<Secret>,
    },
    /// Build the session of a new AI session tab on tokio (`SessionBridge::spawn` with
    /// `build_session`), report with [`Workspace::ai_session_ready`], and follow the host channel
    /// with [`Workspace::ai_session_message`].
    StartAiSession {
        /// What to build the session from.
        start: Box<AiSessionStart>,
        /// Everything the session tells the tab, in order.
        host: tokio::sync::mpsc::UnboundedReceiver<HostMsg>,
    },
    /// Run one turn of an AI session on tokio (`SessionBridge::spawn` with `AiTurnJob::run`). The
    /// session comes back in `HostMsg::TurnDone`; a lost job is reported with
    /// [`Workspace::ai_turn_lost`].
    RunAiTurn(Box<AiTurnJob>),
    /// Read the agent profiles (`load_agent_profiles` on the blocking pool, from the config
    /// directory) and report with [`Workspace::agent_profiles_loaded`].
    LoadAgentProfiles,
    /// Start an agent (`SessionBridge::spawn` with `start_agent`; the view fills `audit` and
    /// `cwd` first), report with [`Workspace::agent_ready`], follow the host channel with
    /// [`Workspace::agent_message`] and keep `finished` to wait for the process at quit.
    StartAgent {
        /// What to start the agent from.
        start: Box<AgentStart>,
        /// Everything the agent tells the tab, in order.
        host: tokio::sync::mpsc::UnboundedReceiver<AgentMsg>,
    },
    /// Run an editor assist request on tokio (`run_assist` through `SessionBridge::spawn`), then
    /// report with [`Workspace::assist_finished`]. Its answer never runs: generated SQL goes into
    /// the editor through `EditorEdit` only.
    RunAssist(Box<AssistJob>),
    /// Send the cancel signal of this assist request.
    CancelAssist(u64),
    /// Run this work of the plugin manager on the background executor and report the result with
    /// [`Workspace::plugin_finished`] (`SessionBridge::blocking` with `PluginEffect::run_blocking`; a
    /// `Start` runs the backend future on tokio).
    Plugins(PluginEffect),
}

impl std::fmt::Debug for Effect {
    #[allow(clippy::too_many_lines)] // one arm per variant
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Connect { request, connect } => f
                .debug_struct("Connect")
                .field("request", request)
                .field("connect", connect)
                .finish(),
            Self::CancelConnect(request) => f.debug_tuple("CancelConnect").field(request).finish(),
            Self::SaveConnections { secrets } => f
                .debug_struct("SaveConnections")
                .field("secrets", secrets)
                .finish(),
            Self::TrustHostKey { key, retry } => f
                .debug_struct("TrustHostKey")
                .field("key", key)
                .field("retry", retry)
                .finish(),
            Self::CloseSession(_) => f.write_str("CloseSession(..)"),
            Self::CloseSessions(sessions) => write!(f, "CloseSessions({})", sessions.len()),
            Self::OpenGrid { tab, .. } => f.debug_struct("OpenGrid").field("tab", tab).finish(),
            Self::Quit => f.write_str("Quit"),
            Self::ApplySettings => f.write_str("ApplySettings"),
            Self::Batch(effects) => f.debug_tuple("Batch").field(effects).finish(),
            Self::StartCatalog {
                conn, generation, ..
            } => f
                .debug_struct("StartCatalog")
                .field("conn", conn)
                .field("generation", generation)
                .finish_non_exhaustive(),
            Self::LoadCatalog {
                conn,
                generation,
                keys,
            } => f
                .debug_struct("LoadCatalog")
                .field("conn", conn)
                .field("generation", generation)
                .field("keys", &keys.len())
                .finish(),
            Self::WatchRefresh {
                conn,
                generation,
                node,
                run,
                ..
            } => f
                .debug_struct("WatchRefresh")
                .field("conn", conn)
                .field("generation", generation)
                .field("node", node)
                .field("run", run)
                .finish_non_exhaustive(),
            Self::RunJob(job) => f.debug_tuple("RunJob").field(job).finish(),
            Self::OpenData { tab, .. } => f.debug_struct("OpenData").field("tab", tab).finish(),
            Self::Copy(text) => write!(f, "Copy({} bytes)", text.len()),
            Self::ShowObjectMenu => f.write_str("ShowObjectMenu"),
            Self::EditorEdit { tab, edit } => f
                .debug_struct("EditorEdit")
                .field("tab", tab)
                .field("edit", edit)
                .finish(),
            Self::EditorSelect { tab, range } => f
                .debug_struct("EditorSelect")
                .field("tab", tab)
                .field("range", range)
                .finish(),
            Self::EditorAddSelection { tab, range } => f
                .debug_struct("EditorAddSelection")
                .field("tab", tab)
                .field("range", range)
                .finish(),
            Self::EditorKey { tab, key } => f
                .debug_struct("EditorKey")
                .field("tab", tab)
                .field("key", key)
                .finish(),
            Self::RunQuery { job, grids, .. } => f
                .debug_struct("RunQuery")
                .field("job", job)
                .field("grids", &grids.len())
                .finish_non_exhaustive(),
            Self::CancelQuery(_) => f.write_str("CancelQuery(..)"),
            // Never the plan: only which grid and which apply.
            Self::ApplyEdits(job) => f
                .debug_struct("ApplyEdits")
                .field("key", &job.key)
                .field("run", &job.run)
                .finish_non_exhaustive(),
            // Never the SQL text.
            Self::RecordHistory(entry) => f
                .debug_struct("RecordHistory")
                .field("connection", &entry.connection)
                .field("outcome", &entry.outcome)
                .finish_non_exhaustive(),
            Self::SearchHistory(search) => f
                .debug_struct("SearchHistory")
                .field("tab", &search.tab)
                .field("request", &search.request)
                .finish_non_exhaustive(),
            Self::ReadAudit(read) => f
                .debug_struct("ReadAudit")
                .field("request", &read.request)
                .finish(),
            Self::ClearHistory => f.write_str("ClearHistory"),
            Self::SaveSavedQueries {
                generation,
                queries,
            } => f
                .debug_struct("SaveSavedQueries")
                .field("generation", generation)
                .field("queries", &queries.queries().len())
                .finish(),
            // Never the folder or the file name.
            Self::PickExportPath { .. } => f.write_str("PickExportPath"),
            Self::RunExport(job) => f.debug_tuple("RunExport").field(&job.tab).finish(),
            // Ids and counts only: never the endpoint, the models or a key.
            Self::SaveAiConfig {
                dir,
                config,
                secrets,
            } => f
                .debug_struct("SaveAiConfig")
                .field("persistent", &dir.is_some())
                .field("providers", &config.providers.len())
                .field("secrets", &secrets.len())
                .finish(),
            Self::TestAiProvider {
                request,
                provider,
                key,
            } => f
                .debug_struct("TestAiProvider")
                .field("request", request)
                .field("provider", &provider.id.as_str())
                .field("key", &key.is_some())
                .finish(),
            // Ids only: never the message, the SQL or the provider.
            Self::StartAiSession { start, .. } => f
                .debug_struct("StartAiSession")
                .field("tab", &start.tab)
                .finish_non_exhaustive(),
            Self::RunAiTurn(job) => f
                .debug_struct("RunAiTurn")
                .field("tab", &job.tab)
                .field("turn", &job.turn)
                .finish_non_exhaustive(),
            Self::LoadAgentProfiles => f.write_str("LoadAgentProfiles"),
            // Ids only: never the profile's `env`.
            Self::StartAgent { start, .. } => f
                .debug_struct("StartAgent")
                .field("tab", &start.tab)
                .field("profile", &start.profile.id)
                .finish_non_exhaustive(),
            // Never the question or the SQL: the job's own Debug has the id, tab and kind only.
            Self::RunAssist(job) => f
                .debug_struct("RunAssist")
                .field("id", &job.id)
                .field("tab", &job.tab)
                .field("kind", &job.request.kind())
                .finish(),
            Self::CancelAssist(id) => f.debug_tuple("CancelAssist").field(id).finish(),
            // Ids and keys only: the effect's own Debug never shows a setting value.
            Self::Plugins(effect) => f.debug_tuple("Plugins").field(effect).finish(),
        }
    }
}

/// The settings tab sits after the last connection tab; it is not a `TabStrip` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    /// Not open.
    Closed,
    /// Open, but a connection tab is shown.
    Background,
    /// Open and shown.
    Active,
}

/// The workspace state. Holds no UI types.
pub struct Workspace {
    pub(super) sidebar: SidebarModel,
    pub(super) tabs: TabStrip,
    sessions: HashMap<TabId, Box<dyn Session>>,
    pub(super) requests: HashMap<RequestId, Purpose>,
    next_request: u64,
    /// The drivers offered by the connection form.
    drivers: Vec<DriverId>,
    /// The dialog on screen.
    pub(super) dialog: Option<Dialog>,
    /// The form hidden behind a host-key prompt.
    parked: Option<Dialog>,
    /// Counts the dialogs opened, so a view can tell a new dialog from the same one.
    dialog_generation: u64,
    /// A message for the user that no dialog shows (a failed move, a delete of a full group).
    pub(super) notice: Option<String>,
    settings: SettingsModel,
    settings_tab: SettingsTab,
    system_appearance: Appearance,
    /// Cancel flags of the grid tabs' ingestion threads.
    pub(super) grid_cancels: HashMap<TabId, GridCancel>,
    /// The table or view behind each open-data tab.
    pub(super) grid_objects: HashMap<TabId, DbObject>,
    /// Rows of the synthetic grid; a field so tests can use a small result.
    synthetic_rows: usize,
    /// The object browser of every connection that was opened.
    pub(super) browsers: HashMap<ConnectionId, BrowserState>,
    /// The last browser generation handed out.
    pub(super) next_generation: u64,
    /// The last refresh id handed out.
    pub(super) next_refresh: u64,
    /// The content of the DDL and properties tabs.
    pub(super) object_tabs: HashMap<TabId, ObjectTab>,
    /// The DROP confirmation on screen.
    pub(super) drop_flow: Option<DropFlow>,
    /// The folder to refresh once the DROP has run, with the browser generation it belongs to.
    pub(super) drop_parent: Option<(u64, TreeNodeId)>,
    /// The headless state of the SQL editor tabs.
    pub(super) editors: EditorDocs,
    /// The saved connection behind every editor and object tab.
    pub(super) tab_conns: HashMap<TabId, ConnectionId>,
    /// The breadcrumb segments of the object tabs (DDL, Properties, data), after the connection.
    pub(super) object_crumbs: HashMap<TabId, Vec<String>>,
    /// Start text for the editor that the connect attempt of this request opens.
    pub(super) editor_seeds: HashMap<RequestId, String>,
    /// Start text of an open that stopped at the host-key prompt, until the retry runs.
    pub(super) retry_seeds: HashMap<ConnectionId, String>,
    /// The run state of every query tab.
    pub(super) queries: HashMap<TabId, QueryTab>,
    /// The query runs in progress; the session of such a tab is on loan to the job.
    pub(super) query_runs: HashMap<TabId, ActiveRun>,
    /// The last run id handed out.
    pub(super) next_run: u64,
    /// The history tabs, saved queries and their dialogs.
    pub(super) history: HistoryWorkspace,
    /// The AI audit log viewer.
    pub(super) audit: AuditPanel,
    /// The sink every AI tool call is audited into; `None` turns the AI surfaces off.
    pub(super) ai_audit: Option<AiAudit>,
    /// Inline editing: pending changes, the transaction flags, the apply flow.
    pub(super) edit: EditFlow,
    /// The export dialog and the exports in progress.
    pub(super) exports: ExportCenter,
    /// Panel visibility, sizes and views.
    pub(super) layout: WorkbenchLayout,
    /// `workbench.toml` is from a newer `SQLer`: keep it as it is.
    workbench_newer: bool,
    /// The connections opened last, most recent first.
    recent_connections: RecentConnections,
    /// The AI settings: providers, models, drafts and test status.
    pub(super) ai: AiState,
    /// The AI session tabs.
    pub(super) ai_sessions: AiSessions,
    /// The agent tabs and the profiles they pick from (ADR-0051).
    pub(super) agents: AgentTabs,
    /// The editor assistant of every query tab (ADR-0052).
    pub(super) assist: EditorAssist,
    /// The plugin manager: the rows, the open permission dialog and the work it asks for (ADR-0060).
    pub(super) plugins: PluginsState,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("sidebar", &self.sidebar)
            .field("tabs", &self.tabs)
            .field("sessions", &self.sessions.len())
            .field("requests", &self.requests)
            .field("dialog", &self.dialog)
            .field("settings_tab", &self.settings_tab)
            .field("grids", &self.grid_cancels.len())
            .field("browsers", &self.browsers.len())
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// Creates a workspace with no tabs.
    #[must_use]
    pub fn new(sidebar: SidebarModel) -> Self {
        Self {
            sidebar,
            tabs: TabStrip::default(),
            sessions: HashMap::new(),
            requests: HashMap::new(),
            next_request: 0,
            drivers: Vec::new(),
            dialog: None,
            parked: None,
            dialog_generation: 0,
            notice: None,
            settings: SettingsModel::default(),
            settings_tab: SettingsTab::Closed,
            system_appearance: Appearance::Light,
            grid_cancels: HashMap::new(),
            grid_objects: HashMap::new(),
            synthetic_rows: SYNTHETIC_ROWS,
            browsers: HashMap::new(),
            next_generation: 0,
            object_tabs: HashMap::new(),
            drop_flow: None,
            drop_parent: None,
            next_refresh: 0,
            editors: EditorDocs::new(),
            tab_conns: HashMap::new(),
            object_crumbs: HashMap::new(),
            editor_seeds: HashMap::new(),
            retry_seeds: HashMap::new(),
            queries: HashMap::new(),
            query_runs: HashMap::new(),
            next_run: 0,
            history: HistoryWorkspace::new(),
            audit: AuditPanel::default(),
            ai_audit: None,
            edit: EditFlow::default(),
            exports: ExportCenter::default(),
            layout: WorkbenchLayout::default(),
            workbench_newer: false,
            recent_connections: RecentConnections::default(),
            ai: AiState::default(),
            ai_sessions: AiSessions::default(),
            agents: AgentTabs::default(),
            assist: EditorAssist::default(),
            plugins: PluginsState::default(),
        }
    }

    /// Applies what `workbench.toml` held: the layout and the recent connections. A file from a
    /// newer `SQLer` keeps the default layout and is never overwritten; a file that cannot be read
    /// gives the default layout and a notice.
    #[must_use]
    pub fn with_workbench(mut self, load: &WorkbenchLoad) -> Self {
        match load {
            WorkbenchLoad::Missing => {}
            WorkbenchLoad::Loaded(state) => {
                self.layout = WorkbenchLayout::from_state(state);
                self.recent_connections =
                    RecentConnections::from_strings(&state.recent_connections, &self.sidebar);
            }
            WorkbenchLoad::Newer(_) => self.workbench_newer = true,
            WorkbenchLoad::Invalid(message) => {
                self.notice = Some(format!(
                    "Could not read workbench.toml, using the default layout \
                     (the file is copied to workbench.toml.bak before it is replaced): {message}"
                ));
            }
        }
        self
    }

    /// The panel layout.
    #[must_use]
    pub fn layout(&self) -> &WorkbenchLayout {
        &self.layout
    }

    /// Applies a layout action. The layout needs no I/O, so the effect is always none.
    pub fn dispatch_workbench(&mut self, action: WorkbenchAction) -> Effect {
        if action == WorkbenchAction::ShowPanelView(PanelView::Audit) {
            return self.open_audit();
        }
        self.layout.apply(action);
        Effect::None
    }

    /// The connections opened last, most recent first.
    #[must_use]
    pub fn recent_connections(&self) -> &RecentConnections {
        &self.recent_connections
    }

    /// The state to write to `workbench.toml`; `None` when the file is from a newer `SQLer`.
    #[must_use]
    pub fn workbench_state(
        &self,
        recent: &RecentCommands,
        window: Option<WindowState>,
    ) -> Option<WorkbenchState> {
        if self.workbench_newer {
            return None;
        }
        let l = &self.layout;
        Some(WorkbenchState {
            version: WORKBENCH_VERSION,
            side_panel: PanelState {
                visible: l.side_visible(),
                size: l.side_width(),
                view: l.side_view().as_str().to_owned(),
            },
            bottom_panel: PanelState {
                visible: l.panel_visible(),
                size: l.panel_height(),
                view: l.panel_view().as_str().to_owned(),
            },
            window: window.filter(|w| {
                WINDOW_SIZE_RANGE.contains(&w.width) && WINDOW_SIZE_RANGE.contains(&w.height)
            }),
            recent_commands: recent.to_strings(),
            recent_connections: self
                .recent_connections
                .iter()
                .filter(|id| self.sidebar.index_of(id).is_some())
                .map(|id| id.as_str().to_owned())
                .collect(),
        })
    }

    /// A tab header with the live state the strip decorates it with.
    #[must_use]
    pub fn tab_decor(&self, id: TabId) -> Option<TabDecor> {
        let mut header = self.tabs.get(id)?.header();
        let dirty = self.editors.is_dirty(id) || self.edit.pending_notice(id).is_some();
        let running = self.queries.get(&id).is_some_and(QueryTab::is_running);
        header.preview &= !dirty;
        Some(TabDecor {
            header,
            dirty,
            running,
        })
    }

    /// What AI does for the connection behind a tab; `None` when the tab has no saved connection.
    fn tab_ai_status(&self, id: TabId) -> Option<AiStatus> {
        let profile = self.sidebar.profile(self.tab_conns.get(&id)?)?;
        ai_status(self.ai.config(), Some(profile.spec()))
    }

    /// The database a tab's connection points at, as the status bar and breadcrumbs show it.
    fn tab_database(&self, id: TabId) -> Option<String> {
        let conn = self.tabs.get(id)?.connection();
        self.tab_conns
            .get(&id)
            .and_then(|c| self.sidebar.index_of(c))
            .and_then(|i| self.sidebar.entries().get(i))
            .and_then(|e| database_label(&conn.driver, &e.config().target))
    }

    /// The breadcrumbs of a tab: the connection, then the database (query tabs) or the object
    /// (other tabs, falling back to the tab title).
    #[must_use]
    pub fn breadcrumbs(&self, id: TabId) -> Option<Breadcrumbs> {
        let tab = self.tabs.get(id)?;
        let conn = tab.connection();
        let mut items = vec![conn.name.clone()];
        if tab.kind() == TabKind::Query {
            items.extend(self.tab_database(id));
        } else if tab.kind() == TabKind::AiSession {
            items.push("AI session".to_owned());
        } else if tab.kind() == TabKind::Agent {
            items.push(
                self.agents
                    .get(id)
                    .and_then(|a| a.profile().map(|p| p.name.clone()))
                    .unwrap_or_else(|| "Agent".to_owned()),
            );
        } else if let Some(crumbs) = self.object_crumbs.get(&id) {
            items.extend(crumbs.iter().cloned());
        } else {
            items.push(tab.title());
        }
        Some(Breadcrumbs {
            items,
            env: conn.environment,
        })
    }

    /// What the status bar shows for the active tab. `rows` describes the result in view.
    #[must_use]
    pub fn status_bar(&self, rows: Option<RowInfo>) -> StatusBarModel {
        let Some(tab) = self.tabs.active() else {
            return StatusBarModel::default();
        };
        let id = tab.id();
        let conn = tab.connection();
        let database = self.tab_database(id);
        let query = self.queries.get(&id);
        let run = match query.map(QueryTab::phase) {
            Some(Phase::Running { cancelling, .. }) => {
                if *cancelling {
                    RunIndicator::Cancelling
                } else {
                    RunIndicator::Running
                }
            }
            _ => RunIndicator::Idle,
        };
        let timing = query.and_then(|q| {
            q.results().iter().rev().find_map(|r| match &r.status {
                StatementStatus::Done { duration, .. }
                | StatementStatus::Failed { duration, .. } => Some(format_duration(*duration)),
                _ => None,
            })
        });
        StatusBarModel {
            env: conn.environment,
            connection: Some(conn.name.clone()),
            accent: tab.header().accent,
            database,
            run,
            rows: rows.map(format_rows),
            timing,
            ai: self.tab_ai_status(id),
            agent_endpoints: self.agent_endpoints(),
        }
    }

    /// Closes `ids` one by one through the normal close path; stops at the first pending-changes
    /// prompt so later tabs do not close behind the dialog. Unlike closing a single tab, a batch
    /// also treats an editor with unsaved text as pending changes (refinement of Q3).
    fn close_many(&mut self, ids: Vec<TabId>) -> Effect {
        let mut effects = Vec::new();
        for id in ids {
            if self.close_prompt().is_some() {
                break;
            }
            if self.tabs.get(id).is_some()
                && !self.edit.is_applying(id)
                && self.editors.is_dirty(id)
            {
                self.edit.text_prompt(id);
                break;
            }
            match self.request_close_tab(id) {
                Effect::None => {}
                effect => effects.push(effect),
            }
        }
        match effects.len() {
            0 => Effect::None,
            1 => effects.swap_remove(0),
            _ => Effect::Batch(effects),
        }
    }

    /// The tabs of the group `id` is in, in order, with the position of `id`.
    fn group_tabs_of(&self, id: TabId) -> Option<(Vec<TabId>, usize)> {
        let group = self.tabs.group_of(id)?;
        let tabs = self
            .tabs
            .groups()
            .iter()
            .find(|g| g.id() == group)?
            .tabs()
            .to_vec();
        let at = tabs.iter().position(|t| *t == id)?;
        Some((tabs, at))
    }

    /// Refuses a split with a notice.
    fn split_right(&mut self) {
        if self.tabs.split_right().is_some() {
            self.show_connection_tabs();
        } else if self.tabs.groups().len() >= crate::tabs::MAX_GROUPS {
            self.notice = Some(format!(
                "At most {} editor groups.",
                crate::tabs::MAX_GROUPS
            ));
        } else {
            self.notice = Some("Split needs two or more tabs in the group.".to_owned());
        }
    }

    /// Runs a tab-strip change that moves or focuses tabs; the connection tabs come to the front
    /// when it worked.
    fn tabs_changed(&mut self, changed: bool) -> Effect {
        if changed {
            self.show_connection_tabs();
        }
        Effect::None
    }

    /// Sets the drivers the connection form offers (the connector's `driver_ids`).
    #[must_use]
    pub fn with_drivers(mut self, drivers: Vec<DriverId>) -> Self {
        self.drivers = drivers;
        self
    }

    /// The drivers the connection form offers.
    #[must_use]
    pub fn drivers(&self) -> &[DriverId] {
        &self.drivers
    }

    /// The dialog on screen, if any.
    #[must_use]
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    /// How many dialogs have been opened so far; it changes whenever a dialog replaces another
    /// (a form restored after a host-key prompt keeps its number).
    #[must_use]
    pub fn dialog_generation(&self) -> u64 {
        self.dialog_generation
    }

    /// Where a copy as SQL `INSERT` of this tab's grid points: the real table for an open-data
    /// tab, else the placeholder table. The dialect is the tab connection's. `None` if the tab
    /// is gone.
    #[must_use]
    pub fn insert_target(&self, tab: TabId) -> Option<InsertTarget> {
        let connection = self.tabs.get(tab)?.connection();
        let dialect = Dialect::from_driver_id(connection.driver.as_str());
        Some(match self.grid_objects.get(&tab) {
            Some(object) => InsertTarget::for_object(dialect, object),
            None => InsertTarget::placeholder(dialect),
        })
    }

    /// Takes the pending message for the user, if any.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Whether the connect attempt of this request is still wanted.
    #[must_use]
    pub fn is_request_live(&self, request: RequestId) -> bool {
        self.requests.contains_key(&request)
    }

    /// Saves the connection list (a no-op when it is not persistent).
    ///
    /// # Errors
    /// The write error as text.
    pub fn save_connections(&mut self) -> Result<(), String> {
        self.sidebar.save()
    }

    /// Saves the connection list, then decides which keychain changes may run.
    ///
    /// The keychain is touched only after `connections.toml` was written; when the file was not
    /// loaded (or there is no config directory) or the write failed, no change is applied, so an
    /// entry of a connection that is still in the file is never deleted and no orphan is
    /// created. Returns the operations to apply and the notices to show.
    pub fn commit_connections(&mut self, secrets: Vec<SecretOp>) -> (Vec<SecretOp>, Vec<String>) {
        if !self.sidebar.persistent() {
            if secrets.is_empty() {
                return (Vec::new(), Vec::new());
            }
            return (
                Vec::new(),
                vec!["Connections are not saved; the keychain was not changed".to_owned()],
            );
        }
        match self.save_connections() {
            Ok(()) => (secrets, Vec::new()),
            Err(message) => {
                let mut notices = vec![format!("Could not save connections: {message}")];
                if !secrets.is_empty() {
                    notices.push("The keychain was not changed".to_owned());
                }
                (Vec::new(), notices)
            }
        }
    }

    /// Replaces the settings (the binary loads them at startup).
    #[must_use]
    pub fn with_settings(mut self, settings: SettingsModel) -> Self {
        self.settings = settings;
        self
    }

    /// Sets the audit sink of the AI surfaces. Without one they are off ([`NO_AUDIT`]).
    #[must_use]
    pub fn with_ai_audit(mut self, audit: Option<AiAudit>) -> Self {
        self.ai_audit = audit;
        self
    }

    /// The audit sink of the AI surfaces.
    ///
    /// # Errors
    /// [`NO_AUDIT`] when the host has none.
    pub(crate) fn ai_audit(&self) -> Result<&AiAudit, &'static str> {
        self.ai_audit.as_ref().ok_or(NO_AUDIT)
    }

    /// The settings.
    #[must_use]
    pub fn settings(&self) -> &SettingsModel {
        &self.settings
    }

    /// Saves the settings (a no-op when they are not persistent).
    pub fn save_settings(&mut self) {
        self.settings.save();
    }

    /// The look in use: the theme preference resolved against the OS appearance.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        theme::resolve(self.settings.settings().theme, self.system_appearance)
    }

    /// Records the OS appearance; `true` if [`appearance`](Self::appearance) changed.
    pub fn set_system_appearance(&mut self, system: Appearance) -> bool {
        let before = self.appearance();
        self.system_appearance = system;
        self.appearance() != before
    }

    /// Whether the settings tab is open.
    #[must_use]
    pub fn settings_tab_open(&self) -> bool {
        self.settings_tab != SettingsTab::Closed
    }

    /// Whether the settings tab is the one shown.
    #[must_use]
    pub fn settings_tab_active(&self) -> bool {
        self.settings_tab == SettingsTab::Active
    }

    /// Uses a smaller synthetic result in tests.
    #[cfg(test)]
    pub(crate) fn set_synthetic_rows(&mut self, rows: usize) {
        self.synthetic_rows = rows;
    }

    /// The sidebar state.
    #[must_use]
    pub fn sidebar(&self) -> &SidebarModel {
        &self.sidebar
    }

    /// The tab strip.
    #[must_use]
    pub fn tabs(&self) -> &TabStrip {
        &self.tabs
    }

    /// Applies an action and returns what the view must do next.
    #[allow(clippy::too_many_lines)] // one arm per action
    pub fn dispatch(&mut self, action: WorkspaceAction) -> Effect {
        match action {
            WorkspaceAction::SelectConnection(index) => {
                self.select_connection_row(index);
                Effect::None
            }
            WorkspaceAction::OpenConnection(index) => self.open_connection(index),
            WorkspaceAction::ActivateTab(id) => {
                if self.tabs.activate(id) && self.settings_tab == SettingsTab::Active {
                    self.settings_tab = SettingsTab::Background;
                }
                Effect::None
            }
            WorkspaceAction::CloseTab(id) => self.request_close_tab(id),
            WorkspaceAction::CloseActiveTab => {
                if self.settings_tab == SettingsTab::Active {
                    self.settings_tab = SettingsTab::Closed;
                    return Effect::None;
                }
                match self.tabs.active().map(crate::tabs::Tab::id) {
                    Some(id) => self.request_close_tab(id),
                    None => Effect::None,
                }
            }
            WorkspaceAction::NextTab => {
                self.step_tab(true);
                Effect::None
            }
            WorkspaceAction::PreviousTab => {
                self.step_tab(false);
                Effect::None
            }
            WorkspaceAction::OpenSyntheticGrid => self.open_synthetic_grid(),
            WorkspaceAction::Quit => self.request_quit(),
            WorkspaceAction::OpenSettings => {
                self.settings_tab = SettingsTab::Active;
                Effect::None
            }
            WorkspaceAction::CloseSettings => {
                self.settings_tab = SettingsTab::Closed;
                Effect::None
            }
            WorkspaceAction::ToggleThemeMode => {
                let pref = self.settings.settings().theme;
                let next = theme::toggled(pref, self.system_appearance);
                Self::changed(self.settings.set_theme(next))
            }
            WorkspaceAction::SetTheme(pref) => Self::changed(self.settings.set_theme(pref)),
            WorkspaceAction::SetUiFontSize(size) => {
                Self::changed(self.settings.set_ui_font_size(size))
            }
            WorkspaceAction::PinTab(id) => {
                self.tabs.pin(id);
                Effect::None
            }
            WorkspaceAction::CloseOtherTabs(id) => match self.group_tabs_of(id) {
                Some((tabs, _)) => self.close_many(tabs.into_iter().filter(|t| *t != id).collect()),
                None => Effect::None,
            },
            WorkspaceAction::CloseTabsToRight(id) => match self.group_tabs_of(id) {
                Some((tabs, at)) => self.close_many(tabs[at + 1..].to_vec()),
                None => Effect::None,
            },
            WorkspaceAction::CloseAllTabs => {
                let focused = self.tabs.focused_group();
                let tabs = self
                    .tabs
                    .groups()
                    .iter()
                    .find(|g| g.id() == focused)
                    .map(|g| g.tabs().to_vec())
                    .unwrap_or_default();
                self.close_many(tabs)
            }
            WorkspaceAction::MoveTab { id, group, index } => {
                let moved = self.tabs.move_tab(id, group, index);
                self.tabs_changed(moved)
            }
            WorkspaceAction::MoveActiveTabLeft => {
                let moved = self.tabs.move_active_by(-1);
                self.tabs_changed(moved)
            }
            WorkspaceAction::MoveActiveTabRight => {
                let moved = self.tabs.move_active_by(1);
                self.tabs_changed(moved)
            }
            WorkspaceAction::SplitRight => {
                self.split_right();
                Effect::None
            }
            WorkspaceAction::FocusGroup(group) => {
                let changed = self.tabs.focus_group(group);
                self.tabs_changed(changed)
            }
            WorkspaceAction::FocusNextGroup => {
                let changed = self.tabs.focus_adjacent_group(true);
                self.tabs_changed(changed)
            }
            WorkspaceAction::FocusPreviousGroup => {
                let changed = self.tabs.focus_adjacent_group(false);
                self.tabs_changed(changed)
            }
            WorkspaceAction::MoveActiveTabToNextGroup => {
                let moved = self.tabs.move_active_to_adjacent_group(true);
                self.tabs_changed(moved)
            }
            WorkspaceAction::MoveActiveTabToPreviousGroup => {
                let moved = self.tabs.move_active_to_adjacent_group(false);
                self.tabs_changed(moved)
            }
        }
    }

    /// Quits for real: stops every run and apply, ends the grid ingestions and returns what the
    /// view has to close.
    pub(super) fn quit_now(&mut self) -> Effect {
        self.cancel_all_exports();
        let mut cancels = self.cancel_all_queries();
        cancels.extend(self.edit.stop_all_applies());
        cancels.extend(self.cancel_ai_sessions());
        // Closes every agent: each tears down its endpoint and process; the view waits for them.
        let (agents_ran, agent_cancels) = self.stop_all_agents();
        cancels.extend(agent_cancels);
        // Ends every grid ingestion, waking one that is paused on a full page.
        for (_, grid) in self.grid_cancels.drain() {
            grid.cancel();
        }
        let sessions = self.take_all_sessions();
        let browsers = self.shutdown_browsers();
        if sessions.is_empty() && !browsers && cancels.is_empty() && !agents_ran {
            Effect::Quit
        } else if cancels.is_empty() {
            Effect::CloseSessions(sessions)
        } else {
            // A session lent to a query or an apply is cancelled and dropped at exit.
            let mut effects: Vec<Effect> = cancels.into_iter().map(Effect::CancelQuery).collect();
            effects.push(Effect::CloseSessions(sessions));
            Effect::Batch(effects)
        }
    }

    fn changed(changed: bool) -> Effect {
        if changed {
            Effect::ApplySettings
        } else {
            Effect::None
        }
    }

    /// Moves to the next or previous tab; the settings tab, when open, comes after the last
    /// connection tab.
    fn step_tab(&mut self, forward: bool) {
        let focused = self.tabs.focused_group();
        let in_first_group = self
            .tabs
            .groups()
            .first()
            .is_some_and(|g| g.id() == focused);
        let group_tabs: Vec<TabId> = self
            .tabs
            .groups()
            .iter()
            .find(|g| g.id() == focused)
            .map(|g| g.tabs().to_vec())
            .unwrap_or_default();
        let first = group_tabs.first().copied();
        let last = group_tabs.last().copied();
        let active = self.tabs.active().map(crate::tabs::Tab::id);
        let plain = |ws: &mut Self| {
            if forward {
                ws.tabs.next();
            } else {
                ws.tabs.previous();
            }
        };
        if !in_first_group {
            plain(self);
            self.show_connection_tabs();
            return;
        }
        match self.settings_tab {
            SettingsTab::Closed => plain(self),
            SettingsTab::Active => {
                // With no connection tabs the settings tab stays active.
                if let Some(id) = if forward { first } else { last } {
                    self.tabs.activate(id);
                    self.settings_tab = SettingsTab::Background;
                }
            }
            SettingsTab::Background => {
                if active == if forward { last } else { first } {
                    self.settings_tab = SettingsTab::Active;
                } else {
                    plain(self);
                }
            }
        }
    }

    /// Opens a grid tab on the active tab's connection, else the selected sidebar entry's.
    fn open_synthetic_grid(&mut self) -> Effect {
        let connection = match self.tabs.active() {
            Some(tab) => tab.connection().clone(),
            None => match self
                .sidebar
                .selected()
                .and_then(|i| self.sidebar.entries().get(i))
            {
                Some(entry) => tab_connection(entry.config()),
                None => return Effect::None,
            },
        };
        let tab = self.tabs.open(connection, TabKind::Grid);
        let (grid, cancel) =
            GridTab::spawn_synthetic(self.synthetic_rows, SYNTHETIC_COLUMNS, SYNTHETIC_BATCH);
        self.grid_cancels.insert(tab, cancel);
        Effect::OpenGrid { tab, grid }
    }

    pub(super) fn open_connection(&mut self, index: usize) -> Effect {
        let Some(id) = self.sidebar.entries().get(index).map(|e| e.id().clone()) else {
            return Effect::None;
        };
        let Some(profile) = self.sidebar.profile(&id) else {
            return Effect::None;
        };
        let connect = match ConnectRequest::from_profile(profile) {
            Ok(connect) => connect,
            Err(err) => {
                self.sidebar.begin_connect(index);
                self.sidebar.finish_connect(index, Err(err.to_string()));
                return Effect::None;
            }
        };
        self.sidebar.begin_connect(index);
        self.sidebar.select(index);
        let request = self.new_request(Purpose::Open(id));
        Effect::Connect { request, connect }
    }

    /// Shows the connection tabs when the settings tab was in front.
    pub(super) fn show_connection_tabs(&mut self) {
        if self.settings_tab == SettingsTab::Active {
            self.settings_tab = SettingsTab::Background;
        }
    }

    pub(super) fn new_request(&mut self, purpose: Purpose) -> RequestId {
        let request = RequestId(self.next_request);
        self.next_request += 1;
        self.requests.insert(request, purpose);
        request
    }

    /// Closes the tab now, without asking about pending changes.
    pub(super) fn close_tab_now(&mut self, id: TabId) -> Effect {
        if !self.tabs.close(id) {
            return Effect::None;
        }
        self.edit.forget_tab(id);
        if let Some(cancel) = self.grid_cancels.remove(&id) {
            cancel.cancel();
        }
        self.grid_objects.remove(&id);
        self.close_object_tab(id);
        self.editors.remove(id);
        self.tab_conns.remove(&id);
        self.object_crumbs.remove(&id);
        self.queries.remove(&id);
        self.history.forget_tab(id);
        self.cancel_tab_export(id);
        let cancel = self.cancel_query_run(id);
        if self.tabs.is_empty() && self.settings_tab == SettingsTab::Background {
            self.settings_tab = SettingsTab::Active;
        }
        // A session on loan comes back through `query_finished`, which closes it.
        let closing = match (self.sessions.remove(&id), cancel) {
            (Some(session), _) => Effect::CloseSession(session),
            (None, Some(handle)) => Effect::CancelQuery(handle),
            (None, None) => Effect::None,
        };
        let ai = super::ai_session::join(self.close_ai_session(id), self.close_agent(id));
        match (closing, ai) {
            (Effect::None, ai) => ai,
            (closing, Effect::None) => closing,
            (closing, ai) => Effect::Batch(vec![closing, ai]),
        }
    }

    /// Reports the outcome of a connect attempt.
    ///
    /// An opened connection gets a query tab, activated, with the session stored under it. A
    /// test closes its session at once and shows the result in the form. A host key the user has
    /// not trusted yet opens the trust dialog; any other failure is shown on the sidebar entry or
    /// in the form. A result nobody waits for any more closes its session.
    pub fn connect_finished(
        &mut self,
        request: RequestId,
        result: Result<Box<dyn Session>, ConnectFailure>,
    ) -> Effect {
        let seed = self.editor_seeds.remove(&request);
        let rerun = self.ai_sessions.reruns.remove(&request);
        let pending = self.history.take_pending(request);
        let before = self.tabs.active().map(crate::tabs::Tab::id);
        let Some(purpose) = self.requests.remove(&request) else {
            return match result {
                Ok(session) => Effect::CloseSession(session),
                Err(_) => Effect::None,
            };
        };
        match purpose {
            Purpose::Open(id) => {
                let effect = self.open_finished(&id, result, seed);
                let effect = self.after_history_open(before, pending, effect);
                match rerun {
                    // An AI session's re-run: the new editor runs the statement it was seeded with.
                    Some(sql) => self.rerun_opened(before, effect, sql),
                    None => effect,
                }
            }
            Purpose::Browse(id) => self.browse_finished(&id, result),
            Purpose::Test => self.test_finished(request, result),
        }
    }

    fn open_finished(
        &mut self,
        id: &ConnectionId,
        result: Result<Box<dyn Session>, ConnectFailure>,
        seed: Option<String>,
    ) -> Effect {
        let Some(index) = self.sidebar.index_of(id) else {
            // The connection was deleted while connecting.
            return match result {
                Ok(session) => Effect::CloseSession(session),
                Err(_) => Effect::None,
            };
        };
        match result {
            Ok(session) => {
                self.sidebar.finish_connect(index, Ok(()));
                let connection = tab_connection(self.sidebar.entries()[index].config());
                let dialect = Dialect::from_driver_id(connection.driver.as_str());
                let tab = self.tabs.open(connection, TabKind::Query);
                self.sessions.insert(tab, session);
                self.tab_conns.insert(tab, id.clone());
                self.editors.open(tab, dialect, seed);
                self.queries.insert(tab, QueryTab::new());
                self.recent_connections.record(id);
                self.show_connection_tabs();
                return self.ensure_browser(id);
            }
            Err(failure) => {
                self.sidebar.finish_connect(index, Err(failure.to_string()));
                if let ConnectFailure::UnknownHostKey(key) = failure {
                    match seed {
                        Some(seed) => self.retry_seeds.insert(id.clone(), seed),
                        None => self.retry_seeds.remove(id),
                    };
                    self.ask_host_key(key, Purpose::Open(id.clone()));
                }
            }
        }
        Effect::None
    }

    fn test_finished(
        &mut self,
        request: RequestId,
        result: Result<Box<dyn Session>, ConnectFailure>,
    ) -> Effect {
        let running = |dialog: &Option<Dialog>| {
            matches!(
                dialog,
                Some(Dialog::Form(form)) if form.test == TestStatus::Running(request)
            )
        };
        // The form may be on screen or parked behind the host-key prompt.
        let on_screen = running(&self.dialog);
        if !on_screen && !running(&self.parked) {
            return match result {
                Ok(session) => Effect::CloseSession(session),
                Err(_) => Effect::None,
            };
        }
        let slot = if on_screen {
            &mut self.dialog
        } else {
            &mut self.parked
        };
        let Some(Dialog::Form(form)) = slot else {
            return Effect::None;
        };
        match result {
            Ok(session) => {
                form.test = TestStatus::Ok;
                Effect::CloseSession(session)
            }
            Err(ConnectFailure::UnknownHostKey(key)) => {
                form.test = TestStatus::Idle;
                if on_screen {
                    self.ask_host_key(key, Purpose::Test);
                } else {
                    // Another prompt is already showing; the user can press Test again.
                    self.notice = Some(
                        "SSH host key needs confirmation; press Test again after the current prompt"
                            .to_owned(),
                    );
                }
                Effect::None
            }
            Err(failure) => {
                form.test = TestStatus::Failed(failure.to_string());
                Effect::None
            }
        }
    }

    /// Shows the trust dialog; a form on screen waits behind it. Any other dialog on screen
    /// (a confirmation, a name or a move dialog) or an earlier prompt is kept: the new prompt is
    /// not shown over it, and the user gets a notice to try again.
    fn ask_host_key(&mut self, key: HostKeyPrompt, retry: Purpose) {
        match &self.dialog {
            None => {}
            Some(Dialog::Form(_)) => self.parked = self.dialog.take(),
            Some(_) => {
                self.notice = Some(
                    "SSH host key needs confirmation; close this dialog and connect again"
                        .to_owned(),
                );
                return;
            }
        }
        self.dialog = Some(Dialog::TrustHostKey { key, retry });
    }

    /// Reports whether the host key was added to `known_hosts`. On success the attempt that
    /// found the key unknown runs again.
    pub fn trust_finished(&mut self, retry: Purpose, result: Result<(), ConnectFailure>) -> Effect {
        match (retry, result) {
            (Purpose::Open(id), Ok(())) => {
                let seed = self.retry_seeds.remove(&id);
                match self.sidebar.index_of(&id) {
                    Some(index) => {
                        let effect = self.open_connection(index);
                        if let (Effect::Connect { request, .. }, Some(seed)) = (&effect, seed) {
                            self.editor_seeds.insert(*request, seed);
                        }
                        effect
                    }
                    None => Effect::None,
                }
            }
            (Purpose::Test, Ok(())) => self.dispatch_connection(ConnectionAction::Test),
            (Purpose::Browse(id), Ok(())) => {
                // A host key is trusted before the first tab opens, so this is not expected;
                // opening the connection again starts the browser.
                self.browsers.remove(&id);
                Effect::None
            }
            (Purpose::Open(_) | Purpose::Browse(_), Err(failure)) => {
                self.notice = Some(failure.to_string());
                Effect::None
            }
            (Purpose::Test, Err(failure)) => {
                if let Some(Dialog::Form(form)) = &mut self.dialog {
                    form.test = TestStatus::Failed(failure.to_string());
                }
                Effect::None
            }
        }
    }

    /// Whether a session is stored for the tab.
    #[must_use]
    pub fn has_session(&self, id: TabId) -> bool {
        self.sessions.contains_key(&id)
    }

    /// Gives a session back to its tab (a query job returns it).
    pub(super) fn restore_session(&mut self, id: TabId, session: Box<dyn Session>) {
        self.sessions.insert(id, session);
    }

    /// Takes the tab's session out (for example to close it on quit).
    pub fn take_session(&mut self, id: TabId) -> Option<Box<dyn Session>> {
        self.sessions.remove(&id)
    }

    /// Takes every open session out (to close them on quit).
    pub fn take_all_sessions(&mut self) -> Vec<Box<dyn Session>> {
        let mut sessions: Vec<Box<dyn Session>> =
            self.sessions.drain().map(|(_, session)| session).collect();
        sessions.extend(self.shut_ai_sources());
        sessions.extend(self.shut_agent_sources());
        sessions
    }

    // ---- connection manager ----

    /// Applies a connection-manager action and returns what the view must do next.
    pub fn dispatch_connection(&mut self, action: ConnectionAction) -> Effect {
        use ConnectionAction as A;
        match action {
            A::New => {
                let group = self.default_group();
                let form = ConnectionForm::new_connection(self.drivers.first().cloned(), group);
                self.open_dialog(Dialog::Form(form))
            }
            A::Edit(id) => match self.sidebar.profile(&id) {
                Some(profile) => {
                    let form = ConnectionForm::edit(profile);
                    self.open_dialog(Dialog::Form(form))
                }
                None => Effect::None,
            },
            A::Delete(id) => {
                let Some(name) = self.sidebar.profile(&id).map(|p| p.spec().name.clone()) else {
                    return Effect::None;
                };
                self.open_dialog(Dialog::ConfirmDelete { id, name })
            }
            A::ConfirmDelete => self.confirm_delete(),
            A::Test => self.start_test(),
            A::CancelTest => self.cancel_test(),
            A::Save => self.save_form(),
            A::Cancel => self.cancel_dialog(),
            A::SetText { field, text } => self.edit_form(|f| f.set_text(field, text)),
            A::SetDriver(driver) => self.edit_form(|f| f.set_driver(driver)),
            A::SetEnvironment(env) => self.edit_form(|f| f.set_environment(env)),
            A::SetGroup(group) => self.edit_form(|f| f.set_group(group)),
            A::SetTlsMode(mode) => self.edit_form(|f| f.set_tls_mode(mode)),
            A::SetSshEnabled(on) => self.edit_form(|f| f.set_ssh_enabled(on)),
            A::SetSshAuth(auth) => self.edit_form(|f| f.set_ssh_auth(auth)),
            A::SetAiEnabled(on) => self.edit_form(|f| f.set_ai_enabled(on)),
            A::SetAiSendRows(rows) => self.edit_form(|f| f.set_ai_send_rows(rows)),
            A::ClearSecret(slot) => self.edit_form(|f| f.clear_secret(slot)),
            A::OpenMoveTo(id) => {
                if self.sidebar.profile(&id).is_none() {
                    return Effect::None;
                }
                self.open_dialog(Dialog::MoveTo { id })
            }
            A::MoveConnection { id, to, index } => self.move_connection(&id, to.as_ref(), index),
            A::NewGroup { parent } => self.open_dialog(Dialog::GroupName {
                purpose: GroupPurpose::New { parent },
                text: String::new(),
                error: None,
            }),
            A::RenameGroup(id) => {
                let Some(name) = self.sidebar.store().group(&id).map(|g| g.name().to_owned())
                else {
                    return Effect::None;
                };
                self.open_dialog(Dialog::GroupName {
                    purpose: GroupPurpose::Rename(id),
                    text: name,
                    error: None,
                })
            }
            A::SetGroupName(typed) => {
                if let Some(Dialog::GroupName { text, error, .. }) = &mut self.dialog {
                    *text = typed;
                    *error = None;
                }
                Effect::None
            }
            A::SubmitGroupName => self.submit_group_name(),
            A::DeleteGroup(id) => match self.sidebar.delete_group(&id) {
                Ok(_) => Effect::SaveConnections {
                    secrets: Vec::new(),
                },
                Err(err) => {
                    self.notice = Some(err.to_string());
                    Effect::None
                }
            },
            A::ToggleGroup(id) => {
                self.sidebar.toggle_group(&id);
                Effect::None
            }
            A::SelectGroup(id) => {
                self.sidebar.select_group(&id);
                Effect::None
            }
            A::TrustHostKey => self.trust_host_key(),
            A::RejectHostKey => self.reject_host_key(),
        }
    }

    /// The group a new connection or group starts in: the selected group, else the group of the
    /// selected connection.
    fn default_group(&self) -> Option<sqler_config::GroupId> {
        if let Some(group) = self.sidebar.selected_group() {
            return Some(group.clone());
        }
        let id = self.sidebar.selected_id()?;
        self.sidebar.profile(id)?.group().cloned()
    }

    /// Cancels a running test of the form on screen.
    fn stop_running_test(&mut self) -> Effect {
        if let Some(Dialog::Form(form)) = &mut self.dialog
            && let TestStatus::Running(request) = form.test
        {
            form.test = TestStatus::Idle;
            self.requests.remove(&request);
            return Effect::CancelConnect(request);
        }
        Effect::None
    }

    /// Shows a dialog in place of the current one.
    fn open_dialog(&mut self, dialog: Dialog) -> Effect {
        // A DROP confirmation that has not started is dropped; two dialogs never overlap.
        if self.drop_flow.as_ref().is_some_and(DropFlow::can_cancel) {
            self.drop_flow = None;
            self.drop_parent = None;
        }
        let effect = self.stop_running_test();
        self.parked = None;
        self.dialog_generation += 1;
        self.dialog = Some(dialog);
        effect
    }

    fn edit_form(&mut self, change: impl FnOnce(&mut ConnectionForm)) -> Effect {
        if let Some(Dialog::Form(form)) = &mut self.dialog {
            change(form);
        }
        Effect::None
    }

    fn cancel_dialog(&mut self) -> Effect {
        if matches!(self.dialog, Some(Dialog::TrustHostKey { .. })) {
            return self.reject_host_key();
        }
        let effect = self.stop_running_test();
        self.dialog = None;
        self.parked = None;
        effect
    }

    fn start_test(&mut self) -> Effect {
        if !matches!(self.dialog, Some(Dialog::Form(_))) {
            let Some(id) = self.sidebar.selected_id().cloned() else {
                return Effect::None;
            };
            let Some(profile) = self.sidebar.profile(&id) else {
                return Effect::None;
            };
            let form = ConnectionForm::edit(profile);
            self.open_dialog(Dialog::Form(form));
        }
        let Some(Dialog::Form(form)) = &mut self.dialog else {
            return Effect::None;
        };
        if matches!(form.test, TestStatus::Running(_)) {
            return Effect::None;
        }
        match form.connect_request() {
            Err(errors) => {
                form.set_errors(errors);
                Effect::None
            }
            Ok(connect) => {
                form.set_errors(Vec::new());
                let request = RequestId(self.next_request);
                self.next_request += 1;
                form.test = TestStatus::Running(request);
                self.requests.insert(request, Purpose::Test);
                Effect::Connect { request, connect }
            }
        }
    }

    fn cancel_test(&mut self) -> Effect {
        self.stop_running_test()
    }

    fn save_form(&mut self) -> Effect {
        let Some(Dialog::Form(form)) = &mut self.dialog else {
            return Effect::None;
        };
        let valid = match form.validate() {
            Ok(valid) => valid,
            Err(errors) => {
                form.set_errors(errors);
                return Effect::None;
            }
        };
        let old = form.original().cloned();
        let mode = form.mode().clone();
        let saved = match &mode {
            FormMode::New => self
                .sidebar
                .add_connection(valid.spec.clone(), valid.group.as_ref()),
            FormMode::Edit(id) => self.update_existing(id, &valid, old.as_ref()),
        };
        let id = match saved {
            Ok(id) => id,
            Err(err) => {
                if let Some(Dialog::Form(form)) = &mut self.dialog {
                    form.set_errors(vec![FieldError::from_store(&err)]);
                }
                return Effect::None;
            }
        };
        if matches!(mode, FormMode::Edit(_)) {
            // The settings may have changed: the next open starts a browser on the new ones.
            self.end_browser(&id);
        }
        let secrets = match self.sidebar.profile(&id) {
            Some(new) => secret_ops(old.as_ref(), new, &valid.db_password, &valid.ssh_secret),
            None => Vec::new(),
        };
        self.sidebar.select_connection(&id);
        let cancel = self.stop_running_test();
        self.dialog = None;
        self.parked = None;
        drop(cancel);
        Effect::SaveConnections { secrets }
    }

    fn update_existing(
        &mut self,
        id: &ConnectionId,
        valid: &crate::connections::ValidForm,
        old: Option<&ConnectionProfile>,
    ) -> Result<ConnectionId, sqler_config::StoreError> {
        self.sidebar.update_connection(id, valid.spec.clone())?;
        let moved = old.is_none_or(|p| p.group() != valid.group.as_ref());
        if moved {
            self.sidebar
                .move_connection(id, valid.group.as_ref(), usize::MAX)?;
        }
        Ok(id.clone())
    }

    fn confirm_delete(&mut self) -> Effect {
        let Some(Dialog::ConfirmDelete { id, .. }) = &self.dialog else {
            return Effect::None;
        };
        let id = id.clone();
        self.dialog = None;
        match self.sidebar.delete_connection(&id) {
            Ok(profile) => {
                self.end_browser(&id);
                let secrets = profile
                    .secret_refs()
                    .iter()
                    .filter_map(|s| from_profile_secret(s).ok())
                    .map(SecretOp::Delete)
                    .collect();
                Effect::SaveConnections { secrets }
            }
            Err(err) => {
                self.notice = Some(err.to_string());
                Effect::None
            }
        }
    }

    fn move_connection(
        &mut self,
        id: &ConnectionId,
        to: Option<&sqler_config::GroupId>,
        index: usize,
    ) -> Effect {
        if matches!(self.dialog, Some(Dialog::MoveTo { .. })) {
            self.dialog = None;
        }
        match self.sidebar.move_connection(id, to, index) {
            Ok(()) => Effect::SaveConnections {
                secrets: Vec::new(),
            },
            Err(err) => {
                self.notice = Some(err.to_string());
                Effect::None
            }
        }
    }

    fn submit_group_name(&mut self) -> Effect {
        let Some(Dialog::GroupName { purpose, text, .. }) = &self.dialog else {
            return Effect::None;
        };
        let result = match purpose {
            GroupPurpose::New { parent } => {
                self.sidebar.add_group(text, parent.as_ref()).map(|_| ())
            }
            GroupPurpose::Rename(id) => self.sidebar.rename_group(id, text),
        };
        match result {
            Ok(()) => {
                self.dialog = None;
                Effect::SaveConnections {
                    secrets: Vec::new(),
                }
            }
            Err(err) => {
                if let Some(Dialog::GroupName { error, .. }) = &mut self.dialog {
                    *error = Some(err.to_string());
                }
                Effect::None
            }
        }
    }

    fn trust_host_key(&mut self) -> Effect {
        let Some(Dialog::TrustHostKey { .. }) = &self.dialog else {
            return Effect::None;
        };
        let Some(Dialog::TrustHostKey { key, retry }) = self.dialog.take() else {
            return Effect::None;
        };
        self.dialog = self.parked.take();
        Effect::TrustHostKey { key, retry }
    }

    fn reject_host_key(&mut self) -> Effect {
        let Some(Dialog::TrustHostKey { retry, .. }) = &self.dialog else {
            return Effect::None;
        };
        let for_test = *retry == Purpose::Test;
        if let Purpose::Open(id) = retry {
            self.retry_seeds.remove(id);
        }
        self.dialog = self.parked.take();
        // Only the test that asked is marked; a form with a test of its own (running, or already
        // finished while parked) keeps that state.
        if let Some(Dialog::Form(form)) = &mut self.dialog
            && for_test
            && form.test == TestStatus::Idle
        {
            form.test = TestStatus::Failed("the SSH host key was not trusted".to_owned());
        }
        Effect::None
    }
}

/// The keychain changes that bring the store in line with a saved profile: entries of the old
/// profile that the new one no longer references are deleted, typed values are stored.
fn secret_ops(
    old: Option<&ConnectionProfile>,
    new: &ConnectionProfile,
    db: &SecretField,
    ssh: &SecretField,
) -> Vec<SecretOp> {
    let refs = |profile: &ConnectionProfile| -> Vec<SecretRef> {
        profile
            .secret_refs()
            .iter()
            .filter_map(|s| from_profile_secret(s).ok())
            .collect()
    };
    let new_refs = refs(new);
    let mut ops: Vec<SecretOp> = old
        .map(refs)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !new_refs.contains(r))
        .map(SecretOp::Delete)
        .collect();
    let typed = |secret: Option<sqler_config::ProfileSecret>, plan: &SecretField| {
        let reference = from_profile_secret(&secret?).ok()?;
        match plan {
            SecretField::Set(value) => Some(SecretOp::Set(reference, value.clone())),
            _ => None,
        }
    };
    ops.extend(typed(new.password_ref(), db));
    ops.extend(typed(new.ssh_secret_ref(), ssh));
    ops
}

/// The tab header data of a connection.
pub(super) fn tab_connection(config: &ConnectionConfig) -> TabConnection {
    TabConnection {
        name: config.name.clone(),
        driver: config.driver.clone(),
        color: config.color.clone(),
        environment: config.environment,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::connections::{FormField, SecretSource, SshAuthKind};
    use crate::sidebar::ConnectionStatus;
    use crate::workbench::{
        RecentCommands, RowInfo, RunIndicator, StatusBarModel, WorkbenchAction,
    };
    use crate::workspace::CloseTarget;
    use crate::workspace::testing::FakeSession;
    use sqler_config::{SecretKind, ThemePreference};
    use sqler_config::{WORKBENCH_VERSION, WindowState, WorkbenchLoad, WorkbenchState};
    use sqler_driver_api::sqler_core::{DriverId, Environment};

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev)
                .with_color("#00ff00"),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
        ]))
    }

    /// Reports a successful connect and returns the tab it opened.
    fn finish_ok(w: &mut Workspace, request: RequestId) -> TabId {
        let (session, _) = FakeSession::boxed();
        // The first successful open also asks for the browser session.
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::None | Effect::Connect { .. }
        ));
        w.tabs().active().map(crate::tabs::Tab::id).unwrap()
    }

    fn connect(ws: &mut Workspace, index: usize) -> RequestId {
        match ws.dispatch(WorkspaceAction::OpenConnection(index)) {
            Effect::Connect { request, .. } => request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    #[test]
    fn select_sets_selection_and_ignores_out_of_range() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::SelectConnection(1)),
            Effect::None
        ));
        assert_eq!(w.sidebar().selected(), Some(1));
        w.dispatch(WorkspaceAction::SelectConnection(7));
        assert_eq!(w.sidebar().selected(), Some(1));
    }

    #[test]
    fn open_then_ok_opens_active_tab_with_session() {
        let mut w = ws();
        let Effect::Connect { request, connect } = w.dispatch(WorkspaceAction::OpenConnection(0))
        else {
            panic!("expected Connect");
        };
        assert_eq!(connect.spec.name, "dev");
        assert_eq!(
            *w.sidebar().entries()[0].status(),
            ConnectionStatus::Connecting
        );
        let tab = finish_ok(&mut w, request);
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tab));
        assert!(w.has_session(tab));
        assert_eq!(*w.sidebar().entries()[0].status(), ConnectionStatus::Idle);
        let header = w.tabs().get(tab).unwrap().header();
        assert_eq!(header.title, "dev");
        assert_eq!(header.badge, Some("DEV"));
    }

    #[test]
    fn err_marks_failed_and_opens_no_tab() {
        let mut w = ws();
        let request = connect(&mut w, 1);
        let failure = ConnectFailure::Tunnel("nope".into());
        assert!(matches!(
            w.connect_finished(request, Err(failure)),
            Effect::None
        ));
        assert!(w.tabs().is_empty());
        assert_eq!(
            *w.sidebar().entries()[1].status(),
            ConnectionStatus::Failed("SSH tunnel failed: nope".into())
        );
    }

    #[test]
    fn unknown_request_drops_session() {
        let mut w = ws();
        let request = connect(&mut w, 0);
        finish_ok(&mut w, request);
        // The same request id again is unknown now: its session is closed, no tab opens.
        let (session, _) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::CloseSession(_)
        ));
        assert_eq!(w.tabs().len(), 1);
    }

    #[test]
    fn several_tabs_per_connection() {
        let mut w = ws();
        for _ in 0..2 {
            let request = connect(&mut w, 0);
            finish_ok(&mut w, request);
        }
        let titles: Vec<_> = w.tabs().iter().map(crate::tabs::Tab::title).collect();
        assert_eq!(titles, ["dev", "dev (2)"]);
    }

    #[test]
    fn close_tab_returns_session_to_close() {
        let mut w = ws();
        let request = connect(&mut w, 0);
        let (session, closed) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::Connect { .. }
        ));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        let Effect::CloseSession(session) = w.dispatch(WorkspaceAction::CloseTab(tab)) else {
            panic!("expected CloseSession");
        };
        assert!(w.tabs().is_empty());
        assert!(!w.has_session(tab));
        assert!(!closed.load(Ordering::SeqCst));
        drop(session);
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseTab(tab)),
            Effect::None
        ));
    }

    #[test]
    fn close_active_tab_and_switching() {
        let mut w = ws();
        let mut tabs = Vec::new();
        for i in [0, 1, 0] {
            let request = connect(&mut w, i);
            tabs.push(finish_ok(&mut w, request));
        }
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[1]));
        w.dispatch(WorkspaceAction::PreviousTab);
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[2]));
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::CloseSession(_)
        ));
        // No right neighbour: the left one becomes active.
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tabs[1]));
        assert_eq!(w.tabs().len(), 2);
    }

    #[test]
    fn close_active_without_tabs_is_noop() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::None
        ));
    }

    #[test]
    fn open_out_of_range_is_noop() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::OpenConnection(5)),
            Effect::None
        ));
    }

    fn open_tabs(w: &mut Workspace, n: usize) -> Vec<TabId> {
        (0..n)
            .map(|_| {
                let request = connect(w, 0);
                finish_ok(w, request)
            })
            .collect()
    }

    fn active(w: &Workspace) -> Option<TabId> {
        w.tabs().active().map(crate::tabs::Tab::id)
    }

    #[test]
    fn open_settings_activates_and_close_closes() {
        let mut w = ws();
        assert!(!w.settings_tab_open());
        assert!(matches!(
            w.dispatch(WorkspaceAction::OpenSettings),
            Effect::None
        ));
        assert!(w.settings_tab_open() && w.settings_tab_active());
        w.dispatch(WorkspaceAction::CloseSettings);
        assert!(!w.settings_tab_open() && !w.settings_tab_active());
    }

    #[test]
    fn close_active_tab_closes_settings_only_when_active() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::OpenSettings);
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::None
        ));
        assert!(!w.settings_tab_open());
        assert_eq!(w.tabs().len(), 2);
        // Background: the connection tab closes and settings stays open.
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[1]));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::CloseSession(_)
        ));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert_eq!(w.tabs().len(), 1);
    }

    #[test]
    fn activate_tab_and_connect_move_settings_to_background() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        w.dispatch(WorkspaceAction::OpenSettings);
        let request = connect(&mut w, 1);
        let tab = finish_ok(&mut w, request);
        assert!(w.settings_tab_open() && !w.settings_tab_active());
        assert_eq!(active(&w), Some(tab));
    }

    #[test]
    fn activate_unknown_tab_keeps_settings_active() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        assert!(w.settings_tab_active());
    }

    #[test]
    fn next_and_previous_cycle_through_settings_tab() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::OpenSettings);
        // Active -> next -> first connection tab.
        w.dispatch(WorkspaceAction::NextTab);
        assert!(!w.settings_tab_active());
        assert_eq!(active(&w), Some(tabs[0]));
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(active(&w), Some(tabs[1]));
        // From the last connection tab -> settings.
        w.dispatch(WorkspaceAction::NextTab);
        assert!(w.settings_tab_active());
        // Previous mirrors it.
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(!w.settings_tab_active());
        assert_eq!(active(&w), Some(tabs[1]));
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(active(&w), Some(tabs[0]));
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(w.settings_tab_active());
        w.dispatch(WorkspaceAction::PreviousTab);
        assert_eq!(active(&w), Some(tabs[1]));
    }

    #[test]
    fn next_with_settings_closed_wraps_connection_tabs() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::NextTab);
        assert_eq!(active(&w), Some(tabs[0]));
        assert!(!w.settings_tab_open());
    }

    #[test]
    fn settings_stays_active_without_connection_tabs() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::NextTab);
        assert!(w.settings_tab_active());
        w.dispatch(WorkspaceAction::PreviousTab);
        assert!(w.settings_tab_active());
    }

    #[test]
    fn closing_last_connection_tab_activates_open_settings() {
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::OpenSettings);
        w.dispatch(WorkspaceAction::ActivateTab(tabs[0]));
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        assert!(w.settings_tab_active());
        // With settings closed nothing is activated.
        let mut w = ws();
        let tabs = open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::CloseTab(tabs[0]));
        assert!(!w.settings_tab_open());
    }

    #[test]
    fn toggle_theme_mode_stores_explicit_opposite() {
        let mut w = ws();
        // System + Light: shows light, toggles to dark.
        assert_eq!(w.appearance(), Appearance::Light);
        assert!(matches!(
            w.dispatch(WorkspaceAction::ToggleThemeMode),
            Effect::ApplySettings
        ));
        assert_eq!(w.settings().settings().theme, ThemePreference::Dark);
        assert_eq!(w.appearance(), Appearance::Dark);
        w.dispatch(WorkspaceAction::ToggleThemeMode);
        assert_eq!(w.settings().settings().theme, ThemePreference::Light);
        // Follow system on a dark OS: toggles to light.
        let mut w = ws();
        w.set_system_appearance(Appearance::Dark);
        w.dispatch(WorkspaceAction::ToggleThemeMode);
        assert_eq!(w.settings().settings().theme, ThemePreference::Light);
        assert_eq!(w.appearance(), Appearance::Light);
    }

    #[test]
    fn set_theme_and_font_size_apply_only_when_changed() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetTheme(ThemePreference::System)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Dark)),
            Effect::ApplySettings
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(14)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(49)),
            Effect::None
        ));
        assert!(matches!(
            w.dispatch(WorkspaceAction::SetUiFontSize(20)),
            Effect::ApplySettings
        ));
        assert_eq!(w.settings().settings().ui_font_size, 20);
    }

    #[test]
    fn system_appearance_matters_only_under_system_preference() {
        let mut w = ws();
        assert!(w.set_system_appearance(Appearance::Dark));
        assert_eq!(w.appearance(), Appearance::Dark);
        assert!(!w.set_system_appearance(Appearance::Dark));
        assert!(w.set_system_appearance(Appearance::Light));
        w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Dark));
        assert!(!w.set_system_appearance(Appearance::Light));
        assert!(!w.set_system_appearance(Appearance::Dark));
        w.dispatch(WorkspaceAction::SetTheme(ThemePreference::Light));
        assert!(!w.set_system_appearance(Appearance::Light));
    }

    #[test]
    fn with_settings_replaces_the_model() {
        let mut model = SettingsModel::default();
        model.set_theme(ThemePreference::Dark);
        let w = ws().with_settings(model);
        assert_eq!(w.appearance(), Appearance::Dark);
    }

    // ---- workbench wiring ----

    #[test]
    fn tab_decor_shows_dirty_and_running_and_editing_pins_a_preview() {
        let mut w = ws();
        let tab = open_tabs(&mut w, 1)[0];
        let d = w.tab_decor(tab).unwrap();
        assert!(!d.dirty && !d.running && !d.header.preview);
        w.editor_edited(tab);
        assert!(w.tab_decor(tab).unwrap().dirty);
        // A preview tab is pinned by an edit.
        let conn = w.sidebar().entries()[0].id().clone();
        let preview = w
            .open_object_preview(&conn, TabKind::Ddl, "t")
            .map(|(t, _)| t)
            .unwrap();
        assert!(w.tab_decor(preview).unwrap().header.preview);
        w.editor_edited(preview);
        assert!(!w.tab_decor(preview).unwrap().header.preview);
    }

    #[test]
    fn close_others_to_the_right_and_all_close_within_the_group() {
        let mut w = ws();
        let t = open_tabs(&mut w, 4);
        w.dispatch(WorkspaceAction::CloseTabsToRight(t[1]));
        assert_eq!(w.tabs().len(), 2);
        w.dispatch(WorkspaceAction::CloseOtherTabs(t[0]));
        assert_eq!(
            w.tabs()
                .iter()
                .map(crate::tabs::Tab::id)
                .collect::<Vec<_>>(),
            [t[0]]
        );
        let more = open_tabs(&mut w, 2);
        w.dispatch(WorkspaceAction::CloseAllTabs);
        assert!(w.tabs().is_empty(), "{more:?}");
    }

    fn stage_pending(w: &mut Workspace, tab: TabId) {
        w.set_pending_edits(crate::grid::edit::GridKey { tab, result: None }, 1);
    }

    #[test]
    fn a_batch_close_stops_at_the_first_pending_changes_prompt() {
        for action in [
            |t: &[TabId]| WorkspaceAction::CloseOtherTabs(t[0]),
            |_: &[TabId]| WorkspaceAction::CloseAllTabs,
        ] {
            let mut w = ws();
            let t = open_tabs(&mut w, 3);
            stage_pending(&mut w, t[1]);
            w.dispatch(action(&t));
            let prompt = *w.close_prompt().expect("a prompt");
            assert_eq!(prompt.target, CloseTarget::Tab(t[1]));
            assert!(w.tabs().get(t[1]).is_some());
            assert!(
                w.tabs().get(t[2]).is_some(),
                "later tabs stay behind the dialog"
            );
            w.confirm_close();
            assert!(w.tabs().get(t[1]).is_none());
            assert!(
                w.tabs().get(t[2]).is_some(),
                "confirm closes only the prompted tab"
            );
        }
        // To the right: the prompt stops the tabs after the dirty one.
        let mut w = ws();
        let t = open_tabs(&mut w, 4);
        stage_pending(&mut w, t[2]);
        w.dispatch(WorkspaceAction::CloseTabsToRight(t[0]));
        assert_eq!(w.close_prompt().unwrap().target, CloseTarget::Tab(t[2]));
        assert!(
            w.tabs().get(t[1]).is_none(),
            "tabs before the prompt closed"
        );
        assert!(w.tabs().get(t[3]).is_some());
        w.confirm_close();
        assert!(w.tabs().get(t[2]).is_none());
        assert!(w.tabs().get(t[3]).is_some());
    }

    fn dirty_editor(w: &mut Workspace, tab: TabId) {
        w.editors.get_mut(tab).expect("an editor").mark_dirty();
    }

    #[test]
    fn a_batch_close_prompts_for_a_dirty_editor_and_a_single_close_does_not() {
        for action in [
            |t: &[TabId]| WorkspaceAction::CloseOtherTabs(t[0]),
            |_: &[TabId]| WorkspaceAction::CloseAllTabs,
        ] {
            let mut w = ws();
            let t = open_tabs(&mut w, 3);
            dirty_editor(&mut w, t[1]);
            w.dispatch(action(&t));
            let prompt = *w.close_prompt().expect("a prompt");
            assert_eq!(prompt.target, CloseTarget::Tab(t[1]));
            assert!(prompt.unsaved_text);
            assert!(w.tabs().get(t[1]).is_some());
            assert!(w.tabs().get(t[2]).is_some(), "later tabs stay behind it");
            w.confirm_close();
            assert!(w.tabs().get(t[1]).is_none());
            assert!(
                w.tabs().get(t[2]).is_some(),
                "confirm closes only the prompted tab"
            );
        }
        // To the right.
        let mut w = ws();
        let t = open_tabs(&mut w, 4);
        dirty_editor(&mut w, t[2]);
        w.dispatch(WorkspaceAction::CloseTabsToRight(t[0]));
        assert_eq!(w.close_prompt().unwrap().target, CloseTarget::Tab(t[2]));
        assert!(w.tabs().get(t[1]).is_none());
        assert!(w.tabs().get(t[3]).is_some());
        // Cancel stops the batch and keeps every remaining tab.
        w.dismiss_close();
        assert!(w.close_prompt().is_none());
        assert!(w.tabs().get(t[2]).is_some());
        assert!(w.tabs().get(t[3]).is_some());
        // A single close of a dirty editor is unchanged (Q3): no prompt.
        w.dispatch(WorkspaceAction::CloseTab(t[2]));
        assert!(w.close_prompt().is_none());
        assert!(w.tabs().get(t[2]).is_none());
    }

    #[test]
    fn a_real_run_decorates_the_tab_sets_the_status_and_reveals_results() {
        let mut w = ws();
        let tab = open_tabs(&mut w, 1)[0];
        assert!(!w.layout().panel_visible());
        let Effect::RunQuery { job, .. } = w.run_query(
            tab,
            crate::query::RunMode::All,
            crate::query::BufferSnapshot::at_cursor("select 1", 0),
        ) else {
            panic!("expected RunQuery");
        };
        assert!(w.tab_decor(tab).unwrap().running);
        assert_eq!(w.status_bar(None).run, RunIndicator::Running);
        assert!(w.layout().panel_visible());
        assert_eq!(
            w.layout().panel_view(),
            crate::workbench::PanelView::Results
        );
        w.cancel_query(tab);
        assert_eq!(w.status_bar(None).run, RunIndicator::Cancelling);
        drop(job);
    }

    #[test]
    fn pin_tab_pins_a_preview() {
        let mut w = ws();
        open_tabs(&mut w, 1);
        let conn = w.sidebar().entries()[0].id().clone();
        let (p, _) = w.open_object_preview(&conn, TabKind::Ddl, "t").unwrap();
        w.dispatch(WorkspaceAction::PinTab(p));
        assert_eq!(w.tabs().mode(p), Some(crate::tabs::TabMode::Pinned));
    }

    #[test]
    fn split_refusals_give_notices_and_a_split_moves_the_tab() {
        let mut w = ws();
        open_tabs(&mut w, 1);
        w.dispatch(WorkspaceAction::SplitRight);
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Split needs two or more tabs in the group.")
        );
        open_tabs(&mut w, 1);
        for _ in 0..3 {
            w.dispatch(WorkspaceAction::SplitRight);
            open_tabs(&mut w, 1);
        }
        assert_eq!(w.tabs().groups().len(), 4);
        assert!(w.take_notice().is_none());
        w.dispatch(WorkspaceAction::SplitRight);
        assert_eq!(w.take_notice().as_deref(), Some("At most 4 editor groups."));
        w.dispatch(WorkspaceAction::FocusPreviousGroup);
        w.dispatch(WorkspaceAction::MoveActiveTabToNextGroup);
        // The third group had one tab: it is gone after the move.
        assert_eq!(w.tabs().groups().len(), 3);
    }

    #[test]
    fn layout_actions_and_run_reveal_the_panel() {
        let mut w = ws();
        assert!(!w.layout().panel_visible());
        assert!(matches!(
            w.dispatch_workbench(WorkbenchAction::ToggleBottomPanel),
            Effect::None
        ));
        assert!(w.layout().panel_visible());
        w.dispatch_workbench(WorkbenchAction::ToggleBottomPanel);
        w.layout.reveal_panel(crate::workbench::PanelView::Results);
        assert!(w.layout().panel_visible());
    }

    #[test]
    fn with_workbench_loaded_newer_and_invalid() {
        let mut state = WorkbenchState::default();
        state.side_panel.visible = false;
        state.side_panel.size = 300.0;
        let w = ws().with_workbench(&WorkbenchLoad::Loaded(state.clone()));
        assert!(!w.layout().side_visible());
        let saved = w.workbench_state(&RecentCommands::default(), None).unwrap();
        assert!(!saved.side_panel.visible);
        assert_eq!(saved.version, WORKBENCH_VERSION);

        let w = ws().with_workbench(&WorkbenchLoad::Newer(2));
        assert!(w.layout().side_visible());
        assert!(
            w.workbench_state(&RecentCommands::default(), None)
                .is_none()
        );

        // An invalid file gives defaults and a notice; the config layer copies the file to
        // `workbench.toml.bak` before the first save replaces it.
        let mut w = ws().with_workbench(&WorkbenchLoad::Invalid("bad".into()));
        assert_eq!(
            w.take_notice().as_deref(),
            Some(
                "Could not read workbench.toml, using the default layout \
                 (the file is copied to workbench.toml.bak before it is replaced): bad"
            )
        );
        assert!(
            w.workbench_state(&RecentCommands::default(), None)
                .is_some()
        );
    }

    #[test]
    fn the_startup_load_reaches_the_workspace() {
        use crate::workbench::WorkbenchStartup;
        use sqler_config::ConfigDir;
        let dir = std::env::temp_dir().join(format!("sqler-ui-startup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = ConfigDir::new(&dir);

        let mut state = WorkbenchState::default();
        state.side_panel.visible = false;
        config.save_workbench_state(&state).unwrap();
        let w = ws().with_workbench(&WorkbenchStartup::load(Some(config.clone())).load);
        assert!(!w.layout().side_visible());

        std::fs::write(config.workbench_path(), "side_panel = [oops").unwrap();
        let mut w = ws().with_workbench(&WorkbenchStartup::load(Some(config)).load);
        assert!(w.layout().side_visible());
        assert!(
            w.take_notice()
                .is_some_and(|n| n.contains("workbench.toml"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_connections_follow_opens_and_the_sidebar() {
        let mut w = ws();
        let dev = w.sidebar().entries()[0].id().clone();
        let prod = w.sidebar().entries()[1].id().clone();
        let r = connect(&mut w, 0);
        finish_ok(&mut w, r);
        let r = connect(&mut w, 1);
        finish_ok(&mut w, r);
        let ids: Vec<&ConnectionId> = w.recent_connections().iter().collect();
        assert_eq!(ids, [&prod, &dev]);
        let state = w
            .workbench_state(
                &RecentCommands::default(),
                Some(WindowState {
                    width: 1200.0,
                    height: 5.0,
                }),
            )
            .unwrap();
        assert_eq!(state.recent_connections.len(), 2);
        assert_eq!(state.window, None, "an out-of-range window is dropped");
        let reload = ws().with_workbench(&WorkbenchLoad::Loaded(state));
        assert_eq!(reload.recent_connections().iter().count(), 2);
    }

    #[test]
    fn status_bar_for_no_tab_prod_and_sqlite() {
        let mut w = ws();
        assert_eq!(w.status_bar(None), StatusBarModel::default());
        let r = connect(&mut w, 1);
        finish_ok(&mut w, r);
        let s = w.status_bar(Some(RowInfo {
            loaded: 1234,
            shown: 1234,
            more: false,
        }));
        assert_eq!(s.env, Some(Environment::Prod));
        assert_eq!(s.connection.as_deref(), Some("prod"));
        assert_eq!(s.database.as_deref(), Some("memory"));
        assert_eq!(s.run, RunIndicator::Idle);
        assert_eq!(s.rows.as_deref(), Some("1,234 rows"));
        assert_eq!(s.timing, None);
        let r = connect(&mut w, 0);
        finish_ok(&mut w, r);
        let s = w.status_bar(None);
        assert_eq!(s.accent, Some(crate::tabs::Rgb(0x0000_ff00)));
        assert_eq!(s.rows, None);
    }

    #[test]
    fn status_bar_shows_ai_state() {
        use sqler_config::{
            AiConfig, ConnectionAi, ModelChoice, ProviderConfig, ProviderId, ProviderKind,
        };
        let pid = |s: &str| ProviderId::new(s).unwrap();
        let mut w = ws();
        assert_eq!(w.status_bar(None).ai, None, "no tab, no AI item");
        // Off by default.
        let r = connect(&mut w, 0);
        finish_ok(&mut w, r);
        assert_eq!(w.status_bar(None).ai, Some(AiStatus::Off));
        let dev = w.sidebar().entries()[0].id().clone();
        let turn_on = |w: &mut Workspace| {
            let spec = w.sidebar().profile(&dev).unwrap().spec().clone();
            w.sidebar
                .update_connection(&dev, spec.with_ai(ConnectionAi::new(true)))
                .unwrap();
        };
        // On, but no provider.
        turn_on(&mut w);
        assert_eq!(w.status_bar(None).ai, Some(AiStatus::NotSetUp));
        // A local provider.
        let mut config = AiConfig::default();
        config
            .providers
            .push(ProviderConfig::new(pid("ollama"), ProviderKind::Ollama).with_models(["q"]));
        config.providers.push(
            ProviderConfig::new(pid("claude"), ProviderKind::Anthropic)
                .with_models(["c"])
                .with_api_key_in_keychain(true),
        );
        config.models.default = Some(ModelChoice::new(pid("ollama"), "q"));
        let w = w.with_ai(AiState::for_config(config.clone()));
        let label = |w: &Workspace| w.status_bar(None).ai.map(|a| a.label());
        assert_eq!(label(&w).as_deref(), Some("ollama \u{b7} local"));
        // A remote one.
        config.models.default = Some(ModelChoice::new(pid("claude"), "c"));
        let w2 = w.with_ai(AiState::for_config(config));
        assert_eq!(label(&w2).as_deref(), Some("claude \u{b7} remote"));
    }

    #[test]
    fn breadcrumbs_for_query_and_object_tabs() {
        let mut w = ws();
        let tab = open_tabs(&mut w, 1)[0];
        let c = w.breadcrumbs(tab).unwrap();
        assert_eq!(c.items, ["dev", "memory"]);
        assert_eq!(c.env, Some(Environment::Dev));
        assert_eq!(c.label(), "dev › memory");
        // An object tab: its recorded crumbs, else its title.
        let conn = w.sidebar().entries()[0].id().clone();
        let (obj, _) = w.open_object_preview(&conn, TabKind::Ddl, "t").unwrap();
        assert_eq!(w.breadcrumbs(obj).unwrap().items, ["dev", "DDL: t"]);
        w.object_crumbs
            .insert(obj, vec!["main".to_owned(), "t".to_owned()]);
        assert_eq!(w.breadcrumbs(obj).unwrap().items, ["dev", "main", "t"]);
        w.dispatch(WorkspaceAction::CloseTab(obj));
        assert!(!w.object_crumbs.contains_key(&obj));
        assert!(w.breadcrumbs(obj).is_none());
    }

    #[test]
    fn quit_and_take_session() {
        let mut w = ws();
        // No session open: quit right away.
        assert!(matches!(w.dispatch(WorkspaceAction::Quit), Effect::Quit));
        let request = connect(&mut w, 0);
        let tab = finish_ok(&mut w, request);
        assert!(w.take_session(tab).is_some());
        assert!(w.take_session(tab).is_none());
    }

    fn open_grid(w: &mut Workspace) -> (TabId, GridTab) {
        match w.dispatch(WorkspaceAction::OpenSyntheticGrid) {
            Effect::OpenGrid { tab, grid } => (tab, grid),
            other => panic!("expected OpenGrid, got {other:?}"),
        }
    }

    #[test]
    fn synthetic_grid_without_tab_or_selection_is_noop() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch(WorkspaceAction::OpenSyntheticGrid),
            Effect::None
        ));
        assert!(w.tabs().is_empty());
    }

    #[test]
    fn synthetic_grid_binds_to_the_active_tab_connection() {
        let mut w = ws();
        w.set_synthetic_rows(1_000);
        let request = connect(&mut w, 1);
        let query = finish_ok(&mut w, request);
        let (tab, mut grid) = open_grid(&mut w);
        assert_ne!(tab, query);
        assert_eq!(w.tabs().len(), 2);
        let opened = w.tabs().get(tab).unwrap();
        assert_eq!(opened.kind(), TabKind::Grid);
        assert_eq!(
            opened.connection(),
            w.tabs().get(query).unwrap().connection()
        );
        assert_eq!(opened.title(), "Results: prod (2)");
        assert_eq!(w.tabs().active().map(crate::tabs::Tab::id), Some(tab));
        assert!(!w.has_session(tab));
        while !grid.poll(usize::MAX).terminal {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(grid.row_count(), 1_000);
    }

    #[test]
    fn synthetic_grid_falls_back_to_the_selected_entry() {
        let mut w = ws();
        w.set_synthetic_rows(10);
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let (tab, _grid) = open_grid(&mut w);
        let opened = w.tabs().get(tab).unwrap();
        assert_eq!(opened.connection().name, "dev");
        assert_eq!(opened.connection().color.as_deref(), Some("#00ff00"));
        assert_eq!(opened.connection().environment, Some(Environment::Dev));
        assert_eq!(opened.title(), "Results: dev");
    }

    #[test]
    fn closing_a_grid_tab_cancels_its_ingestion_only() {
        let mut w = ws();
        w.set_synthetic_rows(1_000);
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let (first, _g1) = open_grid(&mut w);
        let (second, _g2) = open_grid(&mut w);
        let flag_first = w.grid_cancels.get(&first).unwrap().clone();
        let flag_second = w.grid_cancels.get(&second).unwrap().clone();
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseTab(first)),
            Effect::None
        ));
        assert!(flag_first.is_cancelled());
        assert!(!flag_second.is_cancelled());
        assert!(!w.grid_cancels.contains_key(&first));
        assert_eq!(w.tabs().len(), 1);
        // Closing the active grid tab through the action works the same way.
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseActiveTab),
            Effect::None
        ));
        assert!(flag_second.is_cancelled());
        assert!(w.grid_cancels.is_empty());
    }

    #[test]
    fn closing_a_query_tab_leaves_grids_running() {
        let mut w = ws();
        w.set_synthetic_rows(1_000);
        let request = connect(&mut w, 0);
        let query = finish_ok(&mut w, request);
        let (grid_tab, _grid) = open_grid(&mut w);
        let flag = w.grid_cancels.get(&grid_tab).unwrap().clone();
        assert!(matches!(
            w.dispatch(WorkspaceAction::CloseTab(query)),
            Effect::CloseSession(_)
        ));
        assert!(!flag.is_cancelled());
    }

    // ---- connection manager ----

    use ConnectionAction as A;

    fn manager() -> Workspace {
        Workspace::new(SidebarModel::default())
            .with_drivers(vec![DriverId::new("postgres").unwrap()])
    }

    fn text(field: FormField, value: &str) -> ConnectionAction {
        A::SetText {
            field,
            text: value.to_owned(),
        }
    }

    fn fill(w: &mut Workspace, name: &str) {
        w.dispatch_connection(text(FormField::Name, name));
        w.dispatch_connection(text(
            FormField::Target,
            "postgres://db.example:5432/app?user=me",
        ));
    }

    fn form(w: &Workspace) -> &ConnectionForm {
        match w.dialog() {
            Some(Dialog::Form(form)) => form,
            other => panic!("expected the form, got {other:?}"),
        }
    }

    fn saved_secrets(effect: Effect) -> Vec<SecretOp> {
        match effect {
            Effect::SaveConnections { secrets } => secrets,
            other => panic!("expected SaveConnections, got {other:?}"),
        }
    }

    fn ref_text(op: &SecretOp) -> String {
        match op {
            SecretOp::Set(r, _) => format!("set {r}"),
            SecretOp::Delete(r) => format!("delete {r}"),
        }
    }

    fn describe(ops: &[SecretOp]) -> Vec<String> {
        ops.iter().map(ref_text).collect()
    }

    fn id_of(w: &Workspace, name: &str) -> ConnectionId {
        w.sidebar()
            .entries()
            .iter()
            .find(|e| e.config().name == name)
            .map(|e| e.id().clone())
            .unwrap()
    }

    #[test]
    fn with_drivers_feeds_the_new_form() {
        let mut w = manager();
        assert_eq!(w.drivers().len(), 1);
        assert!(w.dialog().is_none());
        assert!(matches!(w.dispatch_connection(A::New), Effect::None));
        assert_eq!(form(&w).driver().map(DriverId::as_str), Some("postgres"));
        assert!(matches!(form(&w).mode(), FormMode::New));
    }

    #[test]
    fn new_connection_saves_and_selects_it() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert!(secrets.is_empty());
        assert!(w.dialog().is_none());
        assert_eq!(w.sidebar().entries().len(), 1);
        assert_eq!(w.sidebar().selected(), Some(0));
        assert_eq!(w.save_connections(), Ok(()));
    }

    #[test]
    fn new_connection_with_password_writes_the_keychain_not_the_target() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        w.dispatch_connection(text(FormField::DbPassword, "hunter2"));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(describe(&secrets), ["set main:db-password"]);
        let SecretOp::Set(_, secret) = &secrets[0] else {
            panic!("expected a set");
        };
        assert_eq!(secret.expose(), "hunter2");
        let profile = w.sidebar().profile(&id_of(&w, "Main")).unwrap();
        assert!(profile.spec().password_in_keychain);
        assert!(!profile.spec().target.contains("hunter2"));
        assert!(!format!("{secrets:?}").contains("hunter2"));
    }

    #[test]
    fn invalid_form_keeps_the_dialog_and_shows_errors() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        assert!(matches!(w.dispatch_connection(A::Save), Effect::None));
        assert!(form(&w).error_for(FormField::Name).is_some());
        assert!(w.sidebar().entries().is_empty());
    }

    #[test]
    fn duplicate_name_is_reported_on_the_form() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        w.dispatch_connection(A::Save);
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        assert!(matches!(w.dispatch_connection(A::Save), Effect::None));
        assert!(form(&w).error_for(FormField::Name).is_some());
        assert_eq!(w.sidebar().entries().len(), 1);
    }

    fn saved_with_secrets(w: &mut Workspace) -> ConnectionId {
        w.dispatch_connection(A::New);
        fill(w, "Main");
        w.dispatch_connection(text(FormField::DbPassword, "pw"));
        w.dispatch_connection(A::SetSshEnabled(true));
        w.dispatch_connection(text(FormField::SshHost, "bastion"));
        w.dispatch_connection(text(FormField::SshUser, "me"));
        w.dispatch_connection(text(FormField::SshSecret, "sshpw"));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(
            describe(&secrets),
            ["set main:db-password", "set main:ssh-password"]
        );
        id_of(w, "Main")
    }

    #[test]
    fn edit_keeps_stored_secrets_untouched() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Edit(id.clone()));
        assert!(matches!(form(&w).mode(), FormMode::Edit(x) if *x == id));
        w.dispatch_connection(text(FormField::Name, "Renamed"));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert!(secrets.is_empty());
        let profile = w.sidebar().profile(&id).unwrap();
        assert_eq!(profile.spec().name, "Renamed");
        assert_eq!(profile.secret_refs().len(), 2);
    }

    #[test]
    fn editing_a_connection_keeps_its_ai_opt_in() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        let mut spec = w.sidebar().profile(&id).unwrap().spec().clone();
        spec = spec.with_ai(sqler_config::ConnectionAi::new(true).with_send_rows(false));
        w.sidebar.update_connection(&id, spec).unwrap();
        let before = w.sidebar().profile(&id).unwrap().spec().ai;
        w.dispatch_connection(A::Edit(id.clone()));
        w.dispatch_connection(text(FormField::Name, "Renamed"));
        w.dispatch_connection(A::Save);
        let profile = w.sidebar().profile(&id).unwrap();
        assert_eq!(profile.spec().name, "Renamed");
        assert_eq!(profile.spec().ai, before);
        // The dialog changes it.
        w.dispatch_connection(A::Edit(id.clone()));
        w.dispatch_connection(A::SetAiEnabled(false));
        w.dispatch_connection(A::SetAiSendRows(None));
        w.dispatch_connection(A::Save);
        assert!(w.sidebar().profile(&id).unwrap().spec().ai.is_default());
    }

    #[test]
    fn edit_replaces_and_clears_secrets() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Edit(id.clone()));
        w.dispatch_connection(text(FormField::DbPassword, "new"));
        w.dispatch_connection(A::ClearSecret(crate::connections::SecretSlot::Ssh));
        w.dispatch_connection(A::SetSshAuth(SshAuthKind::Agent));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(
            describe(&secrets),
            ["delete main:ssh-password", "set main:db-password"]
        );
    }

    #[test]
    fn edit_clearing_the_tunnel_deletes_its_secret() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Edit(id));
        w.dispatch_connection(A::SetSshEnabled(false));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(describe(&secrets), ["delete main:ssh-password"]);
    }

    #[test]
    fn edit_switching_to_a_key_replaces_the_password_by_a_passphrase() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Edit(id));
        w.dispatch_connection(A::SetSshAuth(SshAuthKind::KeyFile));
        w.dispatch_connection(text(FormField::SshKeyPath, "/k"));
        w.dispatch_connection(text(FormField::SshSecret, "phrase"));
        let secrets = saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(
            describe(&secrets),
            ["delete main:ssh-password", "set main:ssh-key-passphrase"]
        );
    }

    #[test]
    fn edit_can_change_the_group() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("prod".into()));
        w.dispatch_connection(A::SubmitGroupName);
        let group = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(A::Edit(id.clone()));
        w.dispatch_connection(A::SetGroup(Some(group.clone())));
        saved_secrets(w.dispatch_connection(A::Save));
        assert_eq!(w.sidebar().profile(&id).unwrap().group(), Some(&group));
    }

    #[test]
    fn cancel_closes_the_dialog_without_saving() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        assert!(matches!(w.dispatch_connection(A::Cancel), Effect::None));
        assert!(w.dialog().is_none());
        assert!(w.sidebar().entries().is_empty());
    }

    #[test]
    fn delete_asks_first_and_removes_every_keychain_entry() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Delete(id.clone()));
        let Some(Dialog::ConfirmDelete { name, .. }) = w.dialog() else {
            panic!("expected the confirmation");
        };
        assert_eq!(name, "Main");
        // Nothing is deleted before the confirmation.
        assert_eq!(w.sidebar().entries().len(), 1);
        let secrets = saved_secrets(w.dispatch_connection(A::ConfirmDelete));
        assert_eq!(
            describe(&secrets),
            ["delete main:db-password", "delete main:ssh-password"]
        );
        assert!(w.sidebar().entries().is_empty());
        assert!(w.dialog().is_none());
    }

    #[test]
    fn declining_a_delete_changes_nothing() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Delete(id));
        assert!(matches!(w.dispatch_connection(A::Cancel), Effect::None));
        assert_eq!(w.sidebar().entries().len(), 1);
        // Confirming without a dialog does nothing either.
        assert!(matches!(
            w.dispatch_connection(A::ConfirmDelete),
            Effect::None
        ));
        assert_eq!(w.sidebar().entries().len(), 1);
    }

    #[test]
    fn group_create_rename_delete() {
        let mut w = manager();
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("  ".into()));
        // An empty name is refused inside the dialog.
        assert!(matches!(
            w.dispatch_connection(A::SubmitGroupName),
            Effect::None
        ));
        let Some(Dialog::GroupName { error, .. }) = w.dialog() else {
            panic!("expected the group dialog");
        };
        assert!(error.is_some());
        w.dispatch_connection(A::SetGroupName("Prod".into()));
        saved_secrets(w.dispatch_connection(A::SubmitGroupName));
        assert!(w.dialog().is_none());
        let group = w.sidebar().group_options()[0].id.clone();
        assert_eq!(w.sidebar().selected_group(), Some(&group));
        w.dispatch_connection(A::RenameGroup(group.clone()));
        let Some(Dialog::GroupName { text, .. }) = w.dialog() else {
            panic!("expected the group dialog");
        };
        assert_eq!(text, "Prod");
        w.dispatch_connection(A::SetGroupName("Production".into()));
        saved_secrets(w.dispatch_connection(A::SubmitGroupName));
        assert_eq!(w.sidebar().group_options()[0].path, "Production");
        saved_secrets(w.dispatch_connection(A::DeleteGroup(group)));
        assert_eq!(w.sidebar().group_options().len(), 0);
    }

    #[test]
    fn nested_group_and_new_connection_start_in_the_selected_group() {
        let mut w = manager();
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("a".into()));
        w.dispatch_connection(A::SubmitGroupName);
        let a = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(A::NewGroup {
            parent: Some(a.clone()),
        });
        w.dispatch_connection(A::SetGroupName("b".into()));
        w.dispatch_connection(A::SubmitGroupName);
        assert_eq!(w.sidebar().group_options()[1].path, "a / b");
        w.dispatch_connection(A::New);
        let b = w.sidebar().group_options()[1].id.clone();
        assert_eq!(form(&w).group(), Some(&b));
    }

    #[test]
    fn deleting_a_full_group_is_refused_with_a_notice() {
        let mut w = manager();
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("a".into()));
        w.dispatch_connection(A::SubmitGroupName);
        let a = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        w.dispatch_connection(A::Save);
        assert!(matches!(
            w.dispatch_connection(A::DeleteGroup(a)),
            Effect::None
        ));
        assert!(w.take_notice().unwrap().contains("not empty"));
        assert!(w.take_notice().is_none());
    }

    #[test]
    fn move_connection_by_dialog_and_by_drag() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("g".into()));
        w.dispatch_connection(A::SubmitGroupName);
        let g = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(A::OpenMoveTo(id.clone()));
        assert!(matches!(w.dialog(), Some(Dialog::MoveTo { .. })));
        let effect = w.dispatch_connection(A::MoveConnection {
            id: id.clone(),
            to: Some(g.clone()),
            index: usize::MAX,
        });
        assert!(saved_secrets(effect).is_empty());
        assert!(w.dialog().is_none());
        assert_eq!(w.sidebar().profile(&id).unwrap().group(), Some(&g));
        // Back to the root, as a drag onto the header does.
        w.dispatch_connection(A::MoveConnection {
            id: id.clone(),
            to: None,
            index: usize::MAX,
        });
        assert_eq!(w.sidebar().profile(&id).unwrap().group(), None);
    }

    #[test]
    fn moving_an_unknown_connection_gives_a_notice() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Delete(id.clone()));
        w.dispatch_connection(A::ConfirmDelete);
        let effect = w.dispatch_connection(A::MoveConnection {
            id,
            to: None,
            index: 0,
        });
        assert!(matches!(effect, Effect::None));
        assert!(w.take_notice().is_some());
    }

    // ---- test connection ----

    fn start_test(w: &mut Workspace) -> RequestId {
        match w.dispatch_connection(A::Test) {
            Effect::Connect { request, .. } => request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    fn test_status(w: &Workspace) -> TestStatus {
        form(w).test.clone()
    }

    #[test]
    fn test_runs_the_form_as_typed() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        w.dispatch_connection(text(FormField::DbPassword, "pw"));
        let Effect::Connect { request, connect } = w.dispatch_connection(A::Test) else {
            panic!("expected Connect");
        };
        assert_eq!(test_status(&w), TestStatus::Running(request));
        assert!(w.is_request_live(request));
        assert!(matches!(connect.db_password, SecretSource::Value(_)));
        // Nothing was saved by testing.
        assert!(w.sidebar().entries().is_empty());
        // A second click while running does nothing.
        assert!(matches!(w.dispatch_connection(A::Test), Effect::None));
    }

    #[test]
    fn test_with_an_invalid_form_shows_errors_and_starts_nothing() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        assert!(matches!(w.dispatch_connection(A::Test), Effect::None));
        assert!(form(&w).error_for(FormField::Name).is_some());
        assert_eq!(test_status(&w), TestStatus::Idle);
    }

    #[test]
    fn test_success_closes_the_session_and_shows_ok() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        let (session, _) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::CloseSession(_)
        ));
        assert_eq!(test_status(&w), TestStatus::Ok);
        assert!(w.tabs().is_empty());
        assert!(!w.is_request_live(request));
    }

    #[test]
    fn test_failure_shows_the_typed_error() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        let failure = ConnectFailure::MissingSecret {
            kind: SecretKind::DatabasePassword,
        };
        assert!(matches!(
            w.connect_finished(request, Err(failure)),
            Effect::None
        ));
        assert_eq!(
            test_status(&w),
            TestStatus::Failed(
                "no db-password stored in the keychain for this connection; edit it to set one"
                    .into()
            )
        );
    }

    #[test]
    fn cancelling_a_test_drops_the_attempt_and_a_late_session_is_closed() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        let Effect::CancelConnect(cancelled) = w.dispatch_connection(A::CancelTest) else {
            panic!("expected CancelConnect");
        };
        assert_eq!(cancelled, request);
        assert_eq!(test_status(&w), TestStatus::Idle);
        assert!(!w.is_request_live(request));
        let (session, _) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::CloseSession(_)
        ));
        assert_eq!(test_status(&w), TestStatus::Idle);
        // Nothing to cancel any more.
        assert!(matches!(w.dispatch_connection(A::CancelTest), Effect::None));
    }

    #[test]
    fn closing_the_form_cancels_a_running_test() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        assert!(matches!(
            w.dispatch_connection(A::Cancel),
            Effect::CancelConnect(r) if r == request
        ));
        assert!(w.dialog().is_none());
        assert!(!w.is_request_live(request));
    }

    #[test]
    fn test_of_the_selected_connection_opens_its_form() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        let Effect::Connect { connect, .. } = w.dispatch_connection(A::Test) else {
            panic!("expected Connect");
        };
        // Stored secrets are referenced, not read by the UI.
        assert!(matches!(connect.db_password, SecretSource::Keychain(_)));
        assert!(matches!(connect.ssh_secret, SecretSource::Keychain(_)));
        assert!(matches!(w.dialog(), Some(Dialog::Form(_))));
    }

    fn prompt() -> HostKeyPrompt {
        HostKeyPrompt {
            host: "bastion".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        }
    }

    #[test]
    fn unknown_host_key_in_a_test_asks_trusts_and_retries() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        let failure = ConnectFailure::UnknownHostKey(prompt());
        assert!(matches!(
            w.connect_finished(request, Err(failure)),
            Effect::None
        ));
        let Some(Dialog::TrustHostKey { key, retry }) = w.dialog() else {
            panic!("expected the trust dialog");
        };
        assert_eq!(key, &prompt());
        assert_eq!(retry, &Purpose::Test);
        let Effect::TrustHostKey { key, retry } = w.dispatch_connection(A::TrustHostKey) else {
            panic!("expected TrustHostKey");
        };
        assert_eq!(key, prompt());
        // The form is back, as typed.
        assert_eq!(form(&w).text(FormField::Name), "Main");
        let Effect::Connect { .. } = w.trust_finished(retry, Ok(())) else {
            panic!("expected the test to run again");
        };
        assert!(matches!(test_status(&w), TestStatus::Running(_)));
    }

    #[test]
    fn rejecting_a_host_key_returns_to_the_form() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(
            w.dispatch_connection(A::RejectHostKey),
            Effect::None
        ));
        assert_eq!(
            test_status(&w),
            TestStatus::Failed("the SSH host key was not trusted".into())
        );
        // Cancel does the same from the trust dialog.
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        w.dispatch_connection(A::Cancel);
        assert!(matches!(w.dialog(), Some(Dialog::Form(_))));
    }

    #[test]
    fn a_failed_trust_is_shown_in_the_form() {
        let mut w = manager();
        w.dispatch_connection(A::New);
        fill(&mut w, "Main");
        let request = start_test(&mut w);
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        let Effect::TrustHostKey { retry, .. } = w.dispatch_connection(A::TrustHostKey) else {
            panic!("expected TrustHostKey");
        };
        w.trust_finished(retry, Err(ConnectFailure::Tunnel("read-only".into())));
        assert_eq!(
            test_status(&w),
            TestStatus::Failed("SSH tunnel failed: read-only".into())
        );
    }

    #[test]
    fn unknown_host_key_when_opening_asks_and_retries_the_open() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        let index = w.sidebar().index_of(&id).unwrap();
        let request = connect(&mut w, index);
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(
            w.dialog(),
            Some(Dialog::TrustHostKey {
                retry: Purpose::Open(_),
                ..
            })
        ));
        assert!(matches!(
            w.sidebar().entries()[index].status(),
            ConnectionStatus::Failed(_)
        ));
        let Effect::TrustHostKey { retry, .. } = w.dispatch_connection(A::TrustHostKey) else {
            panic!("expected TrustHostKey");
        };
        assert!(w.dialog().is_none());
        assert!(matches!(
            w.trust_finished(retry, Ok(())),
            Effect::Connect { .. }
        ));
        assert_eq!(
            *w.sidebar().entries()[index].status(),
            ConnectionStatus::Connecting
        );
    }

    #[test]
    fn background_host_key_does_not_replace_a_confirmation_dialog() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        let request = connect(&mut w, 0);
        w.dispatch_connection(A::Delete(id));
        assert!(matches!(w.dialog(), Some(Dialog::ConfirmDelete { .. })));
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(w.dialog(), Some(Dialog::ConfirmDelete { .. })));
        assert!(w.take_notice().unwrap().contains("host key"));
    }

    #[test]
    fn background_host_key_does_not_replace_another_prompt() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        let request = connect(&mut w, 0);
        w.dispatch_connection(A::New);
        fill(&mut w, "Other");
        let test = start_test(&mut w);
        w.connect_finished(test, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(
            w.dialog(),
            Some(Dialog::TrustHostKey {
                retry: Purpose::Test,
                ..
            })
        ));
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(
            w.dialog(),
            Some(Dialog::TrustHostKey {
                retry: Purpose::Test,
                ..
            })
        ));
        assert!(w.take_notice().is_some());
        let _ = id;
    }

    #[test]
    fn test_result_reaches_a_form_parked_behind_a_background_prompt() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        let open = connect(&mut w, 0);
        w.dispatch_connection(A::New);
        fill(&mut w, "Other");
        let test = start_test(&mut w);
        // A background open finds an unknown host key while the test runs.
        w.connect_finished(open, Err(ConnectFailure::UnknownHostKey(prompt())));
        assert!(matches!(w.dialog(), Some(Dialog::TrustHostKey { .. })));
        // The test result arrives while the form is parked: it is not discarded.
        w.connect_finished(test, Err(ConnectFailure::Aborted));
        w.dispatch_connection(A::RejectHostKey);
        assert_eq!(
            test_status(&w),
            TestStatus::Failed("connect aborted".into())
        );
    }

    #[test]
    fn successful_test_in_a_parked_form_stops_the_spinner() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        let open = connect(&mut w, 0);
        w.dispatch_connection(A::New);
        fill(&mut w, "Other");
        let test = start_test(&mut w);
        w.connect_finished(open, Err(ConnectFailure::UnknownHostKey(prompt())));
        let (session, _) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(test, Ok(session)),
            Effect::CloseSession(_)
        ));
        w.dispatch_connection(A::Cancel);
        assert_eq!(test_status(&w), TestStatus::Ok);
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sqler-ws-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn some_ops() -> Vec<SecretOp> {
        let reference = SecretRef::new("main", SecretKind::DatabasePassword).unwrap();
        vec![
            SecretOp::Set(reference.clone(), Secret::new("pw")),
            SecretOp::Delete(reference),
        ]
    }

    #[test]
    fn keychain_changes_follow_a_successful_save() {
        let dir = temp_dir("ok");
        let store = sqler_config::ConnectionStore::new();
        let mut w = Workspace::new(SidebarModel::from_store(
            store,
            Some(sqler_config::ConfigDir::new(&dir)),
        ));
        let (ops, notices) = w.commit_connections(some_ops());
        assert_eq!(ops.len(), 2);
        assert_eq!(notices, Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keychain_is_untouched_when_the_save_fails() {
        let dir = temp_dir("fail");
        std::fs::write(&dir, b"not a directory").unwrap();
        let mut w = Workspace::new(SidebarModel::from_store(
            sqler_config::ConnectionStore::new(),
            Some(sqler_config::ConfigDir::new(&dir)),
        ));
        let (ops, notices) = w.commit_connections(some_ops());
        assert!(ops.is_empty());
        assert!(notices[0].contains("Could not save"));
        assert!(notices.iter().any(|n| n.contains("keychain")));
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn keychain_is_untouched_when_the_file_was_not_loaded() {
        let mut w = Workspace::new(SidebarModel::with_load_error("bad file".into()));
        let (ops, notices) = w.commit_connections(some_ops());
        assert!(ops.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("keychain"));
        // Nothing to apply, nothing to say.
        let (ops, notices) = w.commit_connections(Vec::new());
        assert!(ops.is_empty());
        assert_eq!(notices, Vec::<String>::new());
    }

    #[test]
    fn a_connection_deleted_while_connecting_closes_its_late_session() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        let request = connect(&mut w, 0);
        w.dispatch_connection(A::Delete(id));
        w.dispatch_connection(A::ConfirmDelete);
        let (session, _) = FakeSession::boxed();
        assert!(matches!(
            w.connect_finished(request, Ok(session)),
            Effect::CloseSession(_)
        ));
        assert!(w.tabs().is_empty());
    }

    #[test]
    fn open_failure_shows_a_typed_message_on_the_entry() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        let request = connect(&mut w, 0);
        w.connect_finished(request, Err(ConnectFailure::Aborted));
        assert_eq!(
            *w.sidebar().entries()[0].status(),
            ConnectionStatus::Failed("connect aborted".into())
        );
    }

    #[test]
    fn quit_with_open_sessions_returns_them_all() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        for _ in 0..2 {
            let request = connect(&mut w, 0);
            finish_ok(&mut w, request);
        }
        let Effect::CloseSessions(sessions) = w.dispatch(WorkspaceAction::Quit) else {
            panic!("expected CloseSessions");
        };
        assert_eq!(sessions.len(), 2);
        // They are gone from the workspace.
        assert!(w.take_all_sessions().is_empty());
    }

    #[test]
    fn open_connection_for_a_store_connection_builds_a_keychain_request() {
        let mut w = manager();
        saved_with_secrets(&mut w);
        let Effect::Connect { connect, .. } = w.dispatch(WorkspaceAction::OpenConnection(0)) else {
            panic!("expected Connect");
        };
        assert!(matches!(connect.db_password, SecretSource::Keychain(_)));
    }

    #[test]
    fn toggle_and_select_group_actions() {
        let mut w = manager();
        w.dispatch_connection(A::NewGroup { parent: None });
        w.dispatch_connection(A::SetGroupName("a".into()));
        w.dispatch_connection(A::SubmitGroupName);
        let a = w.sidebar().group_options()[0].id.clone();
        w.dispatch_connection(A::ToggleGroup(a.clone()));
        assert!(!w.sidebar().is_expanded(&a));
        w.dispatch_connection(A::ToggleGroup(a.clone()));
        assert!(w.sidebar().is_expanded(&a));
        w.sidebar.clear_selection();
        w.dispatch_connection(A::SelectGroup(a.clone()));
        assert_eq!(w.sidebar().selected_group(), Some(&a));
    }

    #[test]
    fn field_actions_without_a_form_do_nothing() {
        let mut w = manager();
        assert!(matches!(
            w.dispatch_connection(text(FormField::Name, "x")),
            Effect::None
        ));
        assert!(matches!(w.dispatch_connection(A::Save), Effect::None));
        assert!(matches!(w.dispatch_connection(A::Test), Effect::None));
        assert!(w.dialog().is_none());
    }

    #[test]
    fn opening_another_dialog_replaces_the_current_one() {
        let mut w = manager();
        let id = saved_with_secrets(&mut w);
        w.dispatch_connection(A::Edit(id.clone()));
        let request = start_test(&mut w);
        let effect = w.dispatch_connection(A::Delete(id));
        assert!(matches!(effect, Effect::CancelConnect(r) if r == request));
        assert!(matches!(w.dialog(), Some(Dialog::ConfirmDelete { .. })));
    }
}
