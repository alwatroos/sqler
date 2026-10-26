// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The object browser inside the workspace: one browser per connected connection, the commands
//! that act on the selected node, the DROP confirmation, the object tabs and the jobs behind
//! them.
//!
//! Like the rest of the workspace this is state only. [`ObjectTree`] never calls the catalog:
//! every method here returns an [`Effect`] (keys to load, a refresh to watch, a job to run) that
//! the view carries out and reports back through `catalog_loaded`, `refresh_event` and
//! `job_finished`.

use std::collections::HashMap;
use std::sync::Arc;

use sqler_catalog::{CacheKey, Catalog, CatalogError, RefreshEvent, RefreshOutcome};
use sqler_config::ConnectionId;
use sqler_driver_api::sqler_core::{DbObject, Environment, Listing, Table};
use sqler_driver_api::{BoxFuture, CancelFlag};
use sqler_sql::{
    DEFAULT_ROW_LIMIT, Dialect, Script, qualified_name, quote_identifier, select_star,
};

use super::state::{Effect, Purpose, Workspace, tab_connection};
use crate::browser::{
    BrowserAction, BrowserError, DropDialog, DropFlow, LoadState, MenuEntry, ObjectTab,
    ObjectTabContent, ObjectTree, Property, TreeNodeId, TreeNodeKind, column_properties,
    generate_script, load_properties, menu_for, open_data, run_drop,
};
use crate::connections::{ConnectFailure, ConnectRequest, Connector};
use crate::grid::{GridLimits, GridSink, GridTab, grid_channel_with};
use crate::sidebar::{RowKind, Selection, SidebarItem, SidebarRow};
use crate::tabs::{TabId, TabKind, object_segments};

/// Where a connection's browser stands.
pub(super) enum BrowserStatus {
    /// The browser session is being opened.
    Connecting,
    /// The catalog is running on the browser session.
    Ready(Catalog),
    /// Opening the browser session failed; expanding the connection row retries.
    Failed(String),
}

/// The browser of one connection.
pub(super) struct BrowserState {
    /// Tells results of an earlier browser session from the current one.
    pub(super) generation: u64,
    pub(super) status: BrowserStatus,
    pub(super) tree: ObjectTree,
    /// `None` for a driver `sqler-sql` has no dialect for: no scripts, open data or copy name.
    pub(super) dialect: Option<Dialect>,
    /// The refresh that is running and the node it started from.
    pub(super) refresh: Option<(TreeNodeId, CancelFlag, u64)>,
    /// The script generations that are running, by ticket.
    pub(super) scripts: HashMap<u64, CancelFlag>,
    /// The last ticket given to a script generation.
    pub(super) next_script: u64,
}

impl BrowserState {
    fn shutdown(&self) {
        if let BrowserStatus::Ready(catalog) = &self.status {
            catalog.shutdown();
        }
        if let Some((_, flag, _)) = &self.refresh {
            flag.cancel();
        }
        for flag in self.scripts.values() {
            flag.cancel();
        }
    }
}

/// A line the sidebar shows under a connection row while its browser is not simply ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserNote {
    /// Something is going on (opening the browser session, loading the first level).
    Busy(&'static str),
    /// The browser cannot show the database.
    Failed(String),
}

/// The answer to one catalog load, as the view got it from `Catalog::children` or `table`.
#[derive(Debug)]
pub enum CatalogResult {
    /// The answer for a `CacheKey::Children`.
    Children(Result<Arc<Listing>, CatalogError>),
    /// The answer for a `CacheKey::Table`.
    Table(Result<Option<Arc<Table>>, CatalogError>),
}

/// The query that fills an open-data grid. The view runs it with
/// [`open_data`].
pub struct OpenDataJob {
    /// What to connect to.
    pub request: ConnectRequest,
    /// The `SELECT *` text.
    pub sql: String,
    /// Where the rows go.
    pub sink: GridSink,
}

/// A background job of the browser. The view runs it on tokio through `SessionBridge` and
/// reports the outcome with [`Workspace::job_finished`].
#[non_exhaustive]
pub enum BrowserJob {
    /// Generate a script to open in a SQL editor.
    Script {
        /// The connection whose editor gets the script.
        conn: ConnectionId,
        /// The browser session the job belongs to.
        generation: u64,
        /// Tells this generation from the others of the same browser.
        ticket: u64,
        /// What to connect to when the script needs a session.
        request: ConnectRequest,
        /// The catalog of the connection.
        catalog: Catalog,
        /// The SQL dialect.
        dialect: Dialect,
        /// The object.
        object: DbObject,
        /// Which script.
        script: Script,
        /// Set when the browser ends.
        cancel: CancelFlag,
    },
    /// Load the fields for the properties tab `tab`.
    Properties {
        /// The tab that shows the fields.
        tab: TabId,
        /// The catalog of the connection.
        catalog: Catalog,
        /// The object.
        object: DbObject,
    },
    /// Run a confirmed `DROP` statement.
    Drop {
        /// The connection the object belongs to.
        conn: ConnectionId,
        /// What to connect to.
        request: ConnectRequest,
        /// The statement, exactly as the confirmation dialog showed it.
        sql: String,
    },
}

impl std::fmt::Debug for BrowserJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Script {
                conn,
                ticket,
                script,
                ..
            } => f
                .debug_struct("Script")
                .field("conn", conn)
                .field("ticket", ticket)
                .field("script", script)
                .finish_non_exhaustive(),
            Self::Properties { tab, .. } => f
                .debug_struct("Properties")
                .field("tab", tab)
                .finish_non_exhaustive(),
            Self::Drop { conn, sql, .. } => f
                .debug_struct("Drop")
                .field("conn", conn)
                .field("sql", sql)
                .finish_non_exhaustive(),
        }
    }
}

/// How a [`BrowserJob`] ended.
#[derive(Debug)]
#[non_exhaustive]
pub enum BrowserJobResult {
    /// The text for a SQL editor.
    Script {
        /// The connection.
        conn: ConnectionId,
        /// The browser session the job belonged to.
        generation: u64,
        /// The ticket of the job.
        ticket: u64,
        /// The script, or why there is none.
        result: Result<String, BrowserError>,
    },
    /// The fields for a properties tab.
    Properties {
        /// The tab.
        tab: TabId,
        /// The fields, or why there are none.
        result: Result<Vec<Property>, BrowserError>,
    },
    /// The `DROP` ran, or failed.
    Drop {
        /// The connection.
        conn: ConnectionId,
        /// The outcome.
        result: Result<(), BrowserError>,
    },
}

impl BrowserJob {
    /// The result to report when the task that runs this job is lost (it panicked, or its
    /// answer channel was dropped), so no tab or dialog waits for it forever.
    #[must_use]
    pub fn lost(&self) -> BrowserJobResult {
        let err = BrowserError::JobLost;
        match self {
            Self::Script {
                conn,
                generation,
                ticket,
                ..
            } => BrowserJobResult::Script {
                conn: conn.clone(),
                generation: *generation,
                ticket: *ticket,
                result: Err(err),
            },
            Self::Properties { tab, .. } => BrowserJobResult::Properties {
                tab: *tab,
                result: Err(err),
            },
            Self::Drop { conn, .. } => BrowserJobResult::Drop {
                conn: conn.clone(),
                result: Err(err),
            },
        }
    }

    /// Runs the job. Only `Drop` and the `CREATE` of some kinds open a session, through
    /// `connector`; it is closed on every path.
    pub fn run(self, connector: Arc<dyn Connector>) -> BoxFuture<'static, BrowserJobResult> {
        match self {
            Self::Script {
                conn,
                generation,
                ticket,
                request,
                catalog,
                dialect,
                object,
                script,
                cancel,
            } => Box::pin(async move {
                let result =
                    generate_script(connector, request, catalog, dialect, object, script, cancel)
                        .await;
                BrowserJobResult::Script {
                    conn,
                    generation,
                    ticket,
                    result,
                }
            }),
            Self::Properties {
                tab,
                catalog,
                object,
            } => Box::pin(async move {
                let result = load_properties(catalog, object).await;
                BrowserJobResult::Properties { tab, result }
            }),
            Self::Drop { conn, request, sql } => Box::pin(async move {
                let result = run_drop(connector, request, sql).await;
                BrowserJobResult::Drop { conn, result }
            }),
        }
    }
}

impl OpenDataJob {
    /// Runs the query into the grid.
    pub fn run(
        self,
        connector: Arc<dyn Connector>,
    ) -> BoxFuture<'static, Result<(), BrowserError>> {
        Box::pin(open_data(connector, self.request, self.sql, self.sink))
    }
}

/// The name an object tab shows: the qualified name, else the plain name.
fn display_name(dialect: Option<Dialect>, object: &DbObject) -> String {
    dialect
        .and_then(|d| qualified_name(d, object).ok())
        .unwrap_or_else(|| object.name.clone())
}

impl Workspace {
    // ---- reads ----

    /// The object tree of a connection, once its browser exists.
    #[must_use]
    pub fn browser(&self, conn: &ConnectionId) -> Option<&ObjectTree> {
        self.browsers.get(conn).map(|b| &b.tree)
    }

    /// Whether the connection has a browser and whether its root is expanded.
    #[must_use]
    pub fn browser_expanded(&self, conn: &ConnectionId) -> Option<bool> {
        let b = self.browsers.get(conn)?;
        Some(b.tree.is_expanded(b.tree.root()))
    }

    /// What to show under a connection row besides its tree.
    #[must_use]
    pub fn browser_note(&self, conn: &ConnectionId) -> Option<BrowserNote> {
        let b = self.browsers.get(conn)?;
        match &b.status {
            BrowserStatus::Connecting => Some(BrowserNote::Busy("opening browser…")),
            BrowserStatus::Failed(message) => Some(BrowserNote::Failed(message.clone())),
            BrowserStatus::Ready(_) => match b.tree.state(b.tree.root()) {
                Some(LoadState::Loading) => Some(BrowserNote::Busy("loading…")),
                Some(LoadState::Failed(message)) => Some(BrowserNote::Failed(message.clone())),
                _ => None,
            },
        }
    }

    /// The catalog of a connection's browser, if `generation` is still the current one.
    #[must_use]
    pub fn catalog(&self, conn: &ConnectionId, generation: u64) -> Option<Catalog> {
        let b = self
            .browsers
            .get(conn)
            .filter(|b| b.generation == generation)?;
        match &b.status {
            BrowserStatus::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        }
    }

    /// The browser generation and catalog of the connection behind `tab`, for the editor's
    /// intellisense. `Some` only while that browser is `Ready`.
    #[must_use]
    pub fn editor_catalog(&self, tab: TabId) -> Option<(u64, Catalog)> {
        let conn = self.tab_conns.get(&tab)?;
        let b = self.browsers.get(conn)?;
        match &b.status {
            BrowserStatus::Ready(catalog) => Some((b.generation, catalog.clone())),
            _ => None,
        }
    }

    /// The menu entries of a node, in menu order.
    #[must_use]
    pub fn menu_entries(&self, conn: &ConnectionId, node: TreeNodeId) -> Vec<MenuEntry> {
        let Some(b) = self.browsers.get(conn) else {
            return Vec::new();
        };
        b.tree
            .kind(node)
            .map(|kind| menu_for(kind, b.dialect))
            .unwrap_or_default()
    }

    /// The DROP confirmation on screen, if any.
    #[must_use]
    pub fn drop_dialog(&self) -> Option<DropDialog> {
        self.drop_flow.as_ref().map(DropFlow::dialog)
    }

    /// The content of an object tab.
    #[must_use]
    pub fn object_tab(&self, tab: TabId) -> Option<&ObjectTab> {
        self.object_tabs.get(&tab)
    }

    /// What a double click on a node does: open the data of an object, else toggle the node.
    #[must_use]
    pub fn default_action(&self, conn: &ConnectionId, node: TreeNodeId) -> BrowserAction {
        if self.menu_entries(conn, node).contains(&MenuEntry::OpenData) {
            BrowserAction::Entry(MenuEntry::OpenData)
        } else {
            BrowserAction::Toggle
        }
    }

    /// The position of the selected line in `items`.
    #[must_use]
    pub fn selected_item(&self, items: &[SidebarItem]) -> Option<usize> {
        self.current_item(items)
    }

    /// The lines of the sidebar list: groups, connections and the visible rows of their trees.
    #[must_use]
    pub fn sidebar_items(&self) -> Vec<SidebarItem> {
        self.sidebar
            .items(|id| self.browsers.get(id).map_or(0, |b| b.tree.rows().len()))
    }

    // ---- selection ----

    /// Selects a node of a connection's tree (and the connection in the sidebar).
    pub fn select_object(&mut self, conn: &ConnectionId, node: TreeNodeId) -> bool {
        let Some(b) = self.browsers.get_mut(conn) else {
            return false;
        };
        if !b.tree.select(node) {
            return false;
        }
        self.sidebar.select_connection(conn)
    }

    /// Selects the connection row: the sidebar selection moves, and so does the tree's.
    pub(super) fn select_connection_row(&mut self, index: usize) {
        if !self.sidebar.select(index) {
            return;
        }
        let Some(id) = self.sidebar.entries().get(index).map(|e| e.id().clone()) else {
            return;
        };
        if let Some(b) = self.browsers.get_mut(&id) {
            let root = b.tree.root();
            b.tree.select(root);
        }
    }

    /// The node the browser commands act on: the selected connection and its selected node (the
    /// root when the connection row itself is selected).
    fn selected_node(&mut self) -> Option<(ConnectionId, TreeNodeId)> {
        let Some(id) = self.sidebar.selected_id().cloned() else {
            self.notice = Some("Select an object in the sidebar first.".to_owned());
            return None;
        };
        let Some(b) = self.browsers.get(&id) else {
            self.notice = Some(
                "Open the connection first: the object browser starts with its first tab."
                    .to_owned(),
            );
            return None;
        };
        let node = b.tree.selected().unwrap_or_else(|| b.tree.root());
        Some((id, node))
    }

    fn current_item(&self, items: &[SidebarItem]) -> Option<usize> {
        match self.sidebar.selection()? {
            Selection::Group(group) => items.iter().position(|item| {
                matches!(
                    item,
                    SidebarItem::Row(SidebarRow { kind: RowKind::Group { id, .. }, .. })
                        if id == group
                )
            }),
            Selection::Connection(conn) => {
                let tree = self.browsers.get(conn).map(|b| &b.tree);
                let object_row = tree.and_then(|t| {
                    t.selected()
                        .filter(|n| *n != t.root())
                        .and_then(|n| t.row_index(n))
                });
                items.iter().position(|item| match (item, object_row) {
                    (SidebarItem::Object { conn: c, row_index }, Some(row)) => {
                        c == conn && *row_index == row
                    }
                    (
                        SidebarItem::Row(SidebarRow {
                            kind: RowKind::Connection { index },
                            ..
                        }),
                        None,
                    ) => self
                        .sidebar
                        .entries()
                        .get(*index)
                        .is_some_and(|e| e.id() == conn),
                    _ => false,
                })
            }
        }
    }

    /// Moves the selection through the whole sidebar list (groups, connections, tree rows).
    fn move_selection(&mut self, delta: isize) {
        let items = self.sidebar_items();
        let Some(last) = items.len().checked_sub(1) else {
            return;
        };
        let target = match self.current_item(&items) {
            Some(i) => i.saturating_add_signed(delta).min(last),
            None if delta > 0 => 0,
            None => last,
        };
        match &items[target] {
            SidebarItem::Row(SidebarRow {
                kind: RowKind::Group { id, .. },
                ..
            }) => {
                self.sidebar.select_group(id);
            }
            SidebarItem::Row(SidebarRow {
                kind: RowKind::Connection { index },
                ..
            }) => self.select_connection_row(*index),
            SidebarItem::Object { conn, row_index } => {
                let node = self
                    .browsers
                    .get(conn)
                    .and_then(|b| b.tree.rows().get(*row_index))
                    .map(|r| r.id);
                if let Some(node) = node {
                    self.select_object(conn, node);
                }
            }
        }
    }

    // ---- actions ----

    /// Runs an action on a node that was clicked.
    pub fn dispatch_object(
        &mut self,
        conn: &ConnectionId,
        node: TreeNodeId,
        action: BrowserAction,
    ) -> Effect {
        if !self.select_object(conn, node) {
            return Effect::None;
        }
        self.run_action(conn.clone(), node, action)
    }

    /// Runs a browser command on the selected node.
    pub fn dispatch_browser(&mut self, action: BrowserAction) -> Effect {
        match action {
            BrowserAction::SelectNext => {
                self.move_selection(1);
                Effect::None
            }
            BrowserAction::SelectPrevious => {
                self.move_selection(-1);
                Effect::None
            }
            BrowserAction::ConfirmDrop => self.confirm_drop(),
            BrowserAction::CancelDrop => {
                if self.drop_flow.as_ref().is_some_and(DropFlow::can_cancel) {
                    self.drop_flow = None;
                    self.drop_parent = None;
                }
                Effect::None
            }
            BrowserAction::CopyTabText => self.copy_tab_text(),
            BrowserAction::OpenTabInEditor => self.open_tab_in_editor(),
            BrowserAction::ShowMenu => Effect::ShowObjectMenu,
            other => match self.selected_node() {
                Some((conn, node)) => self.run_action(conn, node, other),
                None => Effect::None,
            },
        }
    }

    fn run_action(
        &mut self,
        conn: ConnectionId,
        node: TreeNodeId,
        action: BrowserAction,
    ) -> Effect {
        match action {
            BrowserAction::Expand => self.expand_node(&conn, node),
            BrowserAction::Collapse => {
                if let Some(b) = self.browsers.get_mut(&conn) {
                    b.tree.collapse(node);
                }
                Effect::None
            }
            BrowserAction::Toggle => {
                let expanded = self
                    .browsers
                    .get(&conn)
                    .is_some_and(|b| b.tree.is_expanded(node));
                if expanded {
                    self.run_action(conn, node, BrowserAction::Collapse)
                } else {
                    self.expand_node(&conn, node)
                }
            }
            BrowserAction::Entry(entry) => self.run_entry(&conn, node, entry),
            BrowserAction::CancelRefresh => self.cancel_refresh(&conn),
            BrowserAction::SelectNext
            | BrowserAction::SelectPrevious
            | BrowserAction::ShowMenu
            | BrowserAction::ConfirmDrop
            | BrowserAction::CancelDrop
            | BrowserAction::CopyTabText
            | BrowserAction::OpenTabInEditor => self.dispatch_browser(action),
        }
    }

    fn expand_node(&mut self, conn: &ConnectionId, node: TreeNodeId) -> Effect {
        let Some(b) = self.browsers.get_mut(conn) else {
            return Effect::None;
        };
        if node == b.tree.root() {
            match &b.status {
                // The root expands by itself once the session is open.
                BrowserStatus::Connecting => return Effect::None,
                BrowserStatus::Failed(_) => return self.restart_browser(conn),
                BrowserStatus::Ready(_) => {}
            }
        }
        let keys = b.tree.expand(node);
        Self::load(conn, b.generation, keys)
    }

    fn load(conn: &ConnectionId, generation: u64, keys: Vec<CacheKey>) -> Effect {
        if keys.is_empty() {
            Effect::None
        } else {
            Effect::LoadCatalog {
                conn: conn.clone(),
                generation,
                keys,
            }
        }
    }

    fn notify(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    pub(super) fn request_for(&self, conn: &ConnectionId) -> Result<ConnectRequest, String> {
        let profile = self
            .sidebar
            .profile(conn)
            .ok_or_else(|| "The connection no longer exists.".to_owned())?;
        ConnectRequest::from_profile(profile).map_err(|err| err.to_string())
    }

    pub(super) fn ready_catalog(&self, conn: &ConnectionId) -> Option<Catalog> {
        match &self.browsers.get(conn)?.status {
            BrowserStatus::Ready(catalog) => Some(catalog.clone()),
            _ => None,
        }
    }

    /// Opens a preview tab on `conn` for an object and makes it the active one. The focused
    /// group's older preview is replaced: closed through the normal close path, with the new tab
    /// taking its place. An older preview with unsaved text or pending edits is pinned instead
    /// and the new one goes to the end. Returns the tab and the effect of the close.
    pub(super) fn open_object_preview(
        &mut self,
        conn: &ConnectionId,
        kind: TabKind,
        name: &str,
    ) -> Option<(TabId, Effect)> {
        let index = self.sidebar.index_of(conn)?;
        let connection = tab_connection(self.sidebar.entries().get(index)?.config());
        let old = self.tabs.preview_in(self.tabs.focused_group());
        let mut at = None;
        let mut replaced = None;
        if let Some(old) = old {
            if self.editors.is_dirty(old)
                || self.edit.pending_notice(old).is_some()
                || self.edit.is_applying(old)
            {
                self.tabs.pin(old);
            } else {
                at = self.tabs.position_in_group(old).map(|p| p + 1);
                replaced = Some(old);
            }
        }
        let tab = self.tabs.open_preview(connection, kind, name, at);
        self.tab_conns.insert(tab, conn.clone());
        self.show_connection_tabs();
        let effect = replaced.map_or(Effect::None, |old| self.close_tab_now(old));
        Some((tab, effect))
    }

    /// The close effect of a replaced preview, then the effect that opens its successor.
    fn after_close(close: Effect, open: Effect) -> Effect {
        match (close, open) {
            (Effect::None, open) => open,
            (close, Effect::None) => close,
            (close, open) => Effect::Batch(vec![close, open]),
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per menu entry
    fn run_entry(&mut self, conn: &ConnectionId, node: TreeNodeId, entry: MenuEntry) -> Effect {
        let Some(b) = self.browsers.get(conn) else {
            return Effect::None;
        };
        let Some(kind) = b.tree.kind(node).cloned() else {
            return Effect::None;
        };
        let dialect = b.dialect;
        if !menu_for(&kind, dialect).contains(&entry) {
            return self.notify(format!(
                "\"{}\" does not apply to the selected item. Select a table, view or other database object.",
                entry.label()
            ));
        }
        match (entry, kind) {
            (MenuEntry::Refresh, _) => self.start_refresh(conn, node),
            (MenuEntry::OpenData, TreeNodeKind::Object(obj)) => self.open_data_tab(conn, &obj),
            (MenuEntry::OpenInEditor, TreeNodeKind::Object(obj)) => self.open_in_editor(conn, &obj),
            (MenuEntry::GenerateCreate, TreeNodeKind::Object(obj)) => {
                self.generate_into_editor(conn, obj, Script::Create)
            }
            (MenuEntry::GenerateSelect, TreeNodeKind::Object(obj)) => {
                self.generate_into_editor(conn, obj, Script::SelectStar)
            }
            (MenuEntry::GenerateInsert, TreeNodeKind::Object(obj)) => {
                self.generate_into_editor(conn, obj, Script::InsertTemplate)
            }
            (MenuEntry::GenerateDrop, TreeNodeKind::Object(obj)) => {
                self.generate_into_editor(conn, obj, Script::Drop)
            }
            (MenuEntry::Drop, TreeNodeKind::Object(obj)) => self.start_drop(conn, node, obj),
            (MenuEntry::CopyName, TreeNodeKind::Object(obj)) => {
                match dialect.map(|d| qualified_name(d, &obj)) {
                    Some(Ok(name)) => Effect::Copy(name),
                    Some(Err(err)) => self.notify(err.to_string()),
                    None => Effect::None,
                }
            }
            (MenuEntry::CopyName, TreeNodeKind::Column { column, .. }) => {
                match dialect.map(|d| quote_identifier(d, &column.name)) {
                    Some(Ok(name)) => Effect::Copy(name),
                    Some(Err(err)) => self.notify(err.to_string()),
                    None => Effect::None,
                }
            }
            (MenuEntry::Properties, TreeNodeKind::Object(obj)) => {
                let Some(catalog) = self.ready_catalog(conn) else {
                    return self.notify("The object browser is not ready.");
                };
                let name = display_name(dialect, &obj);
                let Some((tab, close)) = self.open_object_preview(conn, TabKind::Properties, &name)
                else {
                    return Effect::None;
                };
                self.object_crumbs.insert(tab, object_segments(&obj));
                let object_tab = ObjectTab::loading(format!("Properties: {name}"));
                self.object_tabs.insert(tab, object_tab);
                Self::after_close(
                    close,
                    Effect::RunJob(BrowserJob::Properties {
                        tab,
                        catalog,
                        object: obj,
                    }),
                )
            }
            (MenuEntry::Properties, TreeNodeKind::Column { table, column }) => {
                let name = format!("{}.{}", display_name(dialect, &table), column.name);
                let Some((tab, close)) = self.open_object_preview(conn, TabKind::Properties, &name)
                else {
                    return Effect::None;
                };
                let mut crumbs = object_segments(&table);
                crumbs.push(column.name.clone());
                self.object_crumbs.insert(tab, crumbs);
                let mut object_tab = ObjectTab::loading(format!("Properties: {name}"));
                object_tab.content =
                    ObjectTabContent::Properties(column_properties(&table, &column));
                self.object_tabs.insert(tab, object_tab);
                close
            }
            (entry, _) => self.notify(format!("\"{}\" does not apply here.", entry.label())),
        }
    }

    fn open_data_tab(&mut self, conn: &ConnectionId, obj: &DbObject) -> Effect {
        let Some(dialect) = self.browsers.get(conn).and_then(|b| b.dialect) else {
            return Effect::None;
        };
        let sql = match select_star(dialect, obj, DEFAULT_ROW_LIMIT) {
            Ok(sql) => sql,
            Err(err) => return self.notify(err.to_string()),
        };
        let request = match self.request_for(conn) {
            Ok(request) => request,
            Err(message) => return self.notify(message),
        };
        let name = display_name(Some(dialect), obj);
        let Some((tab, close)) = self.open_object_preview(conn, TabKind::Grid, &name) else {
            return Effect::None;
        };
        self.object_crumbs.insert(tab, object_segments(obj));
        // The grid edits this object (the editing flow reads it back).
        self.edit.data_tabs.insert(tab, obj.clone());
        let limits = GridLimits::from_settings(self.settings().settings());
        let (model, sink) = grid_channel_with(limits);
        self.grid_cancels.insert(tab, sink.canceller());
        self.grid_objects.insert(tab, obj.clone());
        Self::after_close(
            close,
            Effect::OpenData {
                tab,
                grid: GridTab::new(model),
                job: OpenDataJob { request, sql, sink },
            },
        )
    }

    /// Opens an editor on `conn` that starts with the `SELECT *` of the object.
    fn open_in_editor(&mut self, conn: &ConnectionId, obj: &DbObject) -> Effect {
        let Some(dialect) = self.browsers.get(conn).and_then(|b| b.dialect) else {
            return Effect::None;
        };
        match select_star(dialect, obj, DEFAULT_ROW_LIMIT) {
            Ok(sql) => self.open_editor_on(conn, Some(sql)),
            Err(err) => self.notify(err.to_string()),
        }
    }

    /// Opens the script of the DDL tab on screen in an editor.
    fn open_tab_in_editor(&mut self) -> Effect {
        match self.active_script_text() {
            Some((conn, text)) => self.open_editor_on(&conn, Some(text)),
            None => self.notify("This tab has no script to open."),
        }
    }

    fn generate_into_editor(
        &mut self,
        conn: &ConnectionId,
        obj: DbObject,
        script: Script,
    ) -> Effect {
        let Some(b) = self.browsers.get(conn) else {
            return Effect::None;
        };
        let Some(dialect) = b.dialect else {
            return Effect::None;
        };
        let Some(catalog) = self.ready_catalog(conn) else {
            return self.notify("The object browser is not ready.");
        };
        let request = match self.request_for(conn) {
            Ok(request) => request,
            Err(message) => return self.notify(message),
        };
        let cancel = CancelFlag::new();
        let Some(b) = self.browsers.get_mut(conn) else {
            return Effect::None;
        };
        b.next_script += 1;
        let ticket = b.next_script;
        b.scripts.insert(ticket, cancel.clone());
        Effect::RunJob(BrowserJob::Script {
            conn: conn.clone(),
            generation: b.generation,
            ticket,
            request,
            catalog,
            dialect,
            object: obj,
            script,
            cancel,
        })
    }

    fn copy_tab_text(&mut self) -> Effect {
        let text = self
            .tabs
            .active()
            .and_then(|tab| self.object_tabs.get(&tab.id()))
            .and_then(ObjectTab::copy_text);
        match text {
            Some(text) => Effect::Copy(text),
            None => self.notify("This tab has no text to copy."),
        }
    }

    // ---- DROP ----

    fn start_drop(&mut self, conn: &ConnectionId, node: TreeNodeId, obj: DbObject) -> Effect {
        if self.dialog.is_some() {
            return self.notify("Close the open dialog before dropping an object.");
        }
        if self.drop_flow.as_ref().is_some_and(|f| !f.can_cancel()) {
            return self.notify("A DROP is already running.");
        }
        let Some(b) = self.browsers.get(conn) else {
            return Effect::None;
        };
        let Some(dialect) = b.dialect else {
            return Effect::None;
        };
        let parent = b.tree.parent(node).map(|p| (b.generation, p));
        let Some(entry) = self
            .sidebar
            .index_of(conn)
            .and_then(|i| self.sidebar.entries().get(i))
        else {
            return Effect::None;
        };
        let name = entry.config().name.clone();
        let prod = entry.config().environment == Some(Environment::Prod);
        match DropFlow::start(conn.clone(), name, obj, dialect, prod) {
            Ok(flow) => {
                self.drop_flow = Some(flow);
                self.drop_parent = parent;
                Effect::None
            }
            Err(err) => self.notify(err.to_string()),
        }
    }

    /// Advances the DROP dialog. The statement is only returned from the flow after the last
    /// confirmation, so no other path can produce a `RunJob(Drop)`.
    fn confirm_drop(&mut self) -> Effect {
        let Some(flow) = &self.drop_flow else {
            return Effect::None;
        };
        let conn = flow.connection().clone();
        let request = match self.request_for(&conn) {
            Ok(request) => request,
            Err(message) => {
                self.drop_flow = None;
                self.drop_parent = None;
                return self.notify(message);
            }
        };
        let Some(sql) = self.drop_flow.as_mut().and_then(DropFlow::confirm) else {
            return Effect::None;
        };
        Effect::RunJob(BrowserJob::Drop { conn, request, sql })
    }

    // ---- refresh ----

    fn start_refresh(&mut self, conn: &ConnectionId, node: TreeNodeId) -> Effect {
        let Some(b) = self.browsers.get_mut(conn) else {
            return Effect::None;
        };
        let BrowserStatus::Ready(catalog) = &b.status else {
            return self.notify("The object browser is not ready.");
        };
        let Some(scope) = b.tree.refresh_scope(node) else {
            return Effect::None;
        };
        // A running refresh is replaced; its watcher clears its progress when it ends.
        if let Some((_, flag, _)) = b.refresh.take() {
            flag.cancel();
        }
        let handle = catalog.refresh(scope);
        self.next_refresh += 1;
        let run = self.next_refresh;
        b.refresh = Some((node, handle.cancel_flag(), run));
        Effect::WatchRefresh {
            conn: conn.clone(),
            generation: b.generation,
            node,
            run,
            handle,
        }
    }

    fn cancel_refresh(&mut self, conn: &ConnectionId) -> Effect {
        match self.browsers.get(conn).and_then(|b| b.refresh.as_ref()) {
            Some((_, flag, _)) => {
                flag.cancel();
                Effect::None
            }
            None => self.notify("No refresh is running."),
        }
    }

    /// Reports one event of a watched refresh. `run` is the id from the [`Effect::WatchRefresh`];
    /// the end of a refresh that was replaced never clears the state of the new one.
    pub fn refresh_event(
        &mut self,
        conn: &ConnectionId,
        generation: u64,
        node: TreeNodeId,
        run: u64,
        event: RefreshEvent,
    ) -> Effect {
        let Some(b) = self
            .browsers
            .get_mut(conn)
            .filter(|b| b.generation == generation)
        else {
            return Effect::None;
        };
        let current = b.refresh.as_ref().is_some_and(|(_, _, id)| *id == run);
        match event {
            RefreshEvent::Started { total } if current => {
                b.tree.set_progress(node, Some((0, total)));
            }
            RefreshEvent::Progress { done, total } if current => {
                b.tree.set_progress(node, Some((done, total)));
            }
            RefreshEvent::Finished(outcome) => {
                if current {
                    b.refresh = None;
                    b.tree.set_progress(node, None);
                }
                if let RefreshOutcome::Applied { failed, .. } = outcome {
                    let load = Self::load(conn, generation, b.tree.reload_keys());
                    if failed > 0 {
                        self.notice = Some(format!(
                            "{failed} entries could not be refreshed; their old data was kept."
                        ));
                    }
                    return load;
                }
            }
            _ => {}
        }
        Effect::None
    }

    // ---- results ----

    /// Reports the answer for one key of an [`Effect::LoadCatalog`].
    pub fn catalog_loaded(
        &mut self,
        conn: &ConnectionId,
        generation: u64,
        key: CacheKey,
        result: CatalogResult,
    ) -> Effect {
        let Some(b) = self
            .browsers
            .get_mut(conn)
            .filter(|b| b.generation == generation)
        else {
            return Effect::None;
        };
        match (key, result) {
            (CacheKey::Children(node), CatalogResult::Children(result)) => {
                let more = b.tree.children_loaded(&node, result);
                Self::load(conn, generation, more)
            }
            (CacheKey::Table(obj), CatalogResult::Table(result)) => {
                b.tree.table_loaded(&obj, result);
                // An editable grid may have been waiting for this table.
                self.edit.bump();
                Effect::None
            }
            _ => Effect::None,
        }
    }

    /// Reports the outcome of an [`Effect::RunJob`].
    pub fn job_finished(&mut self, result: BrowserJobResult) -> Effect {
        match result {
            BrowserJobResult::Script {
                conn,
                generation,
                ticket,
                result,
            } => {
                let pending = self
                    .browsers
                    .get_mut(&conn)
                    .filter(|b| b.generation == generation)
                    .and_then(|b| b.scripts.remove(&ticket));
                if pending.is_none() {
                    return Effect::None;
                }
                match result {
                    Ok(text) => self.open_editor_on(&conn, Some(text)),
                    Err(BrowserError::Cancelled) => Effect::None,
                    Err(err) => self.notify(format!("Could not generate the script: {err}")),
                }
            }
            BrowserJobResult::Properties { tab, result } => {
                if let Some(object_tab) = self.object_tabs.get_mut(&tab) {
                    object_tab.content = match result {
                        Ok(fields) => ObjectTabContent::Properties(fields),
                        Err(err) => ObjectTabContent::Failed(err.to_string()),
                    };
                }
                Effect::None
            }
            BrowserJobResult::Drop { conn, result } => {
                if self
                    .drop_flow
                    .as_ref()
                    .is_some_and(|f| *f.connection() == conn)
                {
                    self.drop_flow = None;
                }
                let parent = self.drop_parent.take();
                match result {
                    Ok(()) => match parent {
                        // Node ids of a browser that was restarted since mean something else.
                        Some((generation, parent))
                            if self
                                .browsers
                                .get(&conn)
                                .is_some_and(|b| b.generation == generation) =>
                        {
                            self.start_refresh(&conn, parent)
                        }
                        _ => Effect::None,
                    },
                    Err(err) => self.notify(format!("Drop failed: {err}")),
                }
            }
        }
    }

    // ---- life cycle ----

    /// Starts the browser of a connection that was just opened, unless it has one.
    pub(super) fn ensure_browser(&mut self, conn: &ConnectionId) -> Effect {
        if self.browsers.contains_key(conn) {
            return Effect::None;
        }
        let dialect = self
            .sidebar
            .index_of(conn)
            .and_then(|i| self.sidebar.entries().get(i))
            .and_then(|e| self.dialect_of(&e.config().driver));
        self.next_generation += 1;
        self.browsers.insert(
            conn.clone(),
            BrowserState {
                generation: self.next_generation,
                status: BrowserStatus::Connecting,
                tree: ObjectTree::new(),
                dialect,
                refresh: None,
                scripts: HashMap::new(),
                next_script: 0,
            },
        );
        self.connect_browser(conn)
    }

    /// Opens the browser session of a connection whose browser is in `Connecting`.
    fn connect_browser(&mut self, conn: &ConnectionId) -> Effect {
        match self.request_for(conn) {
            Ok(connect) => {
                let request = self.new_request(Purpose::Browse(conn.clone()));
                Effect::Connect { request, connect }
            }
            Err(message) => {
                if let Some(b) = self.browsers.get_mut(conn) {
                    b.status = BrowserStatus::Failed(message);
                }
                Effect::None
            }
        }
    }

    /// Tries again after a failed start: a new generation, the tree is kept.
    fn restart_browser(&mut self, conn: &ConnectionId) -> Effect {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(b) = self.browsers.get_mut(conn) else {
            return Effect::None;
        };
        b.generation = generation;
        b.status = BrowserStatus::Connecting;
        self.connect_browser(conn)
    }

    /// The browser session of `conn` is open (or failed to open).
    pub(super) fn browse_finished(
        &mut self,
        conn: &ConnectionId,
        result: Result<Box<dyn sqler_driver_api::Session>, ConnectFailure>,
    ) -> Effect {
        let waiting = self
            .browsers
            .get(conn)
            .is_some_and(|b| matches!(b.status, BrowserStatus::Connecting));
        if !waiting {
            return match result {
                Ok(session) => Effect::CloseSession(session),
                Err(_) => Effect::None,
            };
        }
        let Some(b) = self.browsers.get_mut(conn) else {
            return Effect::None;
        };
        match result {
            Ok(session) => {
                let (catalog, worker) = Catalog::new(session);
                b.status = BrowserStatus::Ready(catalog);
                self.edit.bump();
                let generation = b.generation;
                let root = b.tree.root();
                let keys = b.tree.expand(root);
                Effect::Batch(vec![
                    Effect::StartCatalog {
                        conn: conn.clone(),
                        generation,
                        worker,
                    },
                    Self::load(conn, generation, keys),
                ])
            }
            Err(failure) => {
                b.status = BrowserStatus::Failed(failure.to_string());
                Effect::None
            }
        }
    }

    /// Ends the browser of a connection (it was edited or deleted).
    pub(super) fn end_browser(&mut self, conn: &ConnectionId) {
        if let Some(b) = self.browsers.remove(conn) {
            b.shutdown();
            self.edit.bump();
        }
        self.requests
            .retain(|_, purpose| *purpose != Purpose::Browse(conn.clone()));
        if self
            .drop_flow
            .as_ref()
            .is_some_and(|f| f.connection() == conn && f.can_cancel())
        {
            self.drop_flow = None;
            self.drop_parent = None;
        }
    }

    /// Stops every catalog (the app is quitting). `true` if there was a browser to stop.
    pub fn shutdown_browsers(&mut self) -> bool {
        let had = !self.browsers.is_empty();
        for (_, b) in self.browsers.drain() {
            b.shutdown();
        }
        self.requests
            .retain(|_, purpose| !matches!(purpose, Purpose::Browse(_)));
        had
    }

    /// Cancels the work of an object tab that is being closed.
    pub(super) fn close_object_tab(&mut self, tab: TabId) {
        if let Some(object_tab) = self.object_tabs.remove(&tab) {
            object_tab.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use sqler_catalog::{NodeKey, RefreshHandle};
    use sqler_config::ConnectionId;
    use sqler_driver_api::Session;
    use sqler_driver_api::sqler_core::{
        ColumnInfo, ConnectionConfig, DataType, DriverId, ObjectKind, ObjectPath,
    };
    use tokio::runtime::{Builder, Runtime};

    use super::*;
    use crate::browser::testing::{FakeIntrospect, SessionConnector, request};
    use crate::sidebar::SidebarModel;
    use crate::workspace::{SessionBridge, WorkspaceAction};

    fn runtime() -> Runtime {
        Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn main_schema() -> DbObject {
        DbObject::new(ObjectKind::Schema, ObjectPath::root(), "main")
    }

    fn users() -> DbObject {
        DbObject::new(ObjectKind::Table, main_schema().child_path(), "users")
    }

    /// A SQLite-like database: schema `main` with the tables `users` and `orders`.
    fn intro() -> FakeIntrospect {
        let orders = DbObject::new(ObjectKind::Table, main_schema().child_path(), "orders");
        FakeIntrospect::new()
            .with_objects(&ObjectPath::root(), ObjectKind::Schema, vec![main_schema()])
            .with_objects(
                &main_schema().child_path(),
                ObjectKind::Table,
                vec![orders, users()],
            )
            .with_table(
                sqler_driver_api::sqler_core::Table::new(users()).with_columns(vec![
                    ColumnInfo::new("id", DataType::Integer),
                    ColumnInfo::new("name", DataType::Text),
                ]),
            )
    }

    fn workspace(environment: Environment) -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver, ":memory:").with_environment(environment),
        ]))
    }

    #[test]
    fn a_second_preview_replaces_the_first_in_its_slot() {
        let mut w = workspace(Environment::Dev);
        let conn = conn_id(&w);
        let (pinned, _) = w.open_object_preview(&conn, TabKind::Ddl, "a").unwrap();
        w.tabs.pin(pinned);
        let (after, _) = w.open_object_preview(&conn, TabKind::Ddl, "b").unwrap();
        w.tabs.pin(after);
        let (first, _) = w
            .open_object_preview(&conn, TabKind::Grid, "main.users")
            .unwrap();
        w.tabs.activate(first);
        let (_grid, cancel) = crate::grid::GridTab::spawn_synthetic(100, 2, 50);
        w.grid_cancels.insert(first, cancel.clone());
        let slot = w.tabs.position_in_group(first).unwrap();
        let (second, effect) = w
            .open_object_preview(&conn, TabKind::Grid, "main.orders")
            .unwrap();
        assert!(matches!(effect, Effect::None));
        assert!(cancel.is_cancelled(), "the first grid is cancelled");
        assert!(w.tabs.get(first).is_none());
        assert_eq!(w.tabs.position_in_group(second), Some(slot));
        assert_eq!(w.tabs.preview_in(w.tabs.focused_group()), Some(second));
        assert_eq!(w.tabs.len(), 3);
        assert!(w.tabs.get(pinned).is_some());
    }

    #[test]
    fn a_dirty_preview_is_pinned_not_replaced() {
        let mut w = workspace(Environment::Dev);
        let conn = conn_id(&w);
        let (first, _) = w
            .open_object_preview(&conn, TabKind::Ddl, "main.users")
            .unwrap();
        // Pending edits, staged without going through `editor_edited` (which pins by itself),
        // so the preview is still a preview when the next one opens.
        w.set_pending_edits(
            crate::grid::edit::GridKey {
                tab: first,
                result: None,
            },
            2,
        );
        assert_eq!(w.tabs.mode(first), Some(crate::tabs::TabMode::Preview));
        let (second, effect) = w
            .open_object_preview(&conn, TabKind::Ddl, "main.orders")
            .unwrap();
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.tabs.mode(first), Some(crate::tabs::TabMode::Pinned));
        assert_eq!(w.tabs.mode(second), Some(crate::tabs::TabMode::Preview));
        assert_eq!(w.tabs.len(), 2);
    }

    #[test]
    fn an_applying_preview_is_pinned_not_replaced() {
        let mut w = workspace(Environment::Dev);
        let conn = conn_id(&w);
        let (first, _) = w
            .open_object_preview(&conn, TabKind::Grid, "main.users")
            .unwrap();
        w.edit.mark_applying(first);
        let (second, effect) = w
            .open_object_preview(&conn, TabKind::Grid, "main.orders")
            .unwrap();
        assert!(matches!(effect, Effect::None));
        assert!(w.tabs.get(first).is_some(), "the applying tab stays");
        assert_eq!(w.tabs.mode(first), Some(crate::tabs::TabMode::Pinned));
        assert_eq!(w.tabs.mode(second), Some(crate::tabs::TabMode::Preview));
        assert_eq!(w.tabs.len(), 2);
    }

    fn conn_id(w: &Workspace) -> ConnectionId {
        w.sidebar().entries()[0].id().clone()
    }

    /// What a test harness (standing in for the view) found while carrying out effects.
    #[derive(Default)]
    struct Seen {
        jobs: Vec<BrowserJob>,
        copies: Vec<String>,
        watches: Vec<(ConnectionId, u64, TreeNodeId, u64, RefreshHandle)>,
        grids: Vec<(TabId, OpenDataJob)>,
        models: Vec<crate::grid::GridTab>,
        closed: usize,
        menus: usize,
    }

    struct Harness {
        rt: Runtime,
        connector: SessionConnector,
        bridge: SessionBridge,
        w: Workspace,
        seen: Seen,
        runs: Vec<tokio::sync::oneshot::Receiver<()>>,
    }

    impl Harness {
        fn new(environment: Environment, intro: FakeIntrospect) -> Self {
            let rt = runtime();
            let connector = SessionConnector::new(intro);
            let bridge = SessionBridge::new(
                Arc::new(connector.clone()),
                Arc::new(sqler_config::MemoryStore::new()),
                rt.handle().clone(),
            );
            Self {
                rt,
                connector,
                bridge,
                w: workspace(environment),
                seen: Seen::default(),
                runs: Vec::new(),
            }
        }

        /// Carries out an effect the way the view does, synchronously.
        fn apply(&mut self, effect: Effect) {
            match effect {
                Effect::None => {}
                Effect::Batch(effects) => {
                    for effect in effects {
                        self.apply(effect);
                    }
                }
                Effect::Connect { request, connect } => {
                    let result = self.rt.block_on(self.bridge.connect(connect));
                    let next = self.w.connect_finished(request, result);
                    self.apply(next);
                }
                Effect::StartCatalog { worker, .. } => {
                    self.runs.push(self.bridge.run_catalog(worker));
                }
                Effect::LoadCatalog {
                    conn,
                    generation,
                    keys,
                } => {
                    let catalog = self.w.catalog(&conn, generation).expect("catalog");
                    for key in keys {
                        let result = match &key {
                            CacheKey::Children(k) => CatalogResult::Children(
                                self.rt.block_on(catalog.children(k.clone())),
                            ),
                            CacheKey::Table(o) => {
                                CatalogResult::Table(self.rt.block_on(catalog.table(o.clone())))
                            }
                            _ => continue,
                        };
                        let next = self.w.catalog_loaded(&conn, generation, key, result);
                        self.apply(next);
                    }
                }
                Effect::WatchRefresh {
                    conn,
                    generation,
                    node,
                    run,
                    handle,
                } => self
                    .seen
                    .watches
                    .push((conn, generation, node, run, handle)),
                Effect::RunJob(job) => self.seen.jobs.push(job),
                Effect::OpenData { tab, job, grid } => {
                    self.seen.grids.push((tab, job));
                    self.seen.models.push(grid);
                }
                Effect::Copy(text) => self.seen.copies.push(text),
                Effect::CloseSession(session) => {
                    self.seen.closed += 1;
                    drop(session);
                }
                Effect::ShowObjectMenu => self.seen.menus += 1,
                other => panic!("unexpected effect {other:?}"),
            }
        }

        /// Runs the pending job and reports it.
        fn run_job(&mut self) {
            let job = self.seen.jobs.remove(0);
            let result = self.rt.block_on(job.run(self.bridge.connector()));
            let next = self.w.job_finished(result);
            self.apply(next);
        }

        /// Opens the connection and lets the browser load its first level.
        fn open(&mut self) {
            let effect = self.w.dispatch(WorkspaceAction::OpenConnection(0));
            self.apply(effect);
        }

        fn labels(&self) -> Vec<String> {
            let id = conn_id(&self.w);
            self.w
                .browser(&id)
                .unwrap()
                .rows()
                .iter()
                .map(|r| format!("{}{}", "  ".repeat(r.depth), r.label))
                .collect()
        }

        fn node(&self, label: &str) -> TreeNodeId {
            let id = conn_id(&self.w);
            self.w
                .browser(&id)
                .unwrap()
                .rows()
                .iter()
                .find(|r| r.label == label)
                .unwrap_or_else(|| panic!("no row {label}: {:?}", self.labels()))
                .id
        }

        fn click(&mut self, label: &str, action: BrowserAction) {
            let id = conn_id(&self.w);
            let node = self.node(label);
            let effect = self.w.dispatch_object(&id, node, action);
            self.apply(effect);
        }
    }

    #[test]
    fn open_starts_the_browser_and_loads_the_first_level() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let id = conn_id(&h.w);
        // SQLite has no databases: the root falls through to the schemas.
        assert_eq!(h.labels(), ["Schemas (1)"]);
        assert!(h.w.browser_note(&id).is_none());
        assert_eq!(h.w.browser_expanded(&id), Some(true));
        // One tab session and one browser session.
        assert_eq!(h.w.tabs().len(), 1);
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        assert_eq!(h.labels()[0..2], ["Schemas (1)", "  main"]);
        assert!(h.labels().contains(&"    Tables (2)".to_owned()));
    }

    #[test]
    fn editor_catalog_follows_the_browser() {
        let mut h = Harness::new(Environment::Dev, intro());
        let effect = h.w.dispatch(WorkspaceAction::OpenConnection(0));
        let Effect::Connect { request, connect } = effect else {
            panic!("expected Connect");
        };
        let result = h.rt.block_on(h.bridge.connect(connect));
        let browse = h.w.connect_finished(request, result);
        let tab = h.w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        // The browser is still connecting.
        assert!(h.w.editor_catalog(tab).is_none());
        h.apply(browse);
        let (generation, _catalog) = h.w.editor_catalog(tab).expect("ready");
        assert!(generation > 0);
        let id = conn_id(&h.w);
        h.w.end_browser(&id);
        assert!(h.w.editor_catalog(tab).is_none());
    }

    #[test]
    fn a_second_open_does_not_start_a_second_browser() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let effect = h.w.dispatch(WorkspaceAction::OpenConnection(0));
        let Effect::Connect { request, connect } = effect else {
            panic!("expected Connect");
        };
        let result = h.rt.block_on(h.bridge.connect(connect));
        // The tab connect finishes; no Browse connect follows.
        assert!(matches!(
            h.w.connect_finished(request, result),
            Effect::None
        ));
        assert_eq!(h.w.tabs().len(), 2);
    }

    #[test]
    fn a_failed_browser_shows_the_error_and_expanding_retries() {
        let mut h = Harness::new(Environment::Dev, intro());
        let effect = h.w.dispatch(WorkspaceAction::OpenConnection(0));
        let Effect::Connect { request, connect } = effect else {
            panic!("expected Connect");
        };
        let result = h.rt.block_on(h.bridge.connect(connect));
        let Effect::Connect {
            request: browse, ..
        } = h.w.connect_finished(request, result)
        else {
            panic!("expected the Browse connect");
        };
        let id = conn_id(&h.w);
        assert_eq!(
            h.w.browser_note(&id),
            Some(BrowserNote::Busy("opening browser…"))
        );
        assert!(matches!(
            h.w.connect_finished(browse, Err(ConnectFailure::Tunnel("nope".into()))),
            Effect::None
        ));
        assert_eq!(
            h.w.browser_note(&id),
            Some(BrowserNote::Failed("SSH tunnel failed: nope".into()))
        );
        let root = h.w.browser(&id).unwrap().root();
        let effect = h.w.dispatch_object(&id, root, BrowserAction::Expand);
        assert!(matches!(&effect, Effect::Connect { .. }));
        h.apply(effect);
        assert_eq!(h.labels(), ["Schemas (1)"]);
    }

    #[test]
    fn a_stale_browse_result_closes_its_session() {
        let mut h = Harness::new(Environment::Dev, intro());
        let effect = h.w.dispatch(WorkspaceAction::OpenConnection(0));
        let Effect::Connect { request, connect } = effect else {
            panic!("expected Connect");
        };
        let result = h.rt.block_on(h.bridge.connect(connect));
        let Effect::Connect {
            request: browse, ..
        } = h.w.connect_finished(request, result)
        else {
            panic!("expected the Browse connect");
        };
        let id = conn_id(&h.w);
        h.w.end_browser(&id);
        let (session, closed) = crate::workspace::testing::FakeSession::boxed();
        assert!(matches!(
            h.w.connect_finished(browse, Ok(session)),
            Effect::CloseSession(_)
        ));
        let _ = closed;
    }

    #[test]
    fn stale_generations_are_ignored() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let id = conn_id(&h.w);
        let root = h.w.browser(&id).unwrap().root();
        let before = h.labels();
        let key = NodeKey::new(ObjectPath::root(), ObjectKind::Schema);
        let effect = h.w.catalog_loaded(
            &id,
            999,
            CacheKey::Children(key.clone()),
            CatalogResult::Children(Ok(Arc::new(Listing::Objects(Vec::new())))),
        );
        assert!(matches!(effect, Effect::None));
        assert_eq!(h.labels(), before);
        assert!(h.w.catalog(&id, 999).is_none());
        assert!(matches!(
            h.w.refresh_event(&id, 999, root, 1, RefreshEvent::Started { total: 1 }),
            Effect::None
        ));
        assert_eq!(h.w.browser(&id).unwrap().progress(root), None);
    }

    #[test]
    fn open_data_runs_select_star_in_a_grid_tab() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        assert_eq!(h.seen.grids.len(), 1);
        let (tab, job) = h.seen.grids.remove(0);
        assert_eq!(job.sql, "SELECT *\nFROM \"main\".\"users\"\nLIMIT 1000;\n");
        let tab = h.w.tabs().get(tab).unwrap();
        assert_eq!(tab.kind(), TabKind::Grid);
        assert_eq!(tab.title(), "Results: \"main\".\"users\"");
    }

    /// Expands the sqlite tree of `Harness` down to the `Tables` level.
    fn expand_tables(h: &mut Harness) {
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
    }

    #[test]
    fn object_opens_are_previews_and_a_second_one_replaces_the_first() {
        let mut h = Harness::new(Environment::Dev, intro());
        expand_tables(&mut h);
        let query_tab = h.w.tabs().active().unwrap().id();

        // Open data.
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        let (first, _) = h.seen.grids.remove(0);
        assert_eq!(h.w.tabs().mode(first), Some(crate::tabs::TabMode::Preview));
        assert_eq!(
            h.w.tabs().mode(query_tab),
            Some(crate::tabs::TabMode::Pinned)
        );
        let cancel = h.w.grid_cancels.get(&first).cloned().unwrap();
        assert!(!cancel.is_cancelled());

        // Properties replaces it through the close path: the grid cancel fires and its state is gone.
        h.click("users", BrowserAction::Entry(MenuEntry::Properties));
        assert!(cancel.is_cancelled());
        assert!(h.w.tabs().get(first).is_none());
        assert!(!h.w.grid_cancels.contains_key(&first));
        assert!(!h.w.grid_objects.contains_key(&first));
        let props = h.w.tabs().active().unwrap();
        assert_eq!(props.kind(), TabKind::Properties);
        assert_eq!(props.mode(), crate::tabs::TabMode::Preview);
        assert_eq!(h.w.tabs().len(), 2);
        assert_eq!(h.seen.jobs.len(), 1, "properties starts its job");
    }

    #[test]
    fn a_preview_with_pending_edits_is_pinned_by_the_next_object_open() {
        let mut h = Harness::new(Environment::Dev, intro());
        expand_tables(&mut h);
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        let (first, _) = h.seen.grids.remove(0);
        h.w.set_pending_edits(
            crate::grid::edit::GridKey {
                tab: first,
                result: None,
            },
            1,
        );
        h.click("orders", BrowserAction::Entry(MenuEntry::OpenData));
        let (second, _) = h.seen.grids.remove(0);
        assert_eq!(h.w.tabs().mode(first), Some(crate::tabs::TabMode::Pinned));
        assert_eq!(h.w.tabs().mode(second), Some(crate::tabs::TabMode::Preview));
        assert!(h.w.tabs().get(first).is_some());
    }

    #[test]
    fn the_active_tab_follows_the_focused_group() {
        let mut h = Harness::new(Environment::Dev, intro());
        expand_tables(&mut h);
        let query_tab = h.w.tabs().active().unwrap().id();
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        let (grid, _) = h.seen.grids.remove(0);
        h.w.dispatch(WorkspaceAction::SplitRight);
        let groups: Vec<_> =
            h.w.tabs()
                .groups()
                .iter()
                .map(crate::tabs::Group::id)
                .collect();
        assert_eq!(groups.len(), 2);
        // The preview grid went to group 2, which has the focus.
        assert_eq!(h.w.tabs().active().map(crate::tabs::Tab::id), Some(grid));
        h.w.dispatch(WorkspaceAction::FocusGroup(groups[0]));
        assert_eq!(
            h.w.tabs().active().map(crate::tabs::Tab::id),
            Some(query_tab)
        );
        assert_eq!(
            h.w.tabs().active_in(groups[1]).map(crate::tabs::Tab::id),
            Some(grid)
        );
        h.w.dispatch(WorkspaceAction::FocusGroup(groups[1]));
        let active = h.w.tabs().active().unwrap();
        assert_eq!(active.id(), grid);
        assert_eq!(active.kind(), TabKind::Grid);
    }

    #[test]
    fn insert_target_is_the_real_table_for_open_data_and_the_placeholder_otherwise() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        let query_tab = h.w.tabs().active().unwrap().id();
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        let (data_tab, _job) = h.seen.grids.remove(0);
        let real = h.w.insert_target(data_tab).unwrap();
        assert_eq!(real.table, "users");
        assert_eq!(real.schema.as_deref(), Some("main"));
        assert_eq!(real.dialect, sqler_sql::Dialect::Sqlite);
        assert!(!real.assumed_dialect);
        let placeholder = h.w.insert_target(query_tab).unwrap();
        assert_eq!(placeholder.table, crate::grid::PLACEHOLDER_TABLE);
        assert_eq!(placeholder.schema, None);
        assert_eq!(placeholder.dialect, sqler_sql::Dialect::Sqlite);
        let effect = h.w.dispatch(WorkspaceAction::CloseTab(data_tab));
        h.apply(effect);
        assert_eq!(h.w.insert_target(data_tab), None);
        assert!(h.w.grid_objects.is_empty());
    }

    /// Opens a grid on `users`, starts a synthetic ingestion on its sink that outgrows the default
    /// page, and waits until it is paused. Returns the model so the sink stays connected.
    fn paused_open_data(
        h: &mut Harness,
    ) -> (
        TabId,
        crate::grid::GridTab,
        std::thread::JoinHandle<Result<crate::grid::IngestSummary, sqler_driver_api::DriverError>>,
    ) {
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        h.click("users", BrowserAction::Entry(MenuEntry::OpenData));
        let (tab, job) = h.seen.grids.remove(0);
        let mut grid = h.seen.models.remove(0);
        let rows = sqler_config::DEFAULT_RESULT_PAGE_ROWS as usize + 50_000;
        let handle =
            crate::grid::spawn_ingest(crate::grid::SyntheticSource::new(rows, 2, 1024), job.sink)
                .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !matches!(grid.state(), crate::grid::IngestState::Paused) {
            assert!(std::time::Instant::now() < deadline, "never paused");
            grid.poll(usize::MAX);
            std::thread::sleep(Duration::from_millis(1));
        }
        (tab, grid, handle)
    }

    fn assert_cancelled(
        mut grid: crate::grid::GridTab,
        handle: std::thread::JoinHandle<
            Result<crate::grid::IngestSummary, sqler_driver_api::DriverError>,
        >,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !handle.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the paused job stayed parked"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            handle.join().unwrap(),
            Err(sqler_driver_api::DriverError::Cancelled)
        ));
        grid.poll(usize::MAX);
        assert_eq!(grid.state(), &crate::grid::IngestState::Cancelled);
    }

    #[test]
    fn closing_an_open_data_tab_wakes_its_paused_job() {
        let mut h = Harness::new(Environment::Dev, intro());
        let (tab, grid, handle) = paused_open_data(&mut h);
        // The model stays alive here, so only the stored canceller can end the pause.
        let effect = h.w.dispatch(WorkspaceAction::CloseTab(tab));
        h.apply(effect);
        assert_cancelled(grid, handle);
    }

    #[test]
    fn quit_wakes_a_paused_open_data_job() {
        let mut h = Harness::new(Environment::Dev, intro());
        let (_tab, grid, handle) = paused_open_data(&mut h);
        drop(h.w.dispatch(WorkspaceAction::Quit));
        assert_cancelled(grid, handle);
    }

    #[test]
    fn open_in_editor_seeds_a_new_editor_with_select_star() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        let tabs_before = h.w.tabs().len();
        h.click("users", BrowserAction::Entry(MenuEntry::OpenInEditor));
        assert_eq!(h.w.tabs().len(), tabs_before + 1);
        let tab = h.w.tabs().active().unwrap().id();
        assert_eq!(h.w.tabs().get(tab).unwrap().kind(), TabKind::Query);
        assert!(h.w.editor_doc(tab).is_some());
        assert_eq!(
            h.w.take_editor_seed(tab).as_deref(),
            Some("SELECT *\nFROM \"main\".\"users\"\nLIMIT 1000;\n")
        );
        assert!(h.seen.grids.is_empty(), "no grid is opened");
    }

    /// Expands the tree, clicks `entry` on `users` and returns the number of tabs before.
    fn generate(h: &mut Harness, entry: MenuEntry) -> usize {
        expand_tables(h);
        let before = h.w.tabs().len();
        h.click("users", BrowserAction::Entry(entry));
        before
    }

    /// The seed of the editor that the last generation opened.
    fn generated_seed(h: &mut Harness) -> String {
        let tab = h.w.tabs().active().unwrap().id();
        assert_eq!(h.w.tabs().get(tab).unwrap().kind(), TabKind::Query);
        assert!(h.w.editor_doc(tab).is_some());
        h.w.take_editor_seed(tab).expect("a seed")
    }

    #[test]
    fn generate_create_opens_an_editor_with_the_script() {
        let mut h = Harness::new(
            Environment::Dev,
            intro().with_definition(&users(), "unused"),
        );
        let before = generate(&mut h, MenuEntry::GenerateCreate);
        assert_eq!(
            h.w.tabs().len(),
            before,
            "no tab before the script is ready"
        );
        assert_eq!(h.seen.jobs.len(), 1);
        h.run_job();
        assert_eq!(h.w.tabs().len(), before + 1);
        let tab = h.w.tabs().active().unwrap().id();
        assert_eq!(h.w.tab_conns.get(&tab), Some(&conn_id(&h.w)));
        let seed = generated_seed(&mut h);
        assert!(seed.contains("CREATE TABLE \"main\".\"users\""), "{seed}");
        assert!(h.seen.grids.is_empty());
    }

    #[test]
    fn generate_select_opens_an_editor_with_select_star() {
        let mut h = Harness::new(Environment::Dev, intro());
        generate(&mut h, MenuEntry::GenerateSelect);
        h.run_job();
        assert_eq!(
            generated_seed(&mut h),
            "SELECT *\nFROM \"main\".\"users\"\nLIMIT 1000;\n"
        );
        assert!(h.seen.grids.is_empty(), "nothing ran");
    }

    #[test]
    fn generate_insert_and_drop_open_editors() {
        let mut h = Harness::new(Environment::Dev, intro());
        generate(&mut h, MenuEntry::GenerateInsert);
        h.run_job();
        assert!(generated_seed(&mut h).starts_with("INSERT INTO"));
        h.click("users", BrowserAction::Entry(MenuEntry::GenerateDrop));
        h.run_job();
        assert_eq!(generated_seed(&mut h), "DROP TABLE \"main\".\"users\";\n");
        assert!(h.w.drop_flow.is_none(), "no confirmation dialog");
        assert!(h.seen.grids.is_empty());
    }

    #[test]
    fn a_failed_generation_notifies_and_opens_no_editor() {
        let mut h = Harness::new(Environment::Dev, intro());
        let before = generate(&mut h, MenuEntry::GenerateSelect);
        let BrowserJob::Script {
            conn,
            generation,
            ticket,
            ..
        } = h.seen.jobs.remove(0)
        else {
            panic!("expected a script job");
        };
        let effect = h.w.job_finished(BrowserJobResult::Script {
            conn,
            generation,
            ticket,
            result: Err(BrowserError::NotFound),
        });
        h.apply(effect);
        assert_eq!(h.w.tabs().len(), before);
        let notice = h.w.take_notice().expect("a notice");
        assert!(
            notice.starts_with("Could not generate the script:"),
            "{notice}"
        );
    }

    #[test]
    fn a_stale_script_result_is_ignored() {
        let mut h = Harness::new(Environment::Dev, intro());
        let before = generate(&mut h, MenuEntry::GenerateSelect);
        let effect = h.w.job_finished(BrowserJobResult::Script {
            conn: conn_id(&h.w),
            generation: 1,
            ticket: 999,
            result: Ok("select 1".into()),
        });
        assert!(matches!(effect, Effect::None));
        assert_eq!(h.w.tabs().len(), before);
    }

    #[test]
    fn ending_the_browser_cancels_a_pending_script() {
        let mut h = Harness::new(Environment::Dev, intro());
        let before = generate(&mut h, MenuEntry::GenerateSelect);
        let BrowserJob::Script { cancel, .. } = &h.seen.jobs[0] else {
            panic!("expected a script job");
        };
        let cancel = cancel.clone();
        assert!(!cancel.is_cancelled());
        let id = conn_id(&h.w);
        h.w.end_browser(&id);
        assert!(cancel.is_cancelled());
        h.run_job();
        assert_eq!(h.w.tabs().len(), before);
    }

    #[test]
    fn a_ddl_tab_opens_its_script_in_an_editor() {
        let mut h = Harness::new(Environment::Dev, intro());
        expand_tables(&mut h);
        let conn = conn_id(&h.w);
        let (ddl, _) =
            h.w.open_object_preview(&conn, TabKind::Ddl, "main.users")
                .unwrap();
        let mut object_tab = ObjectTab::loading("DDL: main.users");
        // Still loading: nothing to open.
        h.w.object_tabs.insert(ddl, object_tab);
        let effect = h.w.dispatch_browser(BrowserAction::OpenTabInEditor);
        h.apply(effect);
        assert!(h.w.take_notice().is_some());
        object_tab = ObjectTab::loading("DDL: main.users");
        object_tab.content = ObjectTabContent::Script("select 1".into());
        h.w.object_tabs.insert(ddl, object_tab);
        let effect = h.w.dispatch_browser(BrowserAction::OpenTabInEditor);
        h.apply(effect);
        let editor = h.w.tabs().active().unwrap().id();
        assert_ne!(editor, ddl);
        assert_eq!(h.w.take_editor_seed(editor).as_deref(), Some("select 1"));
    }

    #[test]
    fn column_properties_open_without_a_job() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        h.click("users", BrowserAction::Expand);
        h.click("Columns (2)", BrowserAction::Expand);
        h.click("id  integer", BrowserAction::Entry(MenuEntry::Properties));
        assert!(h.seen.jobs.is_empty());
        let tab = h.w.tabs().active().unwrap();
        assert_eq!(tab.kind(), TabKind::Properties);
        let ObjectTabContent::Properties(fields) = &h.w.object_tab(tab.id()).unwrap().content
        else {
            panic!("expected properties");
        };
        assert!(fields.iter().any(|p| p.label == "Name" && p.value == "id"));
    }

    #[test]
    fn copy_name_quotes_with_the_dialect() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
        h.click("users", BrowserAction::Entry(MenuEntry::CopyName));
        h.click("users", BrowserAction::Expand);
        h.click("Columns (2)", BrowserAction::Expand);
        h.click("id  integer", BrowserAction::Entry(MenuEntry::CopyName));
        assert_eq!(h.seen.copies, ["\"main\".\"users\"", "\"id\""]);
    }

    #[test]
    fn a_command_that_does_not_apply_shows_a_notice() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Entry(MenuEntry::OpenData));
        assert!(h.w.take_notice().unwrap().contains("does not apply"));
        // Nothing selected and not opened: the commands say so.
        let mut w = workspace(Environment::Dev);
        assert!(matches!(
            w.dispatch_browser(BrowserAction::Entry(MenuEntry::Refresh)),
            Effect::None
        ));
        assert!(w.take_notice().unwrap().contains("Select an object"));
        w.dispatch(WorkspaceAction::SelectConnection(0));
        w.dispatch_browser(BrowserAction::Expand);
        assert!(
            w.take_notice()
                .unwrap()
                .contains("Open the connection first")
        );
    }

    fn table_row(h: &mut Harness) {
        h.open();
        h.click("Schemas (1)", BrowserAction::Expand);
        h.click("main", BrowserAction::Expand);
        h.click("Tables (2)", BrowserAction::Expand);
    }

    #[test]
    fn drop_needs_one_confirmation_and_is_never_a_job_before_it() {
        let mut h = Harness::new(Environment::Dev, intro());
        table_row(&mut h);
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        assert!(h.seen.jobs.is_empty());
        let dialog = h.w.drop_dialog().unwrap();
        assert_eq!(dialog.sql, "DROP TABLE \"main\".\"users\";\n");
        assert!(dialog.title.starts_with("Drop table"));
        // Cancelling leaves no job.
        let effect = h.w.dispatch_browser(BrowserAction::CancelDrop);
        h.apply(effect);
        assert!(h.w.drop_dialog().is_none());
        assert!(h.seen.jobs.is_empty());
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        assert!(matches!(effect, Effect::None), "no flow, no job");
        // Confirming runs the statement that was shown.
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        let shown = h.w.drop_dialog().unwrap().sql;
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        let Effect::RunJob(BrowserJob::Drop { sql, .. }) = &effect else {
            panic!("expected the Drop job, got {effect:?}");
        };
        assert_eq!(*sql, shown);
        // While it runs the dialog cannot be cancelled.
        assert!(h.w.drop_dialog().unwrap().confirm_label.is_none());
        h.w.dispatch_browser(BrowserAction::CancelDrop);
        assert!(h.w.drop_dialog().is_some());
        h.apply(effect);
        h.run_job();
        assert!(h.w.drop_dialog().is_none());
        assert_eq!(h.connector.sql(), ["DROP TABLE \"main\".\"users\";\n"]);
        // The parent folder is refreshed afterwards.
        assert_eq!(h.seen.watches.len(), 1);
    }

    #[test]
    fn a_lost_drop_job_ends_the_flow_with_a_notice() {
        let mut h = Harness::new(Environment::Dev, intro());
        table_row(&mut h);
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        let Effect::RunJob(job) = effect else {
            panic!("expected the Drop job");
        };
        assert!(h.w.drop_dialog().unwrap().confirm_label.is_none());
        // The task panicked: the view feeds `lost()` back instead of waiting for ever.
        let next = h.w.job_finished(job.lost());
        h.apply(next);
        assert!(h.w.drop_dialog().is_none());
        let notice = h.w.take_notice().unwrap();
        assert!(notice.contains("Drop failed"), "{notice}");
        assert!(notice.contains("unexpectedly"), "{notice}");
    }

    #[test]
    fn drop_on_prod_needs_a_second_confirmation() {
        let mut h = Harness::new(Environment::Prod, intro());
        table_row(&mut h);
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        let first = h.w.drop_dialog().unwrap();
        assert_eq!(first.confirm_label, Some("Drop"));
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        assert!(matches!(effect, Effect::None));
        assert!(h.seen.jobs.is_empty());
        let second = h.w.drop_dialog().unwrap();
        assert_eq!(second.title, "Production connection");
        assert_eq!(second.confirm_label, Some("Drop on production"));
        assert_eq!(second.sql, first.sql);
        // The second step can still be cancelled.
        h.w.dispatch_browser(BrowserAction::CancelDrop);
        assert!(h.w.drop_dialog().is_none());
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        // A double click or key repeat right after the first step does not confirm.
        assert!(matches!(
            h.w.dispatch_browser(BrowserAction::ConfirmDrop),
            Effect::None
        ));
        assert!(h.seen.jobs.is_empty());
        assert_eq!(
            h.w.drop_dialog().unwrap().confirm_label,
            Some("Drop on production")
        );
        h.w.drop_flow.as_mut().unwrap().age_prod_step();
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        assert!(matches!(effect, Effect::RunJob(BrowserJob::Drop { .. })));
        // A third confirm while running does nothing.
        assert!(matches!(
            h.w.dispatch_browser(BrowserAction::ConfirmDrop),
            Effect::None
        ));
    }

    #[test]
    fn drop_is_refused_while_a_connection_dialog_is_open() {
        let mut h = Harness::new(Environment::Dev, intro());
        table_row(&mut h);
        h.w.dispatch_connection(crate::connections::ConnectionAction::New);
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        assert!(h.w.drop_dialog().is_none());
        assert!(h.w.take_notice().unwrap().contains("dialog"));
    }

    #[test]
    fn a_failed_drop_is_a_notice_and_the_dialog_closes() {
        let rt_intro = intro();
        let mut h = Harness::new(Environment::Dev, rt_intro);
        // Make the connector fail the statement.
        h.connector = h.connector.clone().failing_execute("table is locked");
        h.bridge = SessionBridge::new(
            Arc::new(h.connector.clone()),
            Arc::new(sqler_config::MemoryStore::new()),
            h.rt.handle().clone(),
        );
        table_row(&mut h);
        h.click("users", BrowserAction::Entry(MenuEntry::Drop));
        let effect = h.w.dispatch_browser(BrowserAction::ConfirmDrop);
        h.apply(effect);
        h.run_job();
        assert!(h.w.drop_dialog().is_none());
        assert!(h.w.take_notice().unwrap().contains("table is locked"));
        assert!(h.seen.watches.is_empty());
    }

    #[test]
    fn refresh_reports_progress_and_reloads_the_tree() {
        let mut h = Harness::new(Environment::Dev, intro());
        table_row(&mut h);
        let id = conn_id(&h.w);
        h.click("Tables (2)", BrowserAction::Entry(MenuEntry::Refresh));
        let (conn, generation, node, run, mut handle) = h.seen.watches.remove(0);
        // Run the refresh to its end the way the watcher task does.
        let mut events = Vec::new();
        while let Some(event) = h.rt.block_on(handle.next_event()) {
            let end = matches!(event, RefreshEvent::Finished(_));
            events.push(event);
            if end {
                break;
            }
        }
        for event in events {
            let progress = matches!(event, RefreshEvent::Started { .. });
            let effect = h.w.refresh_event(&conn, generation, node, run, event);
            if progress {
                assert_eq!(h.w.browser(&id).unwrap().progress(node), Some((0, 1)));
            }
            h.apply(effect);
        }
        assert_eq!(h.w.browser(&id).unwrap().progress(node), None);
        assert!(
            h.labels().contains(&"  users".to_owned())
                || h.labels().contains(&"      users".to_owned())
        );
    }

    #[test]
    fn a_new_refresh_cancels_the_running_one() {
        let mut h = Harness::new(Environment::Dev, intro());
        table_row(&mut h);
        let id = conn_id(&h.w);
        h.click("Tables (2)", BrowserAction::Entry(MenuEntry::Refresh));
        let first_flag =
            h.w.browsers
                .get(&id)
                .unwrap()
                .refresh
                .as_ref()
                .unwrap()
                .1
                .clone();
        h.click("Tables (2)", BrowserAction::Entry(MenuEntry::Refresh));
        assert!(first_flag.is_cancelled());
        let second_flag =
            h.w.browsers
                .get(&id)
                .unwrap()
                .refresh
                .as_ref()
                .unwrap()
                .1
                .clone();
        assert!(!second_flag.is_cancelled());
        assert_eq!(h.seen.watches.len(), 2);
        // The old watcher's end must not clear the new refresh.
        let (conn, generation, node, old_run, _) = h.seen.watches.remove(0);
        h.w.refresh_event(
            &conn,
            generation,
            node,
            old_run,
            RefreshEvent::Finished(RefreshOutcome::Cancelled),
        );
        assert!(h.w.browsers.get(&id).unwrap().refresh.is_some());
        // The cancel command stops the current one, and its end clears the state.
        let effect = h.w.dispatch_browser(BrowserAction::CancelRefresh);
        h.apply(effect);
        assert!(second_flag.is_cancelled());
        let new_run = h.w.browsers.get(&id).unwrap().refresh.as_ref().unwrap().2;
        assert_ne!(old_run, new_run);
        // A late Applied of the replaced refresh does not clear the new one either.
        h.w.refresh_event(
            &conn,
            generation,
            node,
            old_run,
            RefreshEvent::Finished(RefreshOutcome::Applied {
                refreshed: 0,
                failed: 0,
            }),
        );
        assert!(h.w.browsers.get(&id).unwrap().refresh.is_some());
        h.w.refresh_event(
            &conn,
            generation,
            node,
            new_run,
            RefreshEvent::Finished(RefreshOutcome::Cancelled),
        );
        assert!(h.w.browsers.get(&id).unwrap().refresh.is_none());
        assert!(matches!(
            h.w.dispatch_browser(BrowserAction::CancelRefresh),
            Effect::None
        ));
        assert!(h.w.take_notice().unwrap().contains("No refresh"));
    }

    #[test]
    fn editing_or_deleting_the_connection_shuts_the_catalog_down() {
        for delete in [false, true] {
            let mut h = Harness::new(Environment::Dev, intro());
            h.open();
            let id = conn_id(&h.w);
            let catalog = h.w.catalog(&id, 1).expect("catalog");
            if delete {
                h.w.dispatch_connection(crate::connections::ConnectionAction::Delete(id.clone()));
                h.w.dispatch_connection(crate::connections::ConnectionAction::ConfirmDelete);
            } else {
                h.w.dispatch_connection(crate::connections::ConnectionAction::Edit(id.clone()));
                let effect =
                    h.w.dispatch_connection(crate::connections::ConnectionAction::Save);
                assert!(matches!(effect, Effect::SaveConnections { .. }));
            }
            assert!(h.w.browser(&id).is_none());
            // The worker has stopped: loads fail with `Closed`.
            let run = h.runs.remove(0);
            h.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), run)
                    .await
                    .unwrap()
                    .unwrap();
            });
            let err =
                h.rt.block_on(
                    catalog.children(NodeKey::new(main_schema().child_path(), ObjectKind::View)),
                )
                .unwrap_err();
            assert_eq!(err, CatalogError::Closed);
            // The browser session was closed with the worker.
            assert_eq!(h.connector.closed(), 1);
        }
    }

    #[test]
    fn quit_shuts_the_catalogs_down_and_closes_everything() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let id = conn_id(&h.w);
        let catalog = h.w.catalog(&id, 1).expect("catalog");
        let Effect::CloseSessions(sessions) = h.w.dispatch(WorkspaceAction::Quit) else {
            panic!("expected CloseSessions");
        };
        assert_eq!(sessions.len(), 1);
        assert!(h.w.browser(&id).is_none());
        let run = h.runs.remove(0);
        h.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), run)
                .await
                .unwrap()
                .unwrap();
        });
        assert_eq!(h.connector.closed(), 1);
        drop(catalog);
    }

    #[test]
    fn quit_with_only_a_browser_still_closes_it() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let id = conn_id(&h.w);
        // The only tab goes away; the browser stays.
        let tab = h.w.tabs().active().unwrap().id();
        let effect = h.w.dispatch(WorkspaceAction::CloseTab(tab));
        h.apply(effect);
        assert!(h.w.browser(&id).is_some());
        assert!(matches!(
            h.w.dispatch(WorkspaceAction::Quit),
            Effect::CloseSessions(s) if s.is_empty()
        ));
        // Without any browser, quitting is immediate.
        assert!(matches!(h.w.dispatch(WorkspaceAction::Quit), Effect::Quit));
    }

    #[test]
    fn keyboard_selection_walks_the_whole_sidebar() {
        let mut h = Harness::new(Environment::Dev, intro());
        h.open();
        let id = conn_id(&h.w);
        h.w.dispatch(WorkspaceAction::SelectConnection(0));
        let selected = |h: &Harness| {
            let tree = h.w.browser(&id).unwrap();
            tree.selected()
                .and_then(|n| tree.rows().iter().find(|r| r.id == n))
                .map(|r| r.label.clone())
        };
        assert_eq!(selected(&h), None, "the connection row itself");
        h.w.dispatch_browser(BrowserAction::SelectNext);
        assert_eq!(selected(&h).as_deref(), Some("Schemas (1)"));
        h.w.dispatch_browser(BrowserAction::SelectNext);
        assert_eq!(
            selected(&h).as_deref(),
            Some("Schemas (1)"),
            "last row stays"
        );
        h.w.dispatch_browser(BrowserAction::SelectPrevious);
        assert_eq!(selected(&h), None);
        assert_eq!(h.w.sidebar().selected(), Some(0));
        // Right expands, Left collapses.
        h.w.dispatch_browser(BrowserAction::SelectNext);
        let effect = h.w.dispatch_browser(BrowserAction::Expand);
        h.apply(effect);
        assert_eq!(h.labels(), ["Schemas (1)", "  main"]);
        h.w.dispatch_browser(BrowserAction::Collapse);
        assert_eq!(h.labels(), ["Schemas (1)"]);
    }

    #[test]
    fn show_menu_is_an_effect_for_the_view() {
        let mut w = workspace(Environment::Dev);
        assert!(matches!(
            w.dispatch_browser(BrowserAction::ShowMenu),
            Effect::ShowObjectMenu
        ));
    }

    #[test]
    fn sidebar_items_follow_the_tree() {
        let mut h = Harness::new(Environment::Dev, intro());
        assert_eq!(h.w.sidebar_items().len(), 1);
        h.open();
        let items = h.w.sidebar_items();
        assert_eq!(items.len(), 2);
        assert!(matches!(items[1], SidebarItem::Object { row_index: 0, .. }));
        let id = conn_id(&h.w);
        let root = h.w.browser(&id).unwrap().root();
        h.w.dispatch_object(&id, root, BrowserAction::Collapse);
        assert_eq!(h.w.sidebar_items().len(), 1);
        assert_eq!(h.w.browser_expanded(&id), Some(false));
        let entries = h.w.menu_entries(&id, root);
        assert_eq!(entries, [MenuEntry::Refresh]);
    }

    #[test]
    fn debug_of_effects_and_jobs_hides_secrets() {
        let job = BrowserJob::Drop {
            conn: conn_id(&workspace(Environment::Dev)),
            request: request(),
            sql: "DROP TABLE t;".into(),
        };
        let text = format!("{job:?}");
        assert!(text.contains("DROP TABLE t;"));
        assert!(!text.contains("request"));
    }

    #[test]
    fn session_trait_object_is_used_for_browse() {
        // The browse connect hands a session to `Catalog::new`; it must be introspectable.
        let rt = runtime();
        let connector = SessionConnector::new(intro());
        let mut session: Box<dyn Session> = rt
            .block_on(Connector::connect(&connector, request()))
            .unwrap();
        assert!(session.introspection().is_some());
    }
}
