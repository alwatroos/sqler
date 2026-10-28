// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL editor inside the workspace: opening editor tabs, and the commands that work on the
//! text of the active one.
//!
//! The text lives in the view. A command that needs it gets an [`EditorBuffer`] copied out of the
//! view; the answer is an [`Effect`] (an edit or a selection) that the view applies to its editor.
//! Query execution goes through the [`RunPort`] seam.

use sqler_config::{ConnectionId, SessionLoad, SessionState, SessionTab};
use sqler_driver_api::Session;
use sqler_driver_api::sqler_core::DriverId;
use sqler_sql::{Dialect, FormatOptions};

use super::state::{Effect, Purpose, Workspace, tab_connection};
use crate::browser::ObjectTabContent;
use crate::connections::ConnectFailure;
use crate::editor::{
    BracketIndex, EditorAction, EditorBuffer, EditorDoc, EditorKey, OccurrenceStep, RunMode,
    RunPort, format_edit, synthetic, toggle_block_comment, toggle_line_comment,
};
use crate::query::QueryTab;
use crate::tabs::{TabConnection, TabId, TabKind};

/// How many lines the synthetic script of `dev:open-large-sql-script` has.
const SYNTHETIC_LINES: usize = 10_000;

impl Workspace {
    /// The headless state of an editor tab, if `tab` is one.
    #[must_use]
    pub fn editor_doc(&self, tab: TabId) -> Option<&EditorDoc> {
        self.editors.get(tab)
    }

    /// The text of an editor tab changed: a preview tab is pinned and the tab is dirty.
    pub fn editor_edited(&mut self, tab: TabId) {
        self.tabs.pin(tab);
        if let Some(doc) = self.editors.get_mut(tab) {
            doc.mark_dirty();
        }
    }

    /// Takes the start text of an editor tab (once); the view puts it into the new editor.
    pub fn take_editor_seed(&mut self, tab: TabId) -> Option<String> {
        self.editors.take_seed(tab)
    }

    /// Rebuilds the editor tabs of the last run from `session.json`.
    ///
    /// Every tab comes back without a session; only the active one connects now (the returned
    /// effect), the others when they are first activated. A tab whose connection no longer
    /// exists stays open with its text and no connection.
    pub fn restore_tabs(&mut self, load: &SessionLoad) -> Effect {
        match load {
            SessionLoad::Missing => return Effect::None,
            SessionLoad::Newer(_) => {
                self.session_newer = true;
                return Effect::None;
            }
            SessionLoad::Invalid(message) => {
                self.notice = Some(format!(
                    "Could not read session.json, the open tabs were not restored \
                     (the file is copied to session.json.bak before it is replaced): {message}"
                ));
                return Effect::None;
            }
            SessionLoad::Loaded(_) => {}
        }
        let SessionLoad::Loaded(state) = load else {
            return Effect::None;
        };
        let mut opened: Vec<Option<TabId>> = Vec::with_capacity(state.tabs.len());
        let mut lost = 0usize;
        for saved in &state.tabs {
            let found = saved.connection.as_deref().and_then(|wanted| {
                self.sidebar
                    .entries()
                    .iter()
                    .find(|e| e.id().as_str() == wanted)
                    .map(|e| (e.id().clone(), tab_connection(e.config())))
            });
            let (connection, conn) = if let Some((id, connection)) = found {
                (connection, Some(id))
            } else {
                if saved.connection.is_some() {
                    lost += 1;
                }
                let Ok(driver) = DriverId::new(&saved.driver).or_else(|_| DriverId::new("unknown"))
                else {
                    opened.push(None);
                    continue;
                };
                let connection = TabConnection {
                    name: "No connection".to_owned(),
                    driver,
                    color: None,
                    environment: None,
                };
                (connection, None)
            };
            let dialect = Dialect::from_driver_id(connection.driver.as_str());
            let tab = self.tabs.open(connection, TabKind::Query);
            let seed = (!saved.text.is_empty()).then(|| saved.text.clone());
            self.editors.open(tab, dialect, seed);
            if saved.dirty
                && let Some(doc) = self.editors.get_mut(tab)
            {
                doc.mark_dirty();
            }
            self.queries.insert(tab, QueryTab::new());
            if let Some(id) = conn {
                self.tab_conns.insert(tab, id);
                self.restored.insert(tab);
            } else if let Some(wanted) = &saved.connection {
                self.orphan_conns.insert(tab, wanted.clone());
            }
            opened.push(Some(tab));
        }
        let kept: Vec<TabId> = opened.iter().flatten().copied().collect();
        let active = state
            .active
            .and_then(|i| opened.get(i).copied().flatten())
            .or_else(|| kept.first().copied());
        if let Some(tab) = active {
            self.tabs.activate(tab);
        }
        if !kept.is_empty() {
            self.show_connection_tabs();
        }
        if !state.chat_draft.is_empty() {
            self.pending_chat_draft = Some(state.chat_draft.clone());
        }
        // A connections.toml that failed to load explains every miss; the sidebar says so itself.
        if lost > 0 && self.sidebar.load_error().is_none() {
            self.notice = Some(format!(
                "{lost} restored tab(s) lost their connection (it no longer exists); \
                 the text is kept and the tab has no connection."
            ));
        }
        self.attach_active_restored()
    }

    /// Connects the active tab if it is a restored one that has no session yet. It acts once per
    /// activation: a failed attach is tried again only when the tab is activated again.
    pub fn attach_active_restored(&mut self) -> Effect {
        let active = self.tabs.active().map(crate::tabs::Tab::id);
        if active == self.attach_seen {
            return Effect::None;
        }
        self.attach_seen = active;
        let Some(tab) = active.filter(|t| self.restored.contains(t)) else {
            return Effect::None;
        };
        let Some(conn) = self.tab_conns.get(&tab).cloned() else {
            self.restored.remove(&tab);
            return Effect::None;
        };
        self.attach_tab(tab, &conn)
    }

    /// Starts the connect attempt that gives restored `tab` its session.
    pub(super) fn attach_tab(&mut self, tab: TabId, conn: &ConnectionId) -> Effect {
        let attaching = self
            .requests
            .values()
            .any(|p| matches!(p, Purpose::Attach { tab: t, .. } if *t == tab));
        if attaching {
            return Effect::None;
        }
        let Some(index) = self.sidebar.index_of(conn) else {
            self.restored.remove(&tab);
            return self.notify_editor("The connection no longer exists.");
        };
        let Some(connect) = self.connect_request_for(index) else {
            return Effect::None;
        };
        self.sidebar.begin_connect(index);
        let request = self.new_request(Purpose::Attach {
            tab,
            conn: conn.clone(),
        });
        Effect::Connect { request, connect }
    }

    /// The outcome of the connect attempt of a restored tab.
    pub(super) fn attach_finished(
        &mut self,
        tab: TabId,
        conn: &ConnectionId,
        result: Result<Box<dyn Session>, ConnectFailure>,
    ) -> Effect {
        let index = self.sidebar.index_of(conn);
        let alive =
            index.is_some() && self.tabs.get(tab).is_some() && !self.sessions.contains_key(&tab);
        let (Some(index), true) = (index, alive) else {
            return match result {
                Ok(session) => Effect::CloseSession(session),
                Err(_) => Effect::None,
            };
        };
        match result {
            Ok(session) => {
                self.sidebar.finish_connect(index, Ok(()));
                self.sessions.insert(tab, session);
                self.recent_connections.record(conn);
                self.restored.remove(&tab);
                self.ensure_browser(conn)
            }
            Err(failure) => {
                self.sidebar.finish_connect(index, Err(failure.to_string()));
                if let ConnectFailure::UnknownHostKey(key) = failure {
                    self.ask_host_key(
                        key,
                        Purpose::Attach {
                            tab,
                            conn: conn.clone(),
                        },
                    );
                }
                Effect::None
            }
        }
    }

    /// What `session.json` should hold now; `None` while the file is from a newer `SQLer`.
    /// `text` gives the current text of an editor tab (the view owns it).
    #[must_use]
    pub fn session_state(&self, text: &dyn Fn(TabId) -> Option<String>) -> Option<SessionState> {
        if self.session_newer {
            return None;
        }
        let active = self.tabs.active().map(crate::tabs::Tab::id);
        let mut state = SessionState::default();
        for tab in self.tabs.iter().filter(|t| t.kind() == TabKind::Query) {
            let id = tab.id();
            let Some(doc) = self.editors.get(id) else {
                continue;
            };
            if active == Some(id) {
                state.active = Some(state.tabs.len());
            }
            let text = text(id)
                .or_else(|| doc.seed().map(str::to_owned))
                .unwrap_or_default();
            state.tabs.push(SessionTab {
                connection: self
                    .tab_conns
                    .get(&id)
                    .map(|c| c.as_str().to_owned())
                    .or_else(|| self.orphan_conns.get(&id).cloned()),
                driver: tab.connection().driver.to_string(),
                text,
                dirty: doc.is_dirty(),
            });
        }
        state.chat_draft = match self.chat() {
            Some(chat) => chat.draft().to_owned(),
            None => self.pending_chat_draft.clone().unwrap_or_default(),
        };
        Some(state)
    }

    /// Runs an editor command. `buffer` is a copy of the active editor's text, selection and cursor
    /// (`None` when the view has no editor on screen).
    pub fn dispatch_editor(
        &mut self,
        action: EditorAction,
        buffer: Option<EditorBuffer>,
    ) -> Effect {
        match action {
            EditorAction::New => self.new_editor(None),
            EditorAction::OpenSyntheticScript => {
                self.new_editor(Some(synthetic::script(SYNTHETIC_LINES)))
            }
            EditorAction::AddCursorAbove => self.editor_key(EditorKey::AddCursorAbove),
            EditorAction::AddCursorBelow => self.editor_key(EditorKey::AddCursorBelow),
            EditorAction::TriggerCompletion => self.editor_key(EditorKey::ShowCompletions),
            EditorAction::FormatSql => match self.editor_target(buffer) {
                Some((tab, buffer)) => self.format(tab, &buffer),
                None => Effect::None,
            },
            EditorAction::ToggleLineComment => match self.editor_target(buffer) {
                Some((tab, b)) => toggle_line_comment(&b)
                    .map_or(Effect::None, |edit| Effect::EditorEdit { tab, edit }),
                None => Effect::None,
            },
            EditorAction::ToggleBlockComment => match self.editor_target(buffer) {
                Some((tab, b)) => toggle_block_comment(&b)
                    .map_or(Effect::None, |edit| Effect::EditorEdit { tab, edit }),
                None => Effect::None,
            },
            EditorAction::AddNextOccurrence => match self.editor_target(buffer) {
                Some((tab, buffer)) => self.add_next_occurrence(tab, &buffer),
                None => Effect::None,
            },
            EditorAction::GoToMatchingBracket => match self.editor_target(buffer) {
                Some((tab, buffer)) => self.go_to_matching_bracket(tab, &buffer),
                None => Effect::None,
            },
        }
    }

    /// Runs the selection, the statement at the cursor or the whole text of the active editor
    /// through `port`.
    pub fn run_editor(
        &mut self,
        port: &mut dyn RunPort,
        mode: RunMode,
        buffer: Option<EditorBuffer>,
    ) -> Effect {
        let Some((tab, buffer)) = self.editor_target(buffer) else {
            return Effect::None;
        };
        match port.run(self, tab, mode, buffer) {
            Ok(effect) => effect,
            Err(message) => self.notify_editor(message),
        }
    }

    /// Cancels the run of the active editor through `port`.
    pub fn cancel_editor_run(&mut self, port: &mut dyn RunPort) -> Effect {
        let Some(tab) = self.active_editor() else {
            return self.notify_editor(NO_EDITOR);
        };
        match port.cancel(self, tab) {
            Ok(effect) => effect,
            Err(message) => self.notify_editor(message),
        }
    }

    /// Opens an editor on `conn` that starts with `seed`.
    pub(super) fn open_editor_on(&mut self, conn: &ConnectionId, seed: Option<String>) -> Effect {
        let Some(index) = self.sidebar.index_of(conn) else {
            return self.notify_editor("The connection no longer exists.");
        };
        let effect = self.open_connection(index);
        if let (Effect::Connect { request, .. }, Some(seed)) = (&effect, seed) {
            self.editor_seeds.insert(*request, seed);
        }
        effect
    }

    /// The text of the DDL tab that is on screen, if it shows a script.
    pub(super) fn active_script_text(&self) -> Option<(ConnectionId, String)> {
        let tab = self.tabs.active()?.id();
        let ObjectTabContent::Script(text) = &self.object_tabs.get(&tab)?.content else {
            return None;
        };
        Some((self.tab_conns.get(&tab)?.clone(), text.clone()))
    }

    fn new_editor(&mut self, seed: Option<String>) -> Effect {
        let conn = self
            .tabs
            .active()
            .and_then(|tab| self.tab_conns.get(&tab.id()))
            .or_else(|| self.sidebar.selected_id())
            .cloned();
        match conn {
            Some(conn) => self.open_editor_on(&conn, seed),
            None => self.notify_editor("Select a connection in the sidebar first."),
        }
    }

    fn active_editor(&self) -> Option<TabId> {
        self.tabs
            .active()
            .map(crate::tabs::Tab::id)
            .filter(|tab| self.editors.contains(*tab))
    }

    /// The active editor tab with the buffer, or a notice why there is none.
    fn editor_target(&mut self, buffer: Option<EditorBuffer>) -> Option<(TabId, EditorBuffer)> {
        if let (Some(tab), Some(buffer)) = (self.active_editor(), buffer) {
            Some((tab, buffer))
        } else {
            self.notice = Some(NO_EDITOR.to_owned());
            None
        }
    }

    fn notify_editor(&mut self, message: impl Into<String>) -> Effect {
        self.notice = Some(message.into());
        Effect::None
    }

    fn editor_key(&mut self, key: EditorKey) -> Effect {
        match self.active_editor() {
            Some(tab) => Effect::EditorKey { tab, key },
            None => self.notify_editor(NO_EDITOR),
        }
    }

    fn format(&mut self, tab: TabId, buffer: &EditorBuffer) -> Effect {
        let Some(dialect) = self.editors.get(tab).and_then(EditorDoc::dialect) else {
            let driver = self
                .tabs
                .get(tab)
                .map_or_else(String::new, |t| t.connection().driver.to_string());
            return self.notify_editor(format!(
                "Formatting is not available for {driver} connections."
            ));
        };
        match format_edit(dialect, buffer, &FormatOptions::default()) {
            Ok(Some(edit)) => Effect::EditorEdit { tab, edit },
            Ok(None) => Effect::None,
            Err(failure) => self.notify_editor(failure.to_string()),
        }
    }

    fn add_next_occurrence(&mut self, tab: TabId, buffer: &EditorBuffer) -> Effect {
        let Some(doc) = self.editors.get_mut(tab) else {
            return Effect::None;
        };
        match doc.add_next_occurrence(buffer) {
            OccurrenceStep::Select(range) => Effect::EditorSelect { tab, range },
            OccurrenceStep::Add(range) => Effect::EditorAddSelection { tab, range },
            _ => Effect::None,
        }
    }

    fn go_to_matching_bracket(&mut self, tab: TabId, buffer: &EditorBuffer) -> Effect {
        let dialect = self.editors.get(tab).and_then(EditorDoc::dialect);
        let index = BracketIndex::build(dialect, &buffer.text);
        // The bracket to jump from: a lone selected one (the result of the last jump), else the
        // one at the cursor (as `pair_at` and the frame do), else the one before it.
        let bytes = buffer.text.as_bytes();
        let is_bracket = |p: usize| bytes.get(p).is_some_and(|b| b"()[]{}".contains(b));
        let here = Some(buffer.selection.start)
            .filter(|&p| buffer.selection.len() == 1 && is_bracket(p))
            .or_else(|| Some(buffer.cursor).filter(|&p| is_bracket(p)))
            .or_else(|| buffer.cursor.checked_sub(1).filter(|&p| is_bracket(p)));
        let Some((here, (open, close))) =
            here.and_then(|p| index.pair_at(&buffer.text, p).map(|pair| (p, pair)))
        else {
            return self.notify_editor("No bracket next to the cursor.");
        };
        let target = if here == open { close } else { open };
        Effect::EditorSelect {
            tab,
            range: target..target + 1,
        }
    }
}

/// The notice when a command needs an editor and none is active.
const NO_EDITOR: &str = "Open a SQL editor first.";

#[cfg(test)]
mod tests {
    use sqler_config::ConnectionId;
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId, Environment};

    use super::*;
    use crate::editor::FakeRunPort;
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::state::RequestId;
    use crate::workspace::testing::FakeSession;

    fn ws_with(driver: &str) -> Workspace {
        let driver = DriverId::new(driver).unwrap();
        Workspace::new(SidebarModel::from_connections(vec![
            ConnectionConfig::new("dev", driver, ":memory:")
                .with_environment(Environment::Prod)
                .with_color("#00ff00"),
        ]))
    }

    fn ws() -> Workspace {
        ws_with("sqlite")
    }

    fn connect_request(effect: Effect) -> RequestId {
        match effect {
            Effect::Connect { request, .. } => request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    /// Opens connection 0 and returns the editor tab.
    fn open(w: &mut Workspace) -> TabId {
        let request = connect_request(w.dispatch(WorkspaceAction::OpenConnection(0)));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        w.tabs().active().map(crate::tabs::Tab::id).unwrap()
    }

    fn buffer(text: &str, cursor: usize) -> EditorBuffer {
        EditorBuffer::new(text, cursor)
    }

    #[test]
    fn opening_a_connection_makes_an_editor_tab() {
        let mut w = ws();
        let tab = open(&mut w);
        let doc = w.editor_doc(tab).unwrap();
        assert_eq!(doc.dialect(), Some(sqler_sql::Dialect::Sqlite));
        let header = w.tabs().get(tab).unwrap().header();
        assert_eq!(header.title, "dev");
        assert_eq!(header.badge, Some("PROD"));
        assert!(header.accent.is_some());
        assert_eq!(w.take_editor_seed(tab), None);
    }

    #[test]
    fn a_driver_without_a_dialect_still_opens_an_editor() {
        let mut w = ws_with("fake");
        let tab = open(&mut w);
        assert!(w.editor_doc(tab).is_some());
        assert_eq!(w.editor_doc(tab).and_then(EditorDoc::dialect), None);
        let effect = w.dispatch_editor(EditorAction::FormatSql, Some(buffer("select 1", 0)));
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Formatting is not available for fake connections.")
        );
    }

    #[test]
    fn new_editor_uses_the_active_tabs_connection() {
        let mut w = ws();
        let first = open(&mut w);
        let request = connect_request(w.dispatch_editor(EditorAction::New, None));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        let second = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        assert_ne!(first, second);
        assert!(w.editor_doc(second).is_some());
        assert_eq!(w.take_editor_seed(second), None);
    }

    #[test]
    fn new_editor_falls_back_to_the_selected_connection() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let request = connect_request(w.dispatch_editor(EditorAction::New, None));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        assert_eq!(w.tabs().len(), 1);
    }

    #[test]
    fn new_editor_without_a_connection_is_a_notice() {
        let mut w = ws();
        assert!(matches!(
            w.dispatch_editor(EditorAction::New, None),
            Effect::None
        ));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("Select a connection in the sidebar first.")
        );
    }

    #[test]
    fn the_synthetic_script_is_the_seed() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let request = connect_request(w.dispatch_editor(EditorAction::OpenSyntheticScript, None));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        let seed = w.take_editor_seed(tab).unwrap();
        assert_eq!(seed.lines().count(), SYNTHETIC_LINES);
        assert_eq!(w.take_editor_seed(tab), None, "taken once");
    }

    #[test]
    fn a_seed_for_a_dead_request_is_dropped() {
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let request = connect_request(w.dispatch_editor(EditorAction::New, None));
        w.editor_seeds.insert(request, "select 1".to_owned());
        let (session, _) = FakeSession::boxed();
        // Cancel the attempt, then report it.
        w.requests.remove(&request);
        w.connect_finished(request, Ok(session));
        assert!(w.editor_seeds.is_empty());
        assert!(w.tabs().is_empty());
    }

    #[test]
    fn the_seed_survives_a_host_key_prompt() {
        use crate::connections::{ConnectFailure, HostKeyPrompt};
        let mut w = ws();
        w.dispatch(WorkspaceAction::SelectConnection(0));
        let request = connect_request(w.dispatch_editor(EditorAction::New, None));
        w.editor_seeds.insert(request, "select 1".to_owned());
        let prompt = HostKeyPrompt {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        };
        w.connect_finished(request, Err(ConnectFailure::UnknownHostKey(prompt)));
        let Some(crate::connections::Dialog::TrustHostKey { retry, .. }) = w.dialog() else {
            panic!("expected the host-key prompt");
        };
        let retry = retry.clone();
        let retried = connect_request(w.trust_finished(retry, Ok(())));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(retried, Ok(session));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        assert_eq!(w.take_editor_seed(tab).as_deref(), Some("select 1"));
        assert!(w.retry_seeds.is_empty());
    }

    #[test]
    fn closing_the_tab_drops_the_editor_state() {
        let mut w = ws();
        let tab = open(&mut w);
        assert!(w.tab_conns.contains_key(&tab));
        w.dispatch(WorkspaceAction::CloseTab(tab));
        assert!(w.editor_doc(tab).is_none());
        assert!(!w.tab_conns.contains_key(&tab));
    }

    #[test]
    fn open_in_editor_seeds_the_text() {
        let mut w = ws();
        let conn: ConnectionId = w
            .sidebar()
            .selected_id()
            .cloned()
            .unwrap_or_else(|| w.sidebar().entries()[0].id().clone());
        let request = connect_request(w.open_editor_on(&conn, Some("select 1".into())));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(request, Ok(session));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        assert_eq!(w.take_editor_seed(tab).as_deref(), Some("select 1"));
    }

    #[test]
    fn commands_need_an_editor_and_a_buffer() {
        let mut w = ws();
        for action in [
            EditorAction::FormatSql,
            EditorAction::AddNextOccurrence,
            EditorAction::GoToMatchingBracket,
            EditorAction::AddCursorAbove,
            EditorAction::AddCursorBelow,
        ] {
            let effect = w.dispatch_editor(action, Some(buffer("select 1", 0)));
            assert!(matches!(effect, Effect::None), "{action:?}");
            assert_eq!(
                w.take_notice().as_deref(),
                Some("Open a SQL editor first."),
                "{action:?}"
            );
        }
        let tab = open(&mut w);
        let effect = w.dispatch_editor(EditorAction::FormatSql, None);
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
        // A grid tab is not an editor.
        w.dispatch(WorkspaceAction::OpenSyntheticGrid);
        assert_ne!(w.tabs().active().map(crate::tabs::Tab::id), Some(tab));
        let effect = w.dispatch_editor(EditorAction::FormatSql, Some(buffer("select 1", 0)));
        assert!(matches!(effect, Effect::None));
        assert!(w.take_notice().is_some());
    }

    #[test]
    fn toggles_return_exactly_one_edit() {
        let mut w = ws();
        let tab = open(&mut w);
        let line = w.dispatch_editor(EditorAction::ToggleLineComment, Some(buffer("select 1", 3)));
        match line {
            Effect::EditorEdit { tab: t, edit } => {
                assert_eq!(t, tab);
                assert_eq!(edit.text, "-- select 1");
            }
            other => panic!("expected EditorEdit, got {other:?}"),
        }
        let block = w.dispatch_editor(
            EditorAction::ToggleBlockComment,
            Some(buffer("select 1", 3)),
        );
        assert!(matches!(block, Effect::EditorEdit { .. }));
    }

    #[test]
    fn toggles_need_an_editor() {
        let mut w = ws();
        let effect = w.dispatch_editor(EditorAction::ToggleLineComment, None);
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
    }

    #[test]
    fn format_returns_one_edit() {
        let mut w = ws();
        let tab = open(&mut w);
        let effect = w.dispatch_editor(
            EditorAction::FormatSql,
            Some(buffer("select a,b from t where x=1", 3)),
        );
        match effect {
            Effect::EditorEdit { tab: t, edit } => {
                assert_eq!(t, tab);
                assert_eq!(edit.range, 0..27);
                assert!(edit.text.contains('\n'));
            }
            other => panic!("expected EditorEdit, got {other:?}"),
        }
        assert_eq!(w.take_notice(), None);
    }

    #[test]
    fn formatting_formatted_text_is_a_no_op() {
        let mut w = ws();
        open(&mut w);
        let Effect::EditorEdit { edit, .. } = w.dispatch_editor(
            EditorAction::FormatSql,
            Some(buffer("select a,b from t", 0)),
        ) else {
            panic!("expected an edit");
        };
        let effect = w.dispatch_editor(EditorAction::FormatSql, Some(buffer(&edit.text, 0)));
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice(), None);
    }

    #[test]
    fn format_failure_is_a_notice_with_line_and_column() {
        let mut w = ws();
        open(&mut w);
        let effect = w.dispatch_editor(
            EditorAction::FormatSql,
            Some(buffer("select 1;\nselect 'oops", 0)),
        );
        assert!(matches!(effect, Effect::None));
        let notice = w.take_notice().unwrap();
        assert!(
            notice.starts_with("Cannot format: unterminated"),
            "{notice}"
        );
        assert!(notice.contains("line 2, column 8"), "{notice}");
    }

    #[test]
    fn add_next_occurrence_selects_the_word_then_adds() {
        let mut w = ws();
        let tab = open(&mut w);
        let text = "select a from a";
        let mut b = buffer(text, 8);
        let Effect::EditorSelect { tab: t, range } =
            w.dispatch_editor(EditorAction::AddNextOccurrence, Some(b.clone()))
        else {
            panic!("expected EditorSelect");
        };
        assert_eq!(t, tab);
        assert_eq!(range, 7..8);
        b.selection = range;
        let effect = w.dispatch_editor(EditorAction::AddNextOccurrence, Some(b));
        assert!(
            matches!(effect, Effect::EditorAddSelection { ref range, .. } if *range == (14..15)),
            "{effect:?}"
        );
    }

    #[test]
    fn go_to_matching_bracket_selects_the_partner() {
        let mut w = ws();
        let tab = open(&mut w);
        let text = "select (1 + 2)";
        let Effect::EditorSelect { tab: t, range } =
            w.dispatch_editor(EditorAction::GoToMatchingBracket, Some(buffer(text, 7)))
        else {
            panic!("expected EditorSelect");
        };
        assert_eq!((t, range), (tab, 13..14));
        let Effect::EditorSelect { range, .. } =
            w.dispatch_editor(EditorAction::GoToMatchingBracket, Some(buffer(text, 14)))
        else {
            panic!("expected EditorSelect");
        };
        assert_eq!(range, 7..8);
        let effect = w.dispatch_editor(EditorAction::GoToMatchingBracket, Some(buffer(text, 2)));
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            w.take_notice().as_deref(),
            Some("No bracket next to the cursor.")
        );
    }

    #[test]
    fn go_to_matching_bracket_toggles_on_nested_and_empty_pairs() {
        let mut w = ws();
        open(&mut w);
        let jump = |w: &mut Workspace, b: EditorBuffer| {
            let Effect::EditorSelect { range, .. } =
                w.dispatch_editor(EditorAction::GoToMatchingBracket, Some(b))
            else {
                panic!("expected EditorSelect");
            };
            range
        };
        let text = "f(g(x))";
        // Cursor right after the inner `(`: to the inner `)`.
        let range = jump(&mut w, buffer(text, 4));
        assert_eq!(range, 5..6);
        // The jump leaves `)` selected with the cursor after it: back to the inner `(`.
        let again = EditorBuffer {
            text: text.to_owned(),
            selection: range.clone(),
            cursor: range.end,
        };
        assert_eq!(jump(&mut w, again), 3..4);
        let again = EditorBuffer {
            text: text.to_owned(),
            selection: 3..4,
            cursor: 4,
        };
        assert_eq!(jump(&mut w, again), 5..6);
        // `()` with the cursor between: the bracket at the cursor is the closing one.
        assert_eq!(jump(&mut w, buffer("()", 1)), 0..1);
        // `(a)(b)` with the cursor between the pairs: the bracket at the cursor wins.
        assert_eq!(jump(&mut w, buffer("(a)(b)", 3)), 5..6);
    }

    #[test]
    fn add_cursor_commands_forward_the_key() {
        let mut w = ws();
        let tab = open(&mut w);
        assert!(matches!(
            w.dispatch_editor(EditorAction::AddCursorAbove, None),
            Effect::EditorKey { tab: t, key: EditorKey::AddCursorAbove } if t == tab
        ));
        assert!(matches!(
            w.dispatch_editor(EditorAction::AddCursorBelow, None),
            Effect::EditorKey {
                key: EditorKey::AddCursorBelow,
                ..
            }
        ));
    }

    #[test]
    fn trigger_completion_asks_the_editor_to_show_completions() {
        let mut w = ws();
        let tab = open(&mut w);
        assert!(matches!(
            w.dispatch_editor(EditorAction::TriggerCompletion, None),
            Effect::EditorKey { tab: t, key: EditorKey::ShowCompletions } if t == tab
        ));
    }

    #[test]
    fn run_hands_tab_mode_and_buffer_to_the_port() {
        let mut w = ws();
        let tab = open(&mut w);
        let mut port = FakeRunPort::default();
        let b = buffer("select 1", 8);
        let effect = w.run_editor(&mut port, RunMode::Statement, Some(b.clone()));
        assert!(matches!(effect, Effect::None));
        let effect = w.cancel_editor_run(&mut port);
        assert!(matches!(effect, Effect::None));
        assert_eq!(
            port.calls,
            vec![(tab, Some(RunMode::Statement), Some(b)), (tab, None, None)]
        );
    }

    #[test]
    fn run_refuses_a_tab_that_is_not_an_editor() {
        let mut w = ws();
        let mut port = FakeRunPort::default();
        w.run_editor(&mut port, RunMode::All, Some(buffer("select 1", 0)));
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
        w.cancel_editor_run(&mut port);
        assert_eq!(w.take_notice().as_deref(), Some("Open a SQL editor first."));
        assert_eq!(port.calls.len(), 0);
    }

    #[test]
    fn a_port_error_is_a_notice() {
        struct Failing;
        impl RunPort for Failing {
            fn run(
                &mut self,
                _: &mut Workspace,
                _: TabId,
                _: RunMode,
                _: EditorBuffer,
            ) -> Result<Effect, String> {
                Err("nope".to_owned())
            }
            fn cancel(&mut self, _: &mut Workspace, _: TabId) -> Result<Effect, String> {
                Err("nothing runs".to_owned())
            }
        }
        let mut w = ws();
        open(&mut w);
        w.run_editor(&mut Failing, RunMode::All, Some(buffer("select 1", 0)));
        assert_eq!(w.take_notice().as_deref(), Some("nope"));
        w.cancel_editor_run(&mut Failing);
        assert_eq!(w.take_notice().as_deref(), Some("nothing runs"));
    }
}

#[cfg(test)]
mod session_tests {
    use sqler_config::{
        AiConfig, ConnectionAi, ConnectionSpec, ModelChoice, ProviderConfig, ProviderId,
        ProviderKind, SecretKind,
    };
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::ai::{AiAction, AiAudit, AiState};
    use crate::sidebar::SidebarModel;
    use crate::workspace::WorkspaceAction;
    use crate::workspace::state::RequestId;
    use crate::workspace::testing::FakeSession;

    fn sidebar() -> SidebarModel {
        let sqlite = DriverId::new("sqlite").unwrap();
        SidebarModel::from_connections(vec![
            ConnectionConfig::new("one", sqlite.clone(), ":memory:"),
            ConnectionConfig::new("two", sqlite, ":memory:"),
        ])
    }

    fn request(effect: Effect) -> RequestId {
        match effect {
            Effect::Connect { request, .. } => request,
            other => panic!("expected Connect, got {other:?}"),
        }
    }

    fn no_text(_: TabId) -> Option<String> {
        None
    }

    /// Opens a connection and gives the editor tab its text (as typing would).
    fn editor(w: &mut Workspace, index: usize) -> TabId {
        let r = request(w.dispatch(WorkspaceAction::OpenConnection(index)));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(r, Ok(session));
        let tab = w.tabs().active().map(crate::tabs::Tab::id).unwrap();
        w.take_editor_seed(tab);
        w.editor_edited(tab);
        tab
    }

    /// The session state of `w`, with `texts` as the live editor text of its tabs in order.
    fn state_of(w: &Workspace, texts: &[(TabId, &str)]) -> SessionState {
        let map: std::collections::HashMap<TabId, String> =
            texts.iter().map(|(t, s)| (*t, (*s).to_owned())).collect();
        w.session_state(&|t| map.get(&t).cloned()).unwrap()
    }

    #[test]
    fn the_open_tabs_come_back_in_a_fresh_workspace() {
        let mut w = Workspace::new(sidebar());
        let first = editor(&mut w, 0);
        let second = editor(&mut w, 1);
        let third = editor(&mut w, 0);
        w.editors.get_mut(third).unwrap().mark_clean();
        w.tabs.activate(second);
        let text_a = "select 1;\r\n\tselect \"x\" -- \u{1F600}\r";
        let saved = state_of(&w, &[(first, text_a), (second, "select 2"), (third, "")]);
        assert_eq!(saved.tabs.len(), 3);
        assert_eq!(saved.active, Some(1));

        let mut fresh = Workspace::new(sidebar());
        let effect = fresh.restore_tabs(&SessionLoad::Loaded(saved.clone()));
        let ids: Vec<TabId> = fresh.tabs().iter().map(crate::tabs::Tab::id).collect();
        assert_eq!(ids.len(), 3);
        // Same order, seeds, dirty flags, connections; no session yet.
        let doc = |i: usize| fresh.editor_doc(ids[i]).unwrap();
        assert_eq!(doc(0).seed(), Some(text_a));
        assert_eq!(doc(1).seed(), Some("select 2"));
        assert!(doc(0).is_dirty() && doc(1).is_dirty() && !doc(2).is_dirty());
        assert_eq!(fresh.tab_conns[&ids[0]], fresh.tab_conns[&ids[2]]);
        assert_ne!(fresh.tab_conns[&ids[0]], fresh.tab_conns[&ids[1]]);
        assert!(ids.iter().all(|t| !fresh.has_session(*t)));
        assert_eq!(
            fresh.tabs().active().map(crate::tabs::Tab::id),
            Some(ids[1])
        );
        // The state read back from the restored workspace is the saved one.
        let again = fresh.session_state(&|_| None).unwrap();
        assert_eq!(again, saved);
        // Only the active tab connects.
        let r = request(effect);
        assert_eq!(
            fresh.requests.get(&r),
            Some(&Purpose::Attach {
                tab: ids[1],
                conn: fresh.tab_conns[&ids[1]].clone()
            })
        );
        let (session, _) = FakeSession::boxed();
        fresh.connect_finished(r, Ok(session));
        assert!(fresh.has_session(ids[1]));
        assert!(!fresh.has_session(ids[0]));
        assert_eq!(fresh.tabs().len(), 3);
    }

    #[test]
    fn another_tab_attaches_when_it_is_activated_and_a_failure_does_not_loop() {
        let mut w = Workspace::new(sidebar());
        let a = editor(&mut w, 0);
        let b = editor(&mut w, 1);
        let saved = state_of(&w, &[(a, "a"), (b, "b")]);
        let mut fresh = Workspace::new(sidebar());
        let first = fresh.restore_tabs(&SessionLoad::Loaded(saved));
        let ids: Vec<TabId> = fresh.tabs().iter().map(crate::tabs::Tab::id).collect();
        // The active tab is the last one.
        let r = request(first);
        let failure = ConnectFailure::MissingSecret {
            kind: SecretKind::DatabasePassword,
        };
        fresh.connect_finished(r, Err(failure));
        assert!(!fresh.has_session(ids[1]));
        // Nothing retries on its own.
        assert!(matches!(fresh.attach_active_restored(), Effect::None));
        // Activating the other tab attaches it.
        fresh.dispatch(WorkspaceAction::ActivateTab(ids[0]));
        let r = request(fresh.attach_active_restored());
        let (session, _) = FakeSession::boxed();
        fresh.connect_finished(r, Ok(session));
        assert!(fresh.has_session(ids[0]));
        // Activating the failed one again tries again.
        fresh.dispatch(WorkspaceAction::ActivateTab(ids[1]));
        assert!(matches!(
            fresh.attach_active_restored(),
            Effect::Connect { .. }
        ));
    }

    #[test]
    fn a_session_for_a_closed_tab_is_closed() {
        let mut w = Workspace::new(sidebar());
        let a = editor(&mut w, 0);
        let saved = state_of(&w, &[(a, "a")]);
        let mut fresh = Workspace::new(sidebar());
        let r = request(fresh.restore_tabs(&SessionLoad::Loaded(saved)));
        let tab = fresh.tabs().active().map(crate::tabs::Tab::id).unwrap();
        fresh.close_tab_now(tab);
        let (session, _) = FakeSession::boxed();
        let effect = fresh.connect_finished(r, Ok(session));
        assert!(matches!(effect, Effect::CloseSession(_)));
    }

    #[test]
    fn an_unreadable_connection_store_keeps_the_saved_ids_and_stays_quiet() {
        let state = SessionState {
            tabs: vec![
                SessionTab {
                    connection: Some("one".to_owned()),
                    driver: "sqlite".to_owned(),
                    text: "select 1".to_owned(),
                    dirty: true,
                },
                SessionTab {
                    connection: None,
                    driver: "sqlite".to_owned(),
                    text: "select 2".to_owned(),
                    dirty: false,
                },
            ],
            ..SessionState::default()
        };
        let mut w = Workspace::new(SidebarModel::with_load_error("bad file".into()));
        w.restore_tabs(&SessionLoad::Loaded(state.clone()));
        assert!(w.take_notice().is_none());
        let again = w.session_state(&no_text).unwrap();
        assert_eq!(again.tabs[0].connection.as_deref(), Some("one"));
        assert_eq!(again.tabs[1].connection, None);
        assert_eq!(again.tabs[0].text, "select 1");
    }

    #[test]
    fn a_tab_that_never_had_a_connection_gives_no_notice() {
        let state = SessionState {
            tabs: vec![SessionTab {
                connection: None,
                driver: "sqlite".to_owned(),
                text: "select 1".to_owned(),
                dirty: false,
            }],
            ..SessionState::default()
        };
        let mut w = Workspace::new(sidebar());
        w.restore_tabs(&SessionLoad::Loaded(state));
        assert!(w.take_notice().is_none());
    }

    #[test]
    fn an_unknown_host_key_on_attach_is_asked_and_the_trust_reattaches() {
        use crate::connections::{Dialog, HostKeyPrompt};
        let mut w = Workspace::new(sidebar());
        let a = editor(&mut w, 0);
        let saved = state_of(&w, &[(a, "a")]);
        let mut fresh = Workspace::new(sidebar());
        let r = request(fresh.restore_tabs(&SessionLoad::Loaded(saved)));
        let tab = fresh.tabs().active().map(crate::tabs::Tab::id).unwrap();
        let prompt = HostKeyPrompt {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            public_key: "ssh-ed25519 AAAA".into(),
        };
        fresh.connect_finished(r, Err(ConnectFailure::UnknownHostKey(prompt)));
        let Some(Dialog::TrustHostKey { retry, .. }) = fresh.dialog() else {
            panic!("expected the host-key prompt");
        };
        let retry = retry.clone();
        // Trusting connects the same tab again.
        let again = request(fresh.trust_finished(retry.clone(), Ok(())));
        assert_eq!(
            fresh.requests.get(&again),
            Some(&Purpose::Attach {
                tab,
                conn: fresh.tab_conns[&tab].clone()
            })
        );
        // A failed trust gives a notice and connects nothing.
        let effect = fresh.trust_finished(
            retry,
            Err(ConnectFailure::MissingSecret {
                kind: SecretKind::DatabasePassword,
            }),
        );
        assert!(matches!(effect, Effect::None));
        assert!(fresh.take_notice().is_some());
    }

    #[test]
    fn a_second_attach_waits_for_the_one_in_flight() {
        let mut w = Workspace::new(sidebar());
        let a = editor(&mut w, 0);
        let b = editor(&mut w, 1);
        let saved = state_of(&w, &[(a, "a"), (b, "b")]);
        let mut fresh = Workspace::new(sidebar());
        let first = request(fresh.restore_tabs(&SessionLoad::Loaded(saved)));
        let ids: Vec<TabId> = fresh.tabs().iter().map(crate::tabs::Tab::id).collect();
        // Away and back while the connect is still running: no second connect.
        fresh.dispatch(WorkspaceAction::ActivateTab(ids[0]));
        let other = request(fresh.attach_active_restored());
        fresh.dispatch(WorkspaceAction::ActivateTab(ids[1]));
        assert!(matches!(fresh.attach_active_restored(), Effect::None));
        let (session, _) = FakeSession::boxed();
        fresh.connect_finished(first, Ok(session));
        assert!(fresh.has_session(ids[1]));
        let (session, _) = FakeSession::boxed();
        fresh.connect_finished(other, Ok(session));
        assert!(fresh.has_session(ids[0]));
    }

    /// Startup cost of the restore, headless part (read + parse `session.json`, rebuild the
    /// tabs). Run: `cargo test -p sqler-ui --lib startup_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn startup_cost_of_a_restore() {
        for (tabs, bytes) in [(10_usize, 2_000_usize), (50, 100_000)] {
            let dir =
                std::env::temp_dir().join(format!("sqler-f1-cost-{tabs}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let config = sqler_config::ConfigDir::new(&dir);
            let state = SessionState {
                tabs: (0..tabs)
                    .map(|i| SessionTab {
                        connection: Some(if i % 2 == 0 { "one" } else { "two" }.to_owned()),
                        driver: "sqlite".to_owned(),
                        text: "select 1;\n".repeat(bytes / 10),
                        dirty: true,
                    })
                    .collect(),
                active: Some(0),
                ..SessionState::default()
            };
            let mut w = Workspace::new(sidebar());
            config.save_session_state(&state).unwrap();
            let start = std::time::Instant::now();
            let load = config.load_session_state();
            let loaded = start.elapsed();
            w.restore_tabs(&load);
            let total = start.elapsed();
            eprintln!("{tabs} tabs x {bytes} B: load {loaded:?}, load + restore {total:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_missing_connection_keeps_the_text_and_says_so() {
        let state = SessionState {
            tabs: vec![
                SessionTab {
                    connection: Some("gone".to_owned()),
                    driver: "postgres".to_owned(),
                    text: "select 42".to_owned(),
                    dirty: true,
                },
                SessionTab {
                    connection: None,
                    driver: "!bad driver!".to_owned(),
                    text: String::new(),
                    dirty: false,
                },
            ],
            active: Some(0),
            ..SessionState::default()
        };
        let mut w = Workspace::new(sidebar());
        let effect = w.restore_tabs(&SessionLoad::Loaded(state));
        assert!(matches!(effect, Effect::None));
        let ids: Vec<TabId> = w.tabs().iter().map(crate::tabs::Tab::id).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(w.editor_doc(ids[0]).unwrap().seed(), Some("select 42"));
        assert!(w.tab_conns.is_empty());
        assert_eq!(
            w.tabs().get(ids[0]).unwrap().connection().name,
            "No connection"
        );
        assert!(
            w.take_notice()
                .unwrap()
                .starts_with("1 restored tab(s) lost their connection")
        );
        // Saving it again keeps the text, the driver and the saved id (the tab that never had a
        // connection stays without one).
        let again = w.session_state(&no_text).unwrap();
        assert_eq!(again.tabs[0].connection.as_deref(), Some("gone"));
        assert_eq!(again.tabs[1].connection, None);
        assert_eq!(again.tabs[0].driver, "postgres");
        assert_eq!(again.tabs[0].text, "select 42");
    }

    #[test]
    fn a_newer_file_restores_nothing_and_is_not_saved() {
        let mut w = Workspace::new(sidebar());
        assert!(matches!(
            w.restore_tabs(&SessionLoad::Newer(9)),
            Effect::None
        ));
        assert!(w.tabs().is_empty());
        assert!(w.take_notice().is_none());
        assert!(w.session_state(&no_text).is_none());
    }

    #[test]
    fn an_unreadable_file_is_a_notice_and_a_missing_one_is_silent() {
        let mut w = Workspace::new(sidebar());
        w.restore_tabs(&SessionLoad::Missing);
        assert!(w.take_notice().is_none());
        w.restore_tabs(&SessionLoad::Invalid(
            "syntax error at line 1, column 2".into(),
        ));
        let notice = w.take_notice().unwrap();
        assert!(notice.starts_with("Could not read session.json, the open tabs were not restored"));
        assert!(notice.contains("line 1, column 2"));
        assert!(w.tabs().is_empty());
        assert!(w.session_state(&no_text).is_some());
    }

    #[test]
    fn the_chat_draft_is_pending_and_lands_in_the_chat_started_later() {
        let spec = ConnectionSpec::new("db", DriverId::new("sqlite").unwrap(), ":memory:")
            .with_ai(ConnectionAi::new(true));
        let mut sb = SidebarModel::default();
        sb.add_connection(spec, None).unwrap();
        let mut config = AiConfig::default();
        let id = ProviderId::new("local").unwrap();
        config
            .providers
            .push(ProviderConfig::new(id.clone(), ProviderKind::Ollama).with_models(["m"]));
        config.models.exploration = Some(ModelChoice::new(id, "m"));
        let mut w = Workspace::new(sb)
            .with_ai(AiState::new(config, vec![ProviderKind::Ollama], None))
            .with_ai_audit(Some(AiAudit::in_memory()));
        let state = SessionState {
            chat_draft: "which customers\nare late?".to_owned(),
            ..SessionState::default()
        };
        w.restore_tabs(&SessionLoad::Loaded(state));
        assert_eq!(
            w.session_state(&no_text).unwrap().chat_draft,
            "which customers\nare late?"
        );
        assert!(w.chat().is_none());
        // Opening a connection and showing the chat starts one.
        let r = request(w.dispatch(WorkspaceAction::OpenConnection(0)));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(r, Ok(session));
        w.dispatch_ai(AiAction::ToggleChat);
        assert_eq!(w.chat().unwrap().draft(), "which customers\nare late?");
        assert!(w.pending_chat_draft.is_none());
        // Nothing was sent.
        assert_eq!(w.chat().unwrap().entries().len(), 0);
    }

    #[test]
    fn the_saved_state_holds_no_secret_or_connection_detail() {
        let config = ConnectionConfig::new(
            "payments",
            DriverId::new("sqlite").unwrap(),
            "/tmp/alice-hunter2-pw-db.internal.example.sqlite",
        );
        let mut w = Workspace::new(SidebarModel::from_connections(vec![config]));
        let r = request(w.dispatch(WorkspaceAction::OpenConnection(0)));
        let (session, _) = FakeSession::boxed();
        w.connect_finished(r, Ok(session));
        let state = w.session_state(&|_| Some("select 1".to_owned())).unwrap();
        assert_eq!(state.tabs.len(), 1);
        let dump = format!("{state:?}");
        for secret in ["hunter2-pw", "db.internal.example", "alice"] {
            assert!(!dump.contains(secret), "{secret} leaked");
        }
        assert!(state.tabs[0].connection.is_some());
    }
}
