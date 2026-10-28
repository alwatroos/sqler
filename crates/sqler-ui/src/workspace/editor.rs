// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The SQL editor inside the workspace: opening editor tabs, and the commands that work on the
//! text of the active one.
//!
//! The text lives in the view. A command that needs it gets an [`EditorBuffer`] copied out of the
//! view; the answer is an [`Effect`] (an edit or a selection) that the view applies to its editor.
//! Query execution goes through the [`RunPort`] seam.

use sqler_config::ConnectionId;
use sqler_sql::FormatOptions;

use super::state::{Effect, Workspace};
use crate::browser::ObjectTabContent;
use crate::editor::{
    BracketIndex, EditorAction, EditorBuffer, EditorDoc, EditorKey, OccurrenceStep, RunMode,
    RunPort, format_edit, synthetic, toggle_block_comment, toggle_line_comment,
};
use crate::tabs::TabId;

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
