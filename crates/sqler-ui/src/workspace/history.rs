// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Query history and saved queries inside the workspace.
//!
//! Like the query lifecycle this is state only. Every call returns an [`Effect`] when the store
//! or `queries.toml` must be touched; the view does that on a blocking thread and reports back
//! through the `history_*` and `saved_queries_*` methods. Re-running an entry goes through
//! [`Workspace::run_query`], so the production confirmation of T8 still applies.

use std::collections::HashMap;
use std::time::SystemTime;

use sqler_config::{ConnectionId, HistoryEntry, SavedQueries, SavedQueryDraft, SavedQueryId};

use super::state::{Effect, RequestId, Workspace};
use crate::editor::EditorBuffer;
use crate::history::{
    HistoryCommand, HistoryDialog, HistoryRow, HistoryScope, HistoryTab, NamePurpose, SavedLoad,
    SavedPicker, SavedState, history_entry, history_row,
};
use crate::query::{BufferSnapshot, RunEvent, RunMode};
use crate::tabs::{Tab, TabId, TabKind};
use crate::workbench::PanelView;

/// What to do with the editor tab that a connect attempt opens.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum PendingOpen {
    /// Run this SQL once the tab is open.
    Run(String),
    /// The tab shows this saved query.
    Saved(SavedQueryId),
}

// Debug never prints the SQL.
impl std::fmt::Debug for PendingOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(sql) => f.debug_struct("Run").field("sql_len", &sql.len()).finish(),
            Self::Saved(id) => f.debug_tuple("Saved").field(id).finish(),
        }
    }
}

/// The history state of a workspace.
pub(super) struct HistoryWorkspace {
    /// The history tabs.
    tabs: HashMap<TabId, HistoryTab>,
    /// When the statement that runs in a query tab started.
    started: HashMap<TabId, SystemTime>,
    /// The saved queries.
    saved: SavedState,
    /// The name or confirmation dialog on screen.
    dialog: Option<HistoryDialog>,
    /// The "open saved query" picker on screen.
    picker: Option<SavedPicker>,
    /// The saved query an editor tab was opened from or saved as.
    tab_saved: HashMap<TabId, SavedQueryId>,
    /// What to do once the connect attempt of a request has opened its tab.
    pending: HashMap<RequestId, PendingOpen>,
    /// The last search request number handed out.
    next_request: u64,
    /// A failed write was already reported; the next one stays quiet until one succeeds.
    record_failed: bool,
}

impl HistoryWorkspace {
    pub(super) fn new() -> Self {
        Self {
            tabs: HashMap::new(),
            started: HashMap::new(),
            saved: SavedState::new(),
            dialog: None,
            picker: None,
            tab_saved: HashMap::new(),
            pending: HashMap::new(),
            next_request: 0,
            record_failed: false,
        }
    }

    /// Forgets everything about a closed tab.
    pub(super) fn forget_tab(&mut self, id: TabId) {
        self.tabs.remove(&id);
        self.started.remove(&id);
        self.tab_saved.remove(&id);
    }

    /// Takes what is waiting for the tab that the connect attempt of `request` opens.
    pub(super) fn take_pending(&mut self, request: RequestId) -> Option<PendingOpen> {
        self.pending.remove(&request)
    }
}

impl Workspace {
    fn history_notice(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    // ---- recording ----

    /// Notes one event of an accepted run: the start time of a statement, and the history entry
    /// of a statement that ended.
    pub(super) fn record_event(&mut self, tab: TabId, event: &RunEvent) -> Effect {
        let now = SystemTime::now();
        let index = match event {
            RunEvent::Started { .. } => {
                self.history.started.insert(tab, now);
                return Effect::None;
            }
            // A lookup after a statement, not a statement of its own.
            RunEvent::Resolved { .. } => return Effect::None,
            RunEvent::Finished { index, .. }
            | RunEvent::Failed { index, .. }
            | RunEvent::Cancelled { index } => *index,
        };
        let started = self.history.started.remove(&tab);
        let sql = self
            .queries
            .get(&tab)
            .and_then(|q| q.results().iter().find(|r| r.index == index))
            .map(|r| r.sql.as_str());
        let (Some(sql), Some(connection)) = (sql, self.tab_conns.get(&tab)) else {
            return Effect::None;
        };
        match history_entry(connection, sql, event, started, now) {
            Some(entry) => Effect::RecordHistory(entry),
            None => Effect::None,
        }
    }

    // ---- the history tab ----

    /// Runs a history or saved-query command. `buffer` is a copy of the active editor's text
    /// (`None` when the view has no editor on screen).
    pub fn dispatch_history(
        &mut self,
        command: HistoryCommand,
        buffer: Option<EditorBuffer>,
    ) -> Effect {
        match command {
            HistoryCommand::OpenHistory => self.open_history_panel(),
            HistoryCommand::OpenEntry => match self.selected_entry() {
                Some((tab, index)) => self.open_history_entry(tab, index),
                None => self.history_notice("Select a history entry first."),
            },
            HistoryCommand::RunEntry => match self.selected_entry() {
                Some((tab, index)) => self.run_history_entry(tab, index),
                None => self.history_notice("Select a history entry first."),
            },
            HistoryCommand::ToggleAllConnections => match self.active_history_tab() {
                Some(tab) => {
                    let scope = match self.history.tabs.get(&tab).map(HistoryTab::scope) {
                        Some(HistoryScope::Connection) => HistoryScope::All,
                        _ => HistoryScope::Connection,
                    };
                    self.set_history_scope(tab, scope)
                }
                None => self.history_notice("Open a history tab first."),
            },
            HistoryCommand::ClearHistory => {
                if self.history_dialog_allowed() {
                    self.history.picker = None;
                    self.history.dialog = Some(HistoryDialog::ConfirmClear);
                }
                Effect::None
            }
            HistoryCommand::SaveQuery => self.start_save(buffer),
            HistoryCommand::OpenSaved => {
                if self.saved_ready() && self.history_dialog_allowed() {
                    self.history.dialog = None;
                    self.history.picker = Some(SavedPicker::new());
                }
                Effect::None
            }
            HistoryCommand::RenameSaved => self.start_rename(),
            HistoryCommand::DeleteSaved => self.start_delete(),
        }
    }

    /// The history the commands act on: the panel's while it is visible on History, else the
    /// active strip tab if it is a history tab.
    fn active_history_tab(&self) -> Option<TabId> {
        if self.layout.panel_visible()
            && self.layout.panel_view() == PanelView::History
            && let Some(panel) = self.panel_history()
        {
            return Some(panel);
        }
        self.tabs
            .active()
            .map(Tab::id)
            .filter(|tab| self.history.tabs.contains_key(tab))
    }

    /// The active history tab and its selected entry.
    fn selected_entry(&self) -> Option<(TabId, usize)> {
        let tab = self.active_history_tab()?;
        Some((tab, self.history.tabs.get(&tab)?.selected()?))
    }

    /// The state of a history tab.
    #[must_use]
    pub fn history_tab(&self, tab: TabId) -> Option<&HistoryTab> {
        self.history.tabs.get(&tab)
    }

    /// The label of a connection: its name, or the raw id when it was deleted.
    fn connection_label(&self, id: &ConnectionId) -> String {
        self.sidebar.index_of(id).map_or_else(
            || id.to_string(),
            |index| self.sidebar.entries()[index].config().name.clone(),
        )
    }

    /// The display rows of a history tab, newest first.
    #[must_use]
    pub fn history_rows(&self, tab: TabId, now: SystemTime) -> Vec<HistoryRow> {
        let Some(history) = self.history.tabs.get(&tab) else {
            return Vec::new();
        };
        history
            .results()
            .iter()
            .map(|entry| history_row(entry, &self.connection_label(entry.connection()), now))
            .collect()
    }

    fn search_history(&mut self, tab: TabId) -> Effect {
        self.history.next_request += 1;
        let request = self.history.next_request;
        match self.history.tabs.get_mut(&tab) {
            Some(history) => Effect::SearchHistory(history.begin_search(tab, request)),
            None => Effect::None,
        }
    }

    /// Shows History in the bottom panel, bound to the active tab's connection (else the selected
    /// one). The panel keeps its state under [`TabId::PANEL_HISTORY`] and starts over when the
    /// connection differs. No strip tab opens.
    fn open_history_panel(&mut self) -> Effect {
        let conn = self
            .tabs
            .active()
            .and_then(|tab| self.tab_conns.get(&tab.id()))
            .or_else(|| self.sidebar.selected_id())
            .cloned();
        let Some(conn) = conn else {
            return self.history_notice("Select a connection in the sidebar first.");
        };
        if self.sidebar.index_of(&conn).is_none() {
            return self.history_notice("The connection no longer exists.");
        }
        let bound = self
            .history
            .tabs
            .get(&TabId::PANEL_HISTORY)
            .is_some_and(|history| *history.connection() == conn);
        if !bound {
            self.history
                .tabs
                .insert(TabId::PANEL_HISTORY, HistoryTab::new(conn));
        }
        self.layout.reveal_panel(PanelView::History);
        self.search_history(TabId::PANEL_HISTORY)
    }

    /// The panel's History state, if the panel has been bound to a connection.
    #[must_use]
    pub fn panel_history(&self) -> Option<TabId> {
        self.history
            .tabs
            .contains_key(&TabId::PANEL_HISTORY)
            .then_some(TabId::PANEL_HISTORY)
    }

    /// Sets the search text of a history tab and searches again when it changed.
    pub fn set_history_text(&mut self, tab: TabId, text: String) -> Effect {
        let changed = self
            .history
            .tabs
            .get_mut(&tab)
            .is_some_and(|history| history.set_text(text));
        if changed {
            self.search_history(tab)
        } else {
            Effect::None
        }
    }

    /// Switches a history tab between its connection and all connections, and searches again.
    pub fn set_history_scope(&mut self, tab: TabId, scope: HistoryScope) -> Effect {
        let changed = self
            .history
            .tabs
            .get_mut(&tab)
            .is_some_and(|history| history.set_scope(scope));
        if changed {
            self.search_history(tab)
        } else {
            Effect::None
        }
    }

    /// Selects entry `index` of a history tab.
    pub fn select_history_entry(&mut self, tab: TabId, index: usize) {
        if let Some(history) = self.history.tabs.get_mut(&tab) {
            history.select(index);
        }
    }

    /// The entry's connection and SQL.
    fn history_entry_at(&self, tab: TabId, index: usize) -> Option<&HistoryEntry> {
        self.history.tabs.get(&tab)?.results().get(index)
    }

    /// Opens entry `index` in a new editor tab on the entry's connection.
    pub fn open_history_entry(&mut self, tab: TabId, index: usize) -> Effect {
        let Some(entry) = self.history_entry_at(tab, index) else {
            return Effect::None;
        };
        let redacted = entry.redacted();
        let (conn, sql) = (entry.connection().clone(), entry.sql().to_owned());
        let effect = self.open_editor_on(&conn, Some(sql));
        if redacted {
            self.notice = Some(
                "Sensitive values were redacted from this entry; enter them again before running it."
                    .to_owned(),
            );
        }
        effect
    }

    /// Opens entry `index` in a new editor tab and runs it there once the connection is up. The
    /// run is a normal run: a production connection asks for its confirmation first.
    pub fn run_history_entry(&mut self, tab: TabId, index: usize) -> Effect {
        let Some(entry) = self.history_entry_at(tab, index) else {
            return Effect::None;
        };
        if entry.truncated() {
            return self.history_notice(
                "This entry was cut when it was recorded; open it in an editor instead.",
            );
        }
        if entry.redacted() {
            return self.history_notice(
                "Run again is not available: sensitive values were redacted from this entry.",
            );
        }
        let (conn, sql) = (entry.connection().clone(), entry.sql().to_owned());
        let effect = self.open_editor_on(&conn, Some(sql.clone()));
        if let Effect::Connect { request, .. } = &effect {
            self.history.pending.insert(*request, PendingOpen::Run(sql));
        }
        effect
    }

    /// Carries out what was waiting for the editor tab that a connect attempt opened.
    pub(super) fn after_history_open(
        &mut self,
        before: Option<TabId>,
        pending: Option<PendingOpen>,
        effect: Effect,
    ) -> Effect {
        let Some(pending) = pending else {
            return effect;
        };
        let opened = self
            .tabs
            .active()
            .filter(|tab| Some(tab.id()) != before && tab.kind() == TabKind::Query)
            .map(Tab::id);
        let Some(tab) = opened else {
            return effect;
        };
        match pending {
            PendingOpen::Saved(id) => {
                self.history.tab_saved.insert(tab, id);
                effect
            }
            PendingOpen::Run(sql) => {
                let run = self.run_query(tab, RunMode::All, BufferSnapshot::at_cursor(sql, 0));
                match effect {
                    Effect::None => run,
                    other => Effect::Batch(vec![other, run]),
                }
            }
        }
    }

    /// The entries of search `request` of a history tab; an answer to an older search is ignored.
    pub fn history_results(&mut self, tab: TabId, request: u64, results: Vec<HistoryEntry>) {
        if let Some(history) = self.history.tabs.get_mut(&tab) {
            history.accept(request, results);
        }
    }

    /// The tabs' searches again, in tab order.
    fn research_all(&mut self) -> Effect {
        let mut tabs: Vec<TabId> = self.history.tabs.keys().copied().collect();
        tabs.sort();
        let effects: Vec<Effect> = tabs.into_iter().map(|t| self.search_history(t)).collect();
        if effects.is_empty() {
            Effect::None
        } else {
            Effect::Batch(effects)
        }
    }

    /// A statement was recorded (or not). Open history tabs search again; a failure shows a
    /// notice, but only the first of a series.
    pub fn history_recorded(&mut self, result: Result<(), String>) -> Effect {
        match result {
            Ok(()) => {
                self.history.record_failed = false;
                self.research_all()
            }
            Err(err) => {
                if !self.history.record_failed {
                    self.history.record_failed = true;
                    self.notice = Some(format!("Could not record the query history: {err}"));
                }
                Effect::None
            }
        }
    }

    /// The history was deleted (or not).
    pub fn history_cleared(&mut self, result: Result<(), String>) -> Effect {
        match result {
            Ok(()) => {
                for history in self.history.tabs.values_mut() {
                    history.forget_results();
                }
                self.notice = Some("Query history cleared.".to_owned());
                self.research_all()
            }
            Err(err) => self.history_notice(format!("Could not clear the query history: {err}")),
        }
    }

    // ---- saved queries ----

    /// The saved queries and their load state.
    #[must_use]
    pub fn saved(&self) -> &SavedState {
        &self.history.saved
    }

    /// `queries.toml` was read (or not).
    pub fn saved_queries_loaded(&mut self, result: Result<SavedQueries, String>) {
        match result {
            Ok(queries) => {
                self.history.saved.replace(queries);
                self.history.saved.set_load(SavedLoad::Ready);
            }
            Err(err) => self.history.saved.set_load(SavedLoad::Failed(err)),
        }
    }

    /// A write of generation `generation` ended; a failure shows a notice.
    pub fn saved_queries_written(&mut self, generation: u64, result: Result<(), String>) {
        if let Err(err) = result {
            self.notice = Some(format!(
                "Could not save the saved queries (change {generation}): {err}"
            ));
        }
    }

    /// Whether the saved queries may be changed; otherwise says why in a notice.
    fn saved_ready(&mut self) -> bool {
        match &self.history.saved.load() {
            SavedLoad::Ready => true,
            SavedLoad::Loading => {
                self.notice = Some("Saved queries are still loading.".to_owned());
                false
            }
            SavedLoad::Failed(err) => {
                self.notice = Some(format!("Saved queries are not available: {err}"));
                false
            }
        }
    }

    /// Whether a history dialog may open; the dialogs of the connections win.
    fn history_dialog_allowed(&mut self) -> bool {
        if self.dialog.is_some() {
            self.notice = Some("Close the open dialog first.".to_owned());
            return false;
        }
        true
    }

    /// Counts a change of the saved queries and asks for the write.
    fn commit_saved(&mut self) -> Effect {
        let generation = self.history.saved.bump();
        Effect::SaveSavedQueries {
            generation,
            queries: self.history.saved.queries().clone(),
        }
    }

    /// Selects a saved query.
    pub fn select_saved(&mut self, id: &SavedQueryId) {
        if self.history.saved.queries().get(id).is_some() {
            self.history.saved.select(Some(id.clone()));
        }
    }

    /// The selected saved query, if it still exists; otherwise says so in a notice.
    fn selected_saved(&mut self) -> Option<SavedQueryId> {
        let id = self
            .history
            .saved
            .selected()
            .filter(|id| self.history.saved.queries().get(id).is_some())
            .cloned();
        if id.is_none() {
            self.notice = Some("Select a saved query first.".to_owned());
        }
        id
    }

    fn start_save(&mut self, buffer: Option<EditorBuffer>) -> Effect {
        if !self.saved_ready() {
            return Effect::None;
        }
        let tab = self
            .tabs
            .active()
            .map(Tab::id)
            .filter(|tab| self.editors.contains(*tab));
        let (Some(tab), Some(buffer)) = (tab, buffer) else {
            return self.history_notice("Open a SQL editor first.");
        };
        if buffer.text.trim().is_empty() {
            return self.history_notice("Nothing to save.");
        }
        if !self.history_dialog_allowed() {
            return Effect::None;
        }
        let replaces = self
            .history
            .tab_saved
            .get(&tab)
            .filter(|id| self.history.saved.queries().get(id).is_some())
            .cloned();
        let name = replaces
            .as_ref()
            .and_then(|id| self.history.saved.queries().get(id))
            .map(|q| q.name().to_owned())
            .unwrap_or_default();
        self.history.picker = None;
        self.history.dialog = Some(HistoryDialog::Name {
            purpose: NamePurpose::Save {
                tab,
                sql: buffer.text,
                connection: self.tab_conns.get(&tab).cloned(),
                replaces,
            },
            text: name,
            error: None,
        });
        Effect::None
    }

    fn start_rename(&mut self) -> Effect {
        if !self.saved_ready() {
            return Effect::None;
        }
        let Some(id) = self.selected_saved() else {
            return Effect::None;
        };
        if !self.history_dialog_allowed() {
            return Effect::None;
        }
        let name = self
            .history
            .saved
            .queries()
            .get(&id)
            .map(|q| q.name().to_owned())
            .unwrap_or_default();
        self.history.picker = None;
        self.history.dialog = Some(HistoryDialog::Name {
            purpose: NamePurpose::Rename(id),
            text: name,
            error: None,
        });
        Effect::None
    }

    fn start_delete(&mut self) -> Effect {
        if !self.saved_ready() {
            return Effect::None;
        }
        let Some(id) = self.selected_saved() else {
            return Effect::None;
        };
        if !self.history_dialog_allowed() {
            return Effect::None;
        }
        let name = self
            .history
            .saved
            .queries()
            .get(&id)
            .map(|q| q.name().to_owned())
            .unwrap_or_default();
        self.history.picker = None;
        self.history.dialog = Some(HistoryDialog::ConfirmDelete { id, name });
        Effect::None
    }

    /// The history dialog on screen, if any.
    #[must_use]
    pub fn history_dialog(&self) -> Option<&HistoryDialog> {
        self.history.dialog.as_ref()
    }

    /// Sets the text of the open name dialog and clears its error.
    pub fn set_history_dialog_text(&mut self, text: String) {
        if let Some(HistoryDialog::Name {
            text: current,
            error,
            ..
        }) = &mut self.history.dialog
        {
            *current = text;
            *error = None;
        }
    }

    /// Answers the open dialog with "OK". A name the saved queries reject keeps the dialog open
    /// and shows the reason in it.
    pub fn submit_history_dialog(&mut self) -> Effect {
        match self.history.dialog.take() {
            None => Effect::None,
            Some(HistoryDialog::ConfirmClear) => Effect::ClearHistory,
            Some(HistoryDialog::ConfirmDelete { id, .. }) => self.delete_saved(&id),
            Some(HistoryDialog::Name {
                purpose,
                text,
                error: _,
            }) => match self.apply_name(&purpose, &text) {
                Ok(()) => self.commit_saved(),
                Err(err) => {
                    self.history.dialog = Some(HistoryDialog::Name {
                        purpose,
                        text,
                        error: Some(err),
                    });
                    Effect::None
                }
            },
        }
    }

    /// Saves or renames with `text`; the reason it was rejected.
    fn apply_name(&mut self, purpose: &NamePurpose, text: &str) -> Result<(), String> {
        let name = text.trim();
        let queries = self.history.saved.queries_mut();
        match purpose {
            NamePurpose::Save {
                tab,
                sql,
                connection,
                replaces,
            } => {
                let same = replaces
                    .as_ref()
                    .filter(|id| queries.get(id).is_some_and(|q| q.name() == name));
                if let Some(id) = same {
                    queries
                        .set_sql(id, sql.clone())
                        .map_err(|e| e.to_string())?;
                    self.mark_editor_clean(*tab);
                    return Ok(());
                }
                let draft = SavedQueryDraft {
                    connection: connection.clone(),
                    ..SavedQueryDraft::new(name, sql.clone())
                };
                let id = queries.create(draft).map_err(|e| e.to_string())?;
                self.history.tab_saved.insert(*tab, id);
                self.mark_editor_clean(*tab);
                Ok(())
            }
            NamePurpose::Rename(id) => queries.rename(id, name).map_err(|e| e.to_string()),
        }
    }

    /// The text of the tab is saved under a name: it is no longer dirty.
    fn mark_editor_clean(&mut self, tab: TabId) {
        if let Some(doc) = self.editors.get_mut(tab) {
            doc.mark_clean();
        }
    }

    fn delete_saved(&mut self, id: &SavedQueryId) -> Effect {
        if let Err(err) = self.history.saved.queries_mut().delete(id) {
            return self.history_notice(err.to_string());
        }
        if self.history.saved.selected() == Some(id) {
            self.history.saved.select(None);
        }
        self.history.tab_saved.retain(|_, saved| saved != id);
        self.commit_saved()
    }

    /// Closes the open dialog; nothing changes.
    pub fn cancel_history_dialog(&mut self) {
        self.history.dialog = None;
    }

    /// Opens saved query `id` in a new editor tab: on its connection, else the active tab's, else
    /// the selected one.
    pub fn open_saved(&mut self, id: &SavedQueryId) -> Effect {
        let Some(query) = self.history.saved.queries().get(id) else {
            return Effect::None;
        };
        let sql = query.sql().to_owned();
        let bound = query
            .connection()
            .filter(|conn| self.sidebar.index_of(conn).is_some())
            .cloned();
        let conn = bound
            .or_else(|| {
                self.tabs
                    .active()
                    .and_then(|tab| self.tab_conns.get(&tab.id()))
                    .cloned()
            })
            .or_else(|| self.sidebar.selected_id().cloned());
        let Some(conn) = conn else {
            return self.history_notice("Select a connection in the sidebar first.");
        };
        let effect = self.open_editor_on(&conn, Some(sql));
        if let Effect::Connect { request, .. } = &effect {
            self.history
                .pending
                .insert(*request, PendingOpen::Saved(id.clone()));
        }
        effect
    }

    // ---- the picker ----

    /// The "open saved query" picker on screen, if any.
    #[must_use]
    pub fn saved_picker(&self) -> Option<&SavedPicker> {
        self.history.picker.as_ref()
    }

    /// Sets the filter of the picker; the first match is highlighted.
    pub fn set_picker_filter(&mut self, text: String) {
        if let Some(picker) = &mut self.history.picker {
            picker.set_filter(text);
        }
    }

    /// Moves the highlight of the picker by `delta` matches.
    pub fn move_picker(&mut self, delta: isize) {
        if let Some(picker) = &mut self.history.picker {
            let count = picker.matches(self.history.saved.queries()).len();
            picker.step(delta, count);
        }
    }

    /// Opens the highlighted saved query and closes the picker.
    pub fn confirm_picker(&mut self) -> Effect {
        let Some(picker) = self.history.picker.take() else {
            return Effect::None;
        };
        let id = picker
            .current(self.history.saved.queries())
            .map(|q| q.id().clone());
        if let Some(id) = id {
            self.open_saved(&id)
        } else {
            self.history.picker = Some(picker);
            Effect::None
        }
    }

    /// Closes the picker.
    pub fn close_picker(&mut self) {
        self.history.picker = None;
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use sqler_config::{ConfigDir, HistoryOptions, HistoryOutcome};
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Environment};

    use super::*;
    use crate::history::{HISTORY_PAGE, HistoryService};
    use crate::query::{Phase, QueryJob, RunId};
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::testing::FakeSession;

    fn ws() -> Workspace {
        let driver = DriverId::new("sqlite").unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver.clone(), ":memory:")
                .with_environment(Environment::Dev),
            ConnectionConfig::new("prod", driver, ":memory:").with_environment(Environment::Prod),
        ]))
    }

    fn conn(w: &Workspace, index: usize) -> ConnectionId {
        w.sidebar().entries()[index].id().clone()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sqler-wh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn service(dir: &std::path::Path) -> HistoryService {
        HistoryService::new(Some(ConfigDir::new(dir)), HistoryOptions::default())
    }

    fn connect(w: &mut Workspace, effect: &Effect) -> TabId {
        let Effect::Connect { request, .. } = effect else {
            panic!("expected Connect, got {effect:?}");
        };
        let (session, _) = FakeSession::boxed();
        w.connect_finished(*request, Ok(session));
        w.tabs().active().map(Tab::id).unwrap()
    }

    /// Opens an editor tab on connection `index`.
    fn open(w: &mut Workspace, index: usize) -> TabId {
        let effect = w.dispatch(WorkspaceAction::OpenConnection(index));
        connect(w, &effect)
    }

    fn buf(text: &str) -> BufferSnapshot {
        BufferSnapshot::at_cursor(text, 0)
    }

    fn start(w: &mut Workspace, tab: TabId, text: &str) -> (QueryJob, Vec<Effect>) {
        match w.run_query(tab, RunMode::All, buf(text)) {
            Effect::RunQuery { job, .. } => (job, Vec::new()),
            other => panic!("expected RunQuery, got {other:?}"),
        }
    }

    /// Does what the view does with the history effects, on the service, in place.
    fn apply(w: &mut Workspace, svc: &HistoryService, effect: Effect) {
        match effect {
            Effect::Batch(effects) => {
                for effect in effects {
                    apply(w, svc, effect);
                }
            }
            Effect::RecordHistory(entry) => {
                let next = w.history_recorded(svc.record(entry));
                apply(w, svc, next);
            }
            Effect::SearchHistory(search) => {
                w.history_results(search.tab, search.request, svc.search(&search));
            }
            Effect::ClearHistory => {
                let next = w.history_cleared(svc.clear());
                apply(w, svc, next);
            }
            Effect::SaveSavedQueries {
                generation,
                queries,
            } => w.saved_queries_written(generation, svc.save_saved(generation, &queries)),
            Effect::None => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    fn ended(duration_ms: u64, rows: usize) -> RunEvent {
        RunEvent::Finished {
            index: 0,
            rows,
            rows_affected: None,
            duration: Duration::from_millis(duration_ms),
        }
    }

    fn recorded(effect: Effect) -> sqler_config::NewHistoryEntry {
        match effect {
            Effect::RecordHistory(entry) => entry,
            other => panic!("expected RecordHistory, got {other:?}"),
        }
    }

    /// A workspace with saved queries loaded (empty).
    fn ready() -> Workspace {
        let mut w = ws();
        w.saved_queries_loaded(Ok(SavedQueries::new()));
        w
    }

    #[allow(clippy::unnecessary_wraps)] // the commands take an `Option`
    fn editor(text: &str) -> Option<EditorBuffer> {
        Some(EditorBuffer::new(text, 0))
    }

    fn dialog_text(w: &mut Workspace, text: &str) {
        w.set_history_dialog_text(text.to_owned());
    }

    fn names(w: &Workspace) -> Vec<String> {
        w.saved()
            .queries()
            .queries()
            .iter()
            .map(|q| q.name().to_owned())
            .collect()
    }

    // ---- recording ----

    #[test]
    fn a_failed_run_records_an_error_without_the_message() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let Effect::RunQuery {
            job, mut events, ..
        } = w.run_query(tab, RunMode::All, buf("select * from nope"))
        else {
            panic!("expected RunQuery");
        };
        let run = job.run;
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(job.run());
        let mut entries = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Effect::RecordHistory(entry) = w.query_event(tab, run, event) {
                entries.push(entry);
            }
        }
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.outcome, HistoryOutcome::Error);
        assert_eq!(entry.sql, "select * from nope");
        assert_eq!(entry.rows, None);
        assert_eq!(entry.connection, conn(&w, 0));
    }

    #[test]
    fn finished_statements_record_duration_and_rows() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let (job, _) = start(&mut w, tab, "select 1");
        assert!(matches!(
            w.query_event(tab, job.run, RunEvent::Started { index: 0 }),
            Effect::None
        ));
        let entry = recorded(w.query_event(tab, job.run, ended(12, 4)));
        assert_eq!(entry.outcome, HistoryOutcome::Ok);
        assert_eq!(entry.duration, Duration::from_millis(12));
        assert_eq!(entry.rows, Some(4));
        assert_eq!(entry.sql, "select 1");
        assert!(entry.started_at <= SystemTime::now());
    }

    #[test]
    fn a_resolved_event_records_nothing_and_keeps_the_start_time() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let (job, _) = start(&mut w, tab, "select 1");
        w.query_event(tab, job.run, RunEvent::Started { index: 0 });
        let resolved = RunEvent::Resolved {
            index: 0,
            schema: Some("main".to_owned()),
        };
        assert!(matches!(w.record_event(tab, &resolved), Effect::None));
        let entry = recorded(w.query_event(tab, job.run, ended(12, 4)));
        assert_eq!(entry.outcome, HistoryOutcome::Ok);
        assert_eq!(entry.sql, "select 1");
    }

    #[test]
    fn a_script_records_one_entry_per_statement_and_stops_at_a_failure() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let (job, _) = start(&mut w, tab, "select 1; delete from t; select 3");
        let run = job.run;
        w.query_event(tab, run, RunEvent::Started { index: 0 });
        let first = recorded(w.query_event(tab, run, ended(1, 1)));
        assert_eq!(first.sql, "select 1");
        w.query_event(tab, run, RunEvent::Started { index: 1 });
        let second = recorded(w.query_event(
            tab,
            run,
            RunEvent::Finished {
                index: 1,
                rows: 0,
                rows_affected: Some(5),
                duration: Duration::from_millis(2),
            },
        ));
        assert_eq!(second.sql, "delete from t");
        assert_eq!(second.rows, Some(5));
        w.query_event(tab, run, RunEvent::Started { index: 2 });
        let third = recorded(w.query_event(tab, run, RunEvent::Cancelled { index: 2 }));
        assert_eq!(third.outcome, HistoryOutcome::Cancelled);
        assert_eq!(third.sql, "select 3");
        assert_eq!(third.rows, None);
    }

    #[test]
    fn a_bound_value_never_reaches_the_entry() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        assert!(matches!(
            w.run_query(tab, RunMode::All, buf("select :id")),
            Effect::None
        ));
        let Phase::Prompting(form) = w.query_tab(tab).unwrap().phase() else {
            panic!("expected a prompt");
        };
        let mut inputs = form.inputs();
        inputs[0].text = "424242".to_owned();
        let Effect::RunQuery { job, .. } = w.submit_query_params(tab, &inputs) else {
            panic!("expected RunQuery");
        };
        assert_ne!(job.statements[0].query.sql, "select :id");
        let entry = recorded(w.query_event(tab, job.run, ended(1, 1)));
        assert_eq!(entry.sql, "select :id");
        assert!(!format!("{entry:?}").contains("424242"));
    }

    #[test]
    fn stale_runs_record_nothing() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let (job, _) = start(&mut w, tab, "select 1");
        let stale = RunId::new(job.run.raw() + 100);
        assert!(matches!(
            w.query_event(tab, stale, ended(1, 1)),
            Effect::None
        ));
        // A tab that is gone records nothing either.
        w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(matches!(
            w.query_event(tab, job.run, ended(1, 1)),
            Effect::None
        ));
    }

    #[test]
    fn effect_debug_never_prints_sql() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let (job, _) = start(&mut w, tab, "select secret_column from t");
        let effect = w.query_event(tab, job.run, ended(1, 1));
        assert!(!format!("{effect:?}").contains("secret_column"));
        let history = w.dispatch_history(HistoryCommand::OpenHistory, None);
        assert!(format!("{history:?}").starts_with("SearchHistory"));
    }

    // ---- the history tab ----

    #[test]
    fn history_open_reveals_the_panel_and_rebinds_on_a_connection_change() {
        let mut w = ws();
        let editor_tab = open(&mut w, 0);
        assert!(w.panel_history().is_none());
        let Effect::SearchHistory(search) = w.dispatch_history(HistoryCommand::OpenHistory, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(search.tab, TabId::PANEL_HISTORY);
        assert_eq!(search.limit, HISTORY_PAGE);
        assert_eq!(search.connection, Some(conn(&w, 0)));
        assert_eq!(search.text, "");
        // The panel shows History; no strip tab opened and the editor stays active.
        assert!(w.layout().panel_visible());
        assert_eq!(w.layout().panel_view(), PanelView::History);
        assert_eq!(w.tabs().len(), 1);
        assert_eq!(w.tabs().active().map(Tab::id), Some(editor_tab));
        assert_eq!(w.panel_history(), Some(TabId::PANEL_HISTORY));
        let history = w.history_tab(TabId::PANEL_HISTORY).unwrap();
        assert_eq!(history.connection(), &conn(&w, 0));
        assert!(history.searching());

        // Again on the same connection: the same state, a newer search.
        let Effect::SearchHistory(again) = w.dispatch_history(HistoryCommand::OpenHistory, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(again.tab, TabId::PANEL_HISTORY);
        assert!(again.request > search.request);
        assert_eq!(w.tabs().len(), 1);

        // Another connection: the panel starts over bound to it.
        w.dispatch(WorkspaceAction::SelectConnection(1));
        let prod_tab = open(&mut w, 1);
        assert_eq!(w.tabs().active().map(Tab::id), Some(prod_tab));
        let Effect::SearchHistory(rebound) = w.dispatch_history(HistoryCommand::OpenHistory, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(rebound.tab, TabId::PANEL_HISTORY);
        assert_eq!(rebound.connection, Some(conn(&w, 1)));
        assert_eq!(
            w.history_tab(TabId::PANEL_HISTORY).unwrap().connection(),
            &conn(&w, 1)
        );
        assert_eq!(w.tabs().len(), 2);
    }

    #[test]
    fn history_binds_the_selected_connection_without_a_tab() {
        let mut w = ws();
        w.dispatch_history(HistoryCommand::OpenHistory, None);
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Select a connection in the sidebar first.")
        );
        assert!(w.tabs().is_empty());
        assert!(w.panel_history().is_none());
        w.dispatch(WorkspaceAction::SelectConnection(1));
        let Effect::SearchHistory(search) = w.dispatch_history(HistoryCommand::OpenHistory, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(search.connection, Some(conn(&w, 1)));
        assert_eq!(search.tab, TabId::PANEL_HISTORY);
        assert!(w.tabs().is_empty());
    }

    #[test]
    fn history_commands_act_on_the_panel_only_while_it_shows_history() {
        let dir = temp_dir("panel-visible");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1"]);
        assert_eq!(tab, TabId::PANEL_HISTORY);
        w.select_history_entry(tab, 0);
        // The panel switches to Results: the commands no longer see the history.
        w.layout.reveal_panel(PanelView::Results);
        assert!(matches!(
            w.dispatch_history(HistoryCommand::OpenEntry, None),
            Effect::None
        ));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Select a history entry first.")
        );
        w.layout.reveal_panel(PanelView::History);
        assert!(!matches!(
            w.dispatch_history(HistoryCommand::OpenEntry, None),
            Effect::None
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_text_and_scope_bump_the_request() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let Effect::SearchHistory(first) = w.dispatch_history(HistoryCommand::OpenHistory, None)
        else {
            panic!("expected SearchHistory");
        };
        let tab = first.tab;
        let Effect::SearchHistory(text) = w.set_history_text(tab, "select".to_owned()) else {
            panic!("expected SearchHistory");
        };
        assert_eq!(text.text, "select");
        assert!(text.request > first.request);
        assert!(matches!(
            w.set_history_text(tab, "select".to_owned()),
            Effect::None
        ));
        let Effect::SearchHistory(all) =
            w.dispatch_history(HistoryCommand::ToggleAllConnections, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(all.connection, None);
        assert_eq!(all.text, "select");
        assert_eq!(w.history_tab(tab).unwrap().scope(), HistoryScope::All);
        let Effect::SearchHistory(back) =
            w.dispatch_history(HistoryCommand::ToggleAllConnections, None)
        else {
            panic!("expected SearchHistory");
        };
        assert_eq!(back.connection, Some(conn(&w, 0)));
        assert!(matches!(
            w.set_history_scope(tab, HistoryScope::Connection),
            Effect::None
        ));
    }

    #[test]
    fn history_commands_without_a_history_tab_say_so() {
        let mut w = ws();
        open(&mut w, 0);
        for command in [HistoryCommand::OpenEntry, HistoryCommand::RunEntry] {
            assert!(matches!(w.dispatch_history(command, None), Effect::None));
            assert_eq!(
                w.take_notice().as_deref(),
                Some("Select a history entry first.")
            );
        }
        w.dispatch_history(HistoryCommand::ToggleAllConnections, None);
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Open a history tab first.")
        );
    }

    /// Records `statements` on `index` and opens its history tab with those entries.
    fn history_with(
        w: &mut Workspace,
        svc: &HistoryService,
        index: usize,
        statements: &[&str],
    ) -> TabId {
        let id = conn(w, index);
        for sql in statements {
            svc.record(sqler_config::NewHistoryEntry {
                sql: (*sql).to_owned(),
                connection: id.clone(),
                started_at: SystemTime::now(),
                duration: Duration::from_millis(5),
                outcome: HistoryOutcome::Ok,
                rows: Some(1),
            })
            .unwrap();
        }
        w.dispatch(WorkspaceAction::SelectConnection(index));
        let effect = w.dispatch_history(HistoryCommand::OpenHistory, None);
        let Effect::SearchHistory(search) = &effect else {
            panic!("expected SearchHistory");
        };
        let tab = search.tab;
        apply(w, svc, effect);
        tab
    }

    #[test]
    fn results_fill_the_tab_and_stale_ones_are_ignored() {
        let dir = temp_dir("results");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1", "select 2"]);
        let h = w.history_tab(tab).unwrap();
        assert!(!h.searching());
        assert_eq!(h.results().len(), 2);
        assert_eq!(h.results()[0].sql(), "select 2");
        let rows = w.history_rows(tab, SystemTime::now());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].sql, "select 2");
        assert_eq!(rows[0].connection, "dev");
        assert_eq!(rows[0].outcome, "OK");
        assert_eq!(rows[0].when, "just now");

        // A search for "1" is superseded by one for "2"; the late answer for "1" is dropped.
        let Effect::SearchHistory(one) = w.set_history_text(tab, "1".to_owned()) else {
            panic!("expected SearchHistory");
        };
        let Effect::SearchHistory(two) = w.set_history_text(tab, "2".to_owned()) else {
            panic!("expected SearchHistory");
        };
        w.history_results(tab, two.request, svc.search(&two));
        w.history_results(tab, one.request, svc.search(&one));
        let h = w.history_tab(tab).unwrap();
        assert_eq!(h.results().len(), 1);
        assert_eq!(h.results()[0].sql(), "select 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_connections_lists_every_connection_with_its_label() {
        let dir = temp_dir("all");
        let svc = service(&dir);
        let mut w = ws();
        history_with(&mut w, &svc, 1, &["select 'p'"]);
        let tab = history_with(&mut w, &svc, 0, &["select 'd'"]);
        assert_eq!(w.history_tab(tab).unwrap().results().len(), 1);
        let effect = w.dispatch_history(HistoryCommand::ToggleAllConnections, None);
        apply(&mut w, &svc, effect);
        let rows = w.history_rows(tab, SystemTime::now());
        let labels: Vec<_> = rows.iter().map(|r| r.connection.as_str()).collect();
        assert_eq!(labels, ["dev", "prod"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recording_refreshes_the_open_history_tabs() {
        let dir = temp_dir("refresh");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1"]);
        let editor_tab = open(&mut w, 0);
        let (job, _) = start(&mut w, editor_tab, "select 2");
        let effect = w.query_event(editor_tab, job.run, ended(1, 1));
        apply(&mut w, &svc, effect);
        let sqls: Vec<_> = w
            .history_tab(tab)
            .unwrap()
            .results()
            .iter()
            .map(HistoryEntry::sql)
            .collect();
        assert_eq!(sqls, ["select 2", "select 1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_first_record_failure_is_reported() {
        let mut w = ws();
        assert!(matches!(
            w.history_recorded(Err("disk full".to_owned())),
            Effect::None
        ));
        let notice = w.take_notice().unwrap();
        assert!(notice.contains("disk full"), "{notice}");
        w.history_recorded(Err("disk full".to_owned()));
        assert!(w.take_notice().is_none());
        w.history_recorded(Ok(()));
        w.history_recorded(Err("again".to_owned()));
        assert!(w.take_notice().unwrap().contains("again"));
    }

    #[test]
    fn clearing_empties_the_open_tabs() {
        let dir = temp_dir("clear");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1"]);
        assert!(matches!(
            w.dispatch_history(HistoryCommand::ClearHistory, None),
            Effect::None
        ));
        assert_eq!(w.history_dialog(), Some(&HistoryDialog::ConfirmClear));
        w.cancel_history_dialog();
        assert!(w.history_dialog().is_none());
        w.dispatch_history(HistoryCommand::ClearHistory, None);
        let effect = w.submit_history_dialog();
        assert!(matches!(effect, Effect::ClearHistory));
        apply(&mut w, &svc, effect);
        assert_eq!(w.history_tab(tab).unwrap().results().len(), 0);
        assert_eq!(service(&dir).search(&search_all()).len(), 0);
        assert_eq!(w.take_notice().as_deref(), Some("Query history cleared."));
        w.history_cleared(Err("locked".to_owned()));
        assert!(w.take_notice().unwrap().contains("locked"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tab id of another strip; only used where the id does not matter.
    fn foreign_tab() -> TabId {
        let connection = crate::tabs::TabConnection {
            name: "x".to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        crate::tabs::TabStrip::default().open(connection, TabKind::History)
    }

    fn search_all() -> crate::history::HistorySearch {
        crate::history::HistorySearch {
            tab: foreign_tab(),
            request: 1,
            text: String::new(),
            connection: None,
            limit: 100,
        }
    }

    // ---- open and run again ----

    #[test]
    fn opening_an_entry_connects_and_seeds_the_editor() {
        let dir = temp_dir("open");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1 from t"]);
        w.select_history_entry(tab, 0);
        w.select_history_entry(tab, 9);
        assert_eq!(w.history_tab(tab).unwrap().selected(), Some(0));
        let effect = w.dispatch_history(HistoryCommand::OpenEntry, None);
        let editor_tab = connect(&mut w, &effect);
        assert_eq!(w.tabs().get(editor_tab).unwrap().kind(), TabKind::Query);
        assert_eq!(
            w.take_editor_seed(editor_tab).as_deref(),
            Some("select 1 from t")
        );
        // Opening runs nothing.
        assert_eq!(*w.query_tab(editor_tab).unwrap().phase(), Phase::Idle);
        assert!(w.has_session(editor_tab));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_an_entry_again_runs_it_in_a_new_tab() {
        let dir = temp_dir("run");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1 from t"]);
        w.select_history_entry(tab, 0);
        let effect = w.dispatch_history(HistoryCommand::RunEntry, None);
        let Effect::Connect { request, .. } = effect else {
            panic!("expected Connect");
        };
        let (session, _) = FakeSession::boxed();
        let after = w.connect_finished(request, Ok(session));
        let editor_tab = w.tabs().active().map(Tab::id).unwrap();
        assert_ne!(editor_tab, tab);
        let Effect::Batch(effects) = after else {
            panic!("expected Batch, got {after:?}");
        };
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::RunQuery { job, .. } if job.statements[0].sql == "select 1 from t"))
        );
        assert!(w.query_tab(editor_tab).unwrap().is_running());
        assert_eq!(
            w.take_editor_seed(editor_tab).as_deref(),
            Some("select 1 from t")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_redacted_entry_is_not_run_again_but_can_be_opened() {
        let dir = temp_dir("redacted");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(
            &mut w,
            &svc,
            0,
            &[
                "UPDATE users SET api_token = 'hunter2' WHERE id = 1",
                "select $$a$$ as password",
                "select 1 from t",
            ],
        );
        // Newest first: the plain entry, then the two redacted ones.
        let rows = w.history_tab(tab).unwrap().results().len();
        assert_eq!(rows, 3);
        for (index, sql) in [(1, "select"), (2, "UPDATE")] {
            let stored = w.history_tab(tab).unwrap().results()[index]
                .sql()
                .to_owned();
            assert!(stored.starts_with(sql), "{stored}");
            assert!(stored.contains("'<redacted>'"), "{stored}");
            assert!(!stored.contains("hunter2"));
            w.select_history_entry(tab, index);
            let effect = w.dispatch_history(HistoryCommand::RunEntry, None);
            assert!(matches!(effect, Effect::None), "{effect:?}");
            assert_eq!(
                w.take_notice(),
                Some(
                    "Run again is not available: sensitive values were redacted from this entry."
                        .to_owned()
                )
            );
            assert!(w.history.pending.is_empty());
        }
        // Opening is allowed, with a notice.
        w.select_history_entry(tab, 2);
        let effect = w.dispatch_history(HistoryCommand::OpenEntry, None);
        assert!(matches!(effect, Effect::Connect { .. }), "{effect:?}");
        assert!(w.take_notice().unwrap().contains("enter them again"));
        // A plain entry still runs.
        w.select_history_entry(tab, 0);
        let effect = w.dispatch_history(HistoryCommand::RunEntry, None);
        assert!(matches!(effect, Effect::Connect { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_open_debug_hides_the_sql() {
        let shown = format!("{:?}", PendingOpen::Run("select 'topsecret'".to_owned()));
        assert!(!shown.contains("topsecret"), "{shown}");
        assert!(shown.contains("18"), "{shown}");
    }

    #[test]
    fn running_an_entry_on_prod_asks_for_the_confirmation() {
        let dir = temp_dir("prod");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 1, &["delete from t"]);
        w.select_history_entry(tab, 0);
        let effect = w.dispatch_history(HistoryCommand::RunEntry, None);
        let editor_tab = connect(&mut w, &effect);
        let Phase::Confirming(statements) = w.query_tab(editor_tab).unwrap().phase() else {
            panic!("expected a confirmation");
        };
        assert_eq!(statements[0].sql, "delete from t");
        assert!(!w.query_tab(editor_tab).unwrap().is_running());
        assert!(w.has_session(editor_tab));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_connect_failure_drops_the_pending_run() {
        let dir = temp_dir("fail");
        let svc = service(&dir);
        let mut w = ws();
        let tab = history_with(&mut w, &svc, 0, &["select 1"]);
        w.select_history_entry(tab, 0);
        let Effect::Connect { request, .. } = w.dispatch_history(HistoryCommand::RunEntry, None)
        else {
            panic!("expected Connect");
        };
        let failure = crate::connections::ConnectFailure::Target("down".to_owned());
        assert!(matches!(
            w.connect_finished(request, Err(failure)),
            Effect::None
        ));
        assert!(w.history.pending.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_entry_cut_at_recording_is_not_run_again() {
        let dir = temp_dir("cut");
        let svc = service(&dir);
        let mut w = ws();
        let huge = format!("select '{}'", "x".repeat(300_000));
        let tab = history_with(&mut w, &svc, 0, &[&huge]);
        assert!(w.history_tab(tab).unwrap().results()[0].truncated());
        w.select_history_entry(tab, 0);
        assert!(matches!(
            w.dispatch_history(HistoryCommand::RunEntry, None),
            Effect::None
        ));
        assert!(w.take_notice().unwrap().contains("cut"));
        // Opening it is fine.
        assert!(matches!(
            w.dispatch_history(HistoryCommand::OpenEntry, None),
            Effect::Connect { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- saved queries ----

    #[test]
    fn saved_query_changes_are_refused_until_loaded() {
        let mut w = ws();
        let tab = open(&mut w, 0);
        let _ = tab;
        for command in [
            HistoryCommand::SaveQuery,
            HistoryCommand::OpenSaved,
            HistoryCommand::RenameSaved,
            HistoryCommand::DeleteSaved,
        ] {
            assert!(matches!(
                w.dispatch_history(command, editor("select 1")),
                Effect::None
            ));
            assert_eq!(
                w.take_notice().as_deref(),
                Some("Saved queries are still loading.")
            );
            assert!(w.history_dialog().is_none());
            assert!(w.saved_picker().is_none());
        }
        w.saved_queries_loaded(Err("queries.toml: bad".to_owned()));
        assert!(matches!(w.saved().load(), SavedLoad::Failed(_)));
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Saved queries are not available: queries.toml: bad")
        );
        assert!(w.history_dialog().is_none());
    }

    #[test]
    fn saving_needs_an_editor_and_text() {
        let mut w = ready();
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
        open(&mut w, 0);
        w.dispatch_history(HistoryCommand::SaveQuery, None);
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
        w.dispatch_history(HistoryCommand::SaveQuery, editor("  \n "));
        assert_eq!(w.take_notice().as_deref(), Some("Nothing to save."));
        assert!(w.history_dialog().is_none());
    }

    #[test]
    fn save_rename_and_delete_flow() {
        let dir = temp_dir("saved");
        let svc = service(&dir);
        let mut w = ready();
        let editor_tab = open(&mut w, 0);

        // Save.
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        let Some(HistoryDialog::Name { text, error, .. }) = w.history_dialog() else {
            panic!("expected the name dialog");
        };
        assert_eq!((text.as_str(), error), ("", &None));
        // An empty name keeps the dialog open.
        let effect = w.submit_history_dialog();
        assert!(matches!(effect, Effect::None));
        let Some(HistoryDialog::Name { error, .. }) = w.history_dialog() else {
            panic!("the dialog must stay open");
        };
        assert!(error.as_deref().unwrap().contains("empty"));
        dialog_text(&mut w, "  daily  ");
        assert!(matches!(
            w.history_dialog(),
            Some(HistoryDialog::Name { error: None, .. })
        ));
        let effect = w.submit_history_dialog();
        let Effect::SaveSavedQueries { generation, .. } = &effect else {
            panic!("expected SaveSavedQueries, got {effect:?}");
        };
        assert_eq!(*generation, 1);
        apply(&mut w, &svc, effect);
        assert!(w.history_dialog().is_none());
        assert_eq!(names(&w), ["daily"]);
        let saved = &w.saved().queries().queries()[0];
        assert_eq!(saved.sql(), "select 1");
        assert_eq!(saved.connection(), Some(&conn(&w, 0)));
        let id = saved.id().clone();
        assert_eq!(service(&dir).load_saved().unwrap().queries().len(), 1);

        // Save the same tab again: the name is prefilled and overwrites the SQL.
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 2"));
        let Some(HistoryDialog::Name { text, .. }) = w.history_dialog() else {
            panic!("expected the name dialog");
        };
        assert_eq!(text, "daily");
        let effect = w.submit_history_dialog();
        assert!(matches!(
            effect,
            Effect::SaveSavedQueries { generation: 2, .. }
        ));
        apply(&mut w, &svc, effect);
        assert_eq!(names(&w), ["daily"]);
        assert_eq!(w.saved().queries().get(&id).unwrap().sql(), "select 2");

        // Another name is a new query.
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 3"));
        dialog_text(&mut w, "weekly");
        let effect = w.submit_history_dialog();
        apply(&mut w, &svc, effect);
        assert_eq!(names(&w), ["daily", "weekly"]);

        // A second tab cannot take an existing name; the dialog explains why and stays.
        let second = open(&mut w, 0);
        assert_ne!(second, editor_tab);
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 4"));
        dialog_text(&mut w, "daily");
        assert!(matches!(w.submit_history_dialog(), Effect::None));
        let Some(HistoryDialog::Name { error, text, .. }) = w.history_dialog() else {
            panic!("the dialog must stay open");
        };
        assert!(error.as_deref().unwrap().contains("already exists"));
        assert_eq!(text, "daily");
        w.cancel_history_dialog();
        assert_eq!(names(&w), ["daily", "weekly"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_and_delete_flow() {
        let dir = temp_dir("rename");
        let svc = service(&dir);
        let mut w = ready();
        let first_tab = open(&mut w, 0);
        for (name, sql) in [("daily", "select 1"), ("weekly", "select 3")] {
            w.dispatch_history(HistoryCommand::SaveQuery, editor(sql));
            dialog_text(&mut w, name);
            let effect = w.submit_history_dialog();
            apply(&mut w, &svc, effect);
            // The next save comes from a fresh tab, so it is a new query.
            open(&mut w, 0);
        }
        let id = w.saved().queries().queries()[0].id().clone();
        assert_eq!(names(&w), ["daily", "weekly"]);

        // Rename.
        w.dispatch_history(HistoryCommand::RenameSaved, None);
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Select a saved query first.")
        );
        w.select_saved(&id);
        w.dispatch_history(HistoryCommand::RenameSaved, None);
        let Some(HistoryDialog::Name { text, .. }) = w.history_dialog() else {
            panic!("expected the name dialog");
        };
        assert_eq!(text, "daily");
        dialog_text(&mut w, "weekly");
        assert!(matches!(w.submit_history_dialog(), Effect::None));
        assert!(matches!(
            w.history_dialog(),
            Some(HistoryDialog::Name { error: Some(_), .. })
        ));
        dialog_text(&mut w, "morning");
        let effect = w.submit_history_dialog();
        apply(&mut w, &svc, effect);
        assert_eq!(names(&w), ["morning", "weekly"]);
        assert_eq!(w.saved().generation(), 3);

        // Delete asks first.
        w.dispatch_history(HistoryCommand::DeleteSaved, None);
        assert!(matches!(
            w.history_dialog(),
            Some(HistoryDialog::ConfirmDelete { name, .. }) if name == "morning"
        ));
        w.cancel_history_dialog();
        assert_eq!(names(&w), ["morning", "weekly"]);
        w.dispatch_history(HistoryCommand::DeleteSaved, None);
        let effect = w.submit_history_dialog();
        assert!(matches!(
            effect,
            Effect::SaveSavedQueries { generation: 4, .. }
        ));
        apply(&mut w, &svc, effect);
        assert_eq!(names(&w), ["weekly"]);
        assert!(w.saved().selected().is_none());
        assert_eq!(service(&dir).load_saved().unwrap().queries().len(), 1);
        // The deleted query is no longer the one of its tab.
        w.dispatch(WorkspaceAction::ActivateTab(first_tab));
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 5"));
        let Some(HistoryDialog::Name { text, .. }) = w.history_dialog() else {
            panic!("expected the name dialog");
        };
        assert_eq!(text, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_write_is_reported() {
        let mut w = ready();
        w.saved_queries_written(3, Ok(()));
        assert!(w.take_notice().is_none());
        w.saved_queries_written(3, Err("read-only".to_owned()));
        assert!(w.take_notice().unwrap().contains("read-only"));
    }

    #[test]
    fn a_saved_query_opens_on_its_connection_and_binds_the_tab() {
        let mut w = ready();
        let editor_tab = open(&mut w, 1);
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        dialog_text(&mut w, "q");
        w.submit_history_dialog();
        let id = w.saved().queries().queries()[0].id().clone();
        // Bound to "prod", opened from a tab of "dev".
        w.dispatch(WorkspaceAction::CloseTab(editor_tab));
        let dev = open(&mut w, 0);
        assert_eq!(w.tabs().active().map(Tab::id), Some(dev));
        let effect = w.open_saved(&id);
        let tab = connect(&mut w, &effect);
        assert_eq!(w.tab_conns.get(&tab), Some(&conn(&w, 1)));
        assert_eq!(w.take_editor_seed(tab).as_deref(), Some("select 1"));
        // Saving from this tab proposes the saved name.
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        let Some(HistoryDialog::Name { text, .. }) = w.history_dialog() else {
            panic!("expected the name dialog");
        };
        assert_eq!(text, "q");
    }

    #[test]
    fn an_unbound_saved_query_uses_the_active_or_selected_connection() {
        let mut w = ready();
        let draft = SavedQueryDraft::new("free", "select 9");
        let mut queries = SavedQueries::new();
        let id = queries.create(draft).unwrap();
        w.saved_queries_loaded(Ok(queries));
        assert!(matches!(w.open_saved(&id), Effect::None));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Select a connection in the sidebar first.")
        );
        w.dispatch(WorkspaceAction::SelectConnection(1));
        let Effect::Connect { request, .. } = w.open_saved(&id) else {
            panic!("expected Connect");
        };
        assert_eq!(
            w.requests.get(&request),
            Some(&crate::workspace::Purpose::Open(conn(&w, 1)))
        );
        let _ = open(&mut w, 0);
        let Effect::Connect { request, .. } = w.open_saved(&id) else {
            panic!("expected Connect");
        };
        assert_eq!(
            w.requests.get(&request),
            Some(&crate::workspace::Purpose::Open(conn(&w, 0)))
        );
        // An unknown id does nothing.
        let other = {
            let mut q = SavedQueries::new();
            q.create(SavedQueryDraft::new("x", "")).unwrap();
            q.create(SavedQueryDraft::new("y", "")).unwrap()
        };
        assert!(matches!(w.open_saved(&other), Effect::None));
    }

    #[test]
    fn the_picker_filters_moves_and_opens() {
        let mut w = ready();
        let mut queries = SavedQueries::new();
        for (name, folder) in [
            ("alpha", None),
            ("beta", Some("ops")),
            ("gamma", Some("ops")),
        ] {
            queries
                .create(SavedQueryDraft {
                    folder: folder.map(str::to_owned),
                    ..SavedQueryDraft::new(name, format!("select '{name}'"))
                })
                .unwrap();
        }
        w.saved_queries_loaded(Ok(queries));
        w.dispatch(WorkspaceAction::SelectConnection(0));
        w.dispatch_history(HistoryCommand::OpenSaved, None);
        assert!(w.saved_picker().is_some());
        w.set_picker_filter("ops".to_owned());
        let matches: Vec<_> = w
            .saved_picker()
            .unwrap()
            .matches(w.saved().queries())
            .iter()
            .map(|q| q.name().to_owned())
            .collect();
        assert_eq!(matches, ["beta", "gamma"]);
        w.move_picker(1);
        w.move_picker(1);
        assert_eq!(w.saved_picker().unwrap().selected(), 1);
        let effect = w.confirm_picker();
        let tab = connect(&mut w, &effect);
        assert!(w.saved_picker().is_none());
        assert_eq!(w.take_editor_seed(tab).as_deref(), Some("select 'gamma'"));

        // A filter without a match keeps the picker open; Escape closes it.
        w.dispatch_history(HistoryCommand::OpenSaved, None);
        w.set_picker_filter("zzz".to_owned());
        assert!(matches!(w.confirm_picker(), Effect::None));
        assert!(w.saved_picker().is_some());
        w.close_picker();
        assert!(w.saved_picker().is_none());
        assert!(matches!(w.confirm_picker(), Effect::None));
    }

    #[test]
    fn history_dialogs_wait_for_the_connection_dialogs() {
        let mut w = ready();
        open(&mut w, 0);
        w.dialog = Some(crate::connections::Dialog::MoveTo { id: conn(&w, 0) });
        for command in [HistoryCommand::ClearHistory, HistoryCommand::OpenSaved] {
            w.dispatch_history(command, editor("select 1"));
            assert_eq!(
                w.take_notice().as_deref(),
                Some("Close the open dialog first.")
            );
        }
        w.dispatch_history(HistoryCommand::SaveQuery, editor("select 1"));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Close the open dialog first.")
        );
        assert!(w.history_dialog().is_none());
        assert!(w.saved_picker().is_none());
    }
}
