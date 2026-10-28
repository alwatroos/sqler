// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The saved chats of the chat panel (ADR-0077): what the workspace writes into the store, the
//! list of saved chats and the actions on it.
//!
//! Only the chat panel (`TabId::CHAT_PANEL`) is saved. The store (`sqler_config::ChatHistory`)
//! takes writes without blocking, so [`Workspace::persist_chat`] runs on the UI thread. Reading
//! blocks, so the view runs [`Effect::ListChats`] and [`Effect::LoadChat`] on the background
//! executor and hands the answers back to [`Workspace::chat_history_listed`] and
//! [`Workspace::chat_loaded`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqler_ai_tools::session::stored_entry;
use sqler_config::{
    ChatHistory, ChatHistoryError, ConnectionId, ConversationId, ConversationSummary,
    StoredConversation,
};

use super::state::{Effect, Workspace};
use crate::ai::AiAction;
use crate::ai::chat::{ChatRow, HISTORY_OFF_NO_DIR, HISTORY_UNTITLED, REMOVED_CONNECTION};
use crate::history::age;
use crate::tabs::TabId;

/// The list of saved chats shown in the chat panel.
#[derive(Debug, Default)]
pub struct ChatList {
    open: bool,
    loading: bool,
    rows: Vec<ConversationSummary>,
    error: Option<String>,
    confirm: bool,
}

impl ChatList {
    /// Whether the list replaces the transcript.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the rows are still being read.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Why there are no rows (the store is off), if it is.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the "delete all" confirmation is armed.
    #[must_use]
    pub fn confirming(&self) -> bool {
        self.confirm
    }

    /// The saved chats, newest first.
    #[must_use]
    pub fn rows(&self) -> &[ConversationSummary] {
        &self.rows
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Workspace {
    /// Attaches the chat history store. Without one, nothing is saved and the list says why.
    #[must_use]
    pub fn with_chat_history(mut self, store: ChatHistory) -> Self {
        self.chat_store = Some(store);
        self
    }

    /// The chat history store, if there is a configuration directory.
    #[must_use]
    pub fn chat_history(&self) -> Option<&ChatHistory> {
        self.chat_store.as_ref()
    }

    /// The list of saved chats.
    #[must_use]
    pub fn chat_list(&self) -> &ChatList {
        &self.chat_list
    }

    /// The saved chat that is on screen, once it has an entry.
    fn current_chat_id(&self) -> Option<&ConversationId> {
        self.chat().and_then(|t| t.history.as_ref())
    }

    /// The newest saved chat that is not the one on screen: what the palette commands to open
    /// or delete a past chat act on while the list is open.
    #[must_use]
    pub fn next_past_chat(&self) -> Option<ConversationId> {
        if !self.chat_list.open {
            return None;
        }
        let current = self.current_chat_id();
        self.chat_list
            .rows
            .iter()
            .map(|r| &r.id)
            .find(|id| Some(*id) != current)
            .cloned()
    }

    /// The lines of the list: the first message, the connection and the age.
    #[must_use]
    pub fn chat_rows(&self) -> Vec<ChatRow> {
        let now = now_ms();
        let current = self.current_chat_id();
        self.chat_list
            .rows
            .iter()
            .map(|row| {
                let name = self
                    .sidebar
                    .entries()
                    .iter()
                    .find(|e| e.id().as_str() == row.connection)
                    .map_or(REMOVED_CONNECTION, |e| e.config().name.as_str());
                let elapsed = u64::try_from(now.saturating_sub(row.updated_ms)).unwrap_or(0);
                let title = if row.title.is_empty() {
                    HISTORY_UNTITLED.to_owned()
                } else {
                    row.title.clone()
                };
                ChatRow {
                    id: row.id.clone(),
                    title,
                    line: format!("{name} \u{b7} {}", age(Duration::from_millis(elapsed))),
                    current: current == Some(&row.id),
                }
            })
            .collect()
    }

    /// Saves what the chat panel gained since the last call. Every entry with a stable number
    /// from `persisted` on is written through `stored_entry`; when the tab dropped its oldest
    /// entries, the stored ones go too.
    pub(super) fn persist_chat(&mut self) {
        let Some(store) = self.chat_store.clone() else {
            return;
        };
        let Some(t) = self.ai_sessions.tabs.get_mut(&TabId::CHAT_PANEL) else {
            return;
        };
        if t.entries.is_empty() && t.history.is_none() {
            return;
        }
        let id = t.history.get_or_insert_with(ChatHistory::new_id).clone();
        if t.dropped > t.trimmed {
            store.trim_before(&id, as_seq(t.dropped));
            t.trimmed = t.dropped;
        }
        let from = t.persisted.max(t.dropped);
        let batch: Vec<_> = t
            .entries
            .iter()
            .enumerate()
            .skip(from - t.dropped)
            .filter_map(|(i, entry)| Some((as_seq(t.dropped + i), stored_entry(entry)?)))
            .collect();
        if let Some(conn) = &t.conn {
            store.append(&id, conn.as_str(), batch);
        }
        t.persisted = t.dropped + t.entries.len();
    }

    /// The chat on screen is no longer kept in the store (it was deleted there): what it gains
    /// from now on is saved as a new chat.
    fn detach_chat_history(&mut self) {
        if let Some(t) = self.ai_sessions.tabs.get_mut(&TabId::CHAT_PANEL) {
            t.history = None;
            t.persisted = t.dropped + t.entries.len();
            t.trimmed = t.dropped;
        }
    }

    /// Runs an action on the list of saved chats.
    pub(super) fn dispatch_chat_history(&mut self, action: AiAction) -> Effect {
        match action {
            AiAction::ToggleChatHistory => {
                if self.chat_list.open {
                    self.chat_list = ChatList::default();
                    Effect::None
                } else {
                    self.open_chat_list(false)
                }
            }
            AiAction::ClearChatHistory => {
                if self.chat_list.open {
                    self.chat_list.confirm = true;
                    Effect::None
                } else {
                    self.open_chat_list(true)
                }
            }
            AiAction::CancelClearChatHistory => {
                self.chat_list.confirm = false;
                Effect::None
            }
            AiAction::ConfirmClearChatHistory => {
                if !self.chat_list.confirm {
                    return Effect::None;
                }
                if let Some(store) = &self.chat_store {
                    store.clear();
                }
                self.chat_list.confirm = false;
                self.chat_list.rows.clear();
                self.detach_chat_history();
                Effect::None
            }
            AiAction::DeletePastChat(id) => {
                if let Some(store) = &self.chat_store {
                    store.delete(&id);
                }
                self.chat_list.rows.retain(|r| r.id != id);
                if self.current_chat_id() == Some(&id) {
                    self.detach_chat_history();
                }
                Effect::None
            }
            AiAction::OpenPastChat(id) => {
                if self.current_chat_id() == Some(&id) {
                    self.chat_list = ChatList::default();
                    return Effect::None;
                }
                Effect::LoadChat(id)
            }
            _ => Effect::None,
        }
    }

    /// Opens the list and asks for its rows; `confirm` arms the "delete all" confirmation.
    fn open_chat_list(&mut self, confirm: bool) -> Effect {
        self.layout.reveal_chat();
        self.chat_list = ChatList {
            open: true,
            loading: self.chat_store.is_some(),
            confirm,
            error: self
                .chat_store
                .is_none()
                .then(|| HISTORY_OFF_NO_DIR.to_owned()),
            rows: Vec::new(),
        };
        if self.chat_store.is_some() {
            Effect::ListChats
        } else {
            Effect::None
        }
    }

    /// The rows were read (or the store is off). Ignored when the list was closed meanwhile.
    pub fn chat_history_listed(
        &mut self,
        result: Result<Vec<ConversationSummary>, ChatHistoryError>,
    ) {
        if !self.chat_list.open {
            return;
        }
        self.chat_list.loading = false;
        match result {
            Ok(rows) => {
                self.chat_list.rows = rows;
                self.chat_list.error = None;
            }
            Err(err) => {
                self.chat_list.rows.clear();
                self.chat_list.error = Some(err.to_string());
            }
        }
    }

    /// A saved chat was read. It replaces the chat on screen: the current chat is closed, and
    /// the saved one is started with its transcript as context (nothing is sent), or, when it
    /// cannot start, shown read-only with the reason.
    pub fn chat_loaded(
        &mut self,
        id: ConversationId,
        result: Result<Option<StoredConversation>, ChatHistoryError>,
    ) -> Effect {
        match result {
            Err(err) => {
                self.notice = Some(err.to_string());
                Effect::None
            }
            Ok(None) => {
                self.notice = Some("That chat is no longer saved.".to_owned());
                self.chat_list.rows.retain(|r| r.id != id);
                Effect::None
            }
            Ok(Some(conversation)) => {
                self.chat_list = ChatList::default();
                self.restore_chat(id, conversation)
            }
        }
    }

    /// The saved connection with this stored id, if it still exists.
    pub(super) fn saved_connection(&self, stored: &str) -> Option<ConnectionId> {
        self.sidebar
            .entries()
            .iter()
            .find(|e| e.id().as_str() == stored)
            .map(|e| e.id().clone())
    }

    /// Opens the list with synthetic rows (the screenshot scene).
    #[cfg(feature = "screenshots")]
    pub(super) fn chat_history_fixture(&mut self, rows: Vec<ConversationSummary>) {
        self.layout.reveal_chat();
        self.chat_list = ChatList {
            open: true,
            ..ChatList::default()
        };
        self.chat_history_listed(Ok(rows));
    }
}

fn as_seq(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use sqler_ai_api::mock::ScriptedProvider;
    use sqler_ai_api::{AiError, AiProvider, Capabilities, ChatEvent, StopReason, Usage};
    use sqler_ai_tools::session::{SessionEvent, TranscriptEntry};
    use sqler_config::{
        AiConfig, ChatHistory, ConnectionAi, ConnectionSpec, ModelChoice, ProviderConfig,
        ProviderId, ProviderKind, Secret, StoredEntry,
    };
    use sqler_driver_api::sqler_core::DriverId;

    use super::*;
    use crate::ai::chat::{CHAT_CONNECTION_REMOVED, RESTORED_SQL_HIDDEN};
    use crate::ai::session::{HostMsg, SessionPhase, build_session};
    use crate::ai::{AiAudit, AiProviderFactory, AiState};
    use crate::sidebar::SidebarModel;
    use crate::workspace::testing::FakeConnector;

    const WAIT: Duration = Duration::from_secs(10);

    /// A scratch directory that goes away with the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "sqler-ui-chat-history-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn store(&self) -> ChatHistory {
            ChatHistory::open(self.0.join("chat-history.db"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fixed(Arc<dyn AiProvider>);

    impl AiProviderFactory for Fixed {
        fn kinds(&self) -> Vec<ProviderKind> {
            vec![ProviderKind::Ollama]
        }

        fn build(
            &self,
            _provider: &ProviderConfig,
            _key: Option<&Secret>,
        ) -> Result<Arc<dyn AiProvider>, AiError> {
            Ok(self.0.clone())
        }
    }

    fn ai_state() -> AiState {
        let mut config = AiConfig::default();
        let id = ProviderId::new("local").unwrap();
        config
            .providers
            .push(ProviderConfig::new(id.clone(), ProviderKind::Ollama).with_models(["m"]));
        config.models.exploration = Some(ModelChoice::new(id, "m"));
        AiState::new(config, vec![ProviderKind::Ollama], None)
    }

    fn provider() -> Arc<ScriptedProvider> {
        Arc::new(
            ScriptedProvider::new("mock")
                .with_capabilities(Capabilities::new(true, false, false, None)),
        )
    }

    /// A workspace with one connection called `db` (AI on) and the store of `scratch`.
    fn workspace(scratch: &Scratch) -> Workspace {
        workspace_with_ai(scratch, true)
    }

    fn workspace_with_ai(scratch: &Scratch, ai_on: bool) -> Workspace {
        let mut sidebar = SidebarModel::default();
        let spec = ConnectionSpec::new("db", DriverId::new("sqlite").unwrap(), ":memory:")
            .with_ai(ConnectionAi::new(ai_on));
        sidebar.add_connection(spec, None).unwrap();
        Workspace::new(sidebar)
            .with_ai(ai_state())
            .with_ai_audit(Some(AiAudit::in_memory()))
            .with_chat_history(scratch.store())
    }

    fn user(text: &str) -> TranscriptEntry {
        TranscriptEntry::User {
            text: text.to_owned(),
        }
    }

    fn assistant(text: &str) -> TranscriptEntry {
        TranscriptEntry::Assistant {
            text: text.to_owned(),
        }
    }

    fn say(w: &mut Workspace, entry: TranscriptEntry) {
        let effect = w.ai_session_message(
            TabId::CHAT_PANEL,
            HostMsg::Event(SessionEvent::Entry(entry)),
        );
        assert!(matches!(effect, Effect::None));
    }

    /// The session start inside an effect (the connection is opened first, in a batch).
    fn session_start(
        effect: Effect,
    ) -> Option<(
        Box<crate::ai::session::AiSessionStart>,
        tokio::sync::mpsc::UnboundedReceiver<HostMsg>,
    )> {
        match effect {
            Effect::StartAiSession { start, host } => Some((start, host)),
            Effect::Batch(effects) => effects.into_iter().find_map(session_start),
            _ => None,
        }
    }

    fn start_chat(w: &mut Workspace) {
        let effect = w.dispatch_ai(AiAction::NewChat);
        assert!(session_start(effect).is_some());
    }

    fn saved(w: &Workspace) -> Vec<ConversationSummary> {
        let store = w.chat_history().unwrap();
        assert!(store.flush(WAIT));
        store.list().unwrap()
    }

    #[test]
    fn entries_reach_the_store_and_only_once() {
        let scratch = Scratch::new("persist");
        let mut w = workspace(&scratch);
        start_chat(&mut w);
        assert!(saved(&w).is_empty(), "nothing to save yet");
        say(&mut w, user("how many customers?"));
        say(&mut w, assistant("There are 8."));
        let rows = saved(&w);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entries, 2);
        assert_eq!(rows[0].connection, "db");
        assert_eq!(rows[0].title, "how many customers?");
        assert_eq!(w.chat().unwrap().history_id(), Some(&rows[0].id));
        // A later entry is added to the same chat, not saved as a new one.
        say(&mut w, user("and orders?"));
        let rows = saved(&w);
        assert_eq!((rows.len(), rows[0].entries), (1, 3));
    }

    #[test]
    fn a_new_chat_is_a_new_conversation() {
        let scratch = Scratch::new("new");
        let mut w = workspace(&scratch);
        start_chat(&mut w);
        say(&mut w, user("first"));
        start_chat(&mut w);
        say(&mut w, user("second"));
        let rows = saved(&w);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "second");
    }

    #[test]
    fn the_stored_chat_is_trimmed_with_the_tab() {
        let scratch = Scratch::new("trim");
        let mut w = workspace(&scratch);
        start_chat(&mut w);
        for n in 0..505 {
            say(&mut w, user(&format!("question {n}")));
        }
        let chat = w.chat().unwrap();
        assert_eq!((chat.dropped(), chat.entries().len()), (5, 500));
        let id = chat.history_id().unwrap().clone();
        let loaded = w.chat_history().unwrap().load(&id).unwrap().unwrap();
        assert_eq!(loaded.first_seq, 5);
        assert_eq!(loaded.entries.len(), 500);
        assert_eq!(
            loaded.entries[0],
            StoredEntry::User {
                text: "question 5".to_owned()
            }
        );
    }

    /// Stores a two-entry chat about `connection` and returns its id.
    fn store_chat(w: &Workspace, connection: &str, entries: Vec<StoredEntry>) -> ConversationId {
        let store = w.chat_history().unwrap();
        let id = ChatHistory::new_id();
        let numbered = entries.into_iter().enumerate().map(|(i, e)| (i as u64, e));
        store.append(&id, connection, numbered.collect());
        assert!(store.flush(WAIT));
        id
    }

    fn two_entries() -> Vec<StoredEntry> {
        vec![
            StoredEntry::User {
                text: "which table has the most rows?".to_owned(),
            },
            StoredEntry::Assistant {
                text: "orders has the most.".to_owned(),
            },
        ]
    }

    fn open(w: &mut Workspace, id: &ConversationId) -> Effect {
        let effect = w.dispatch_ai(AiAction::OpenPastChat(id.clone()));
        assert!(matches!(effect, Effect::LoadChat(_)), "{effect:?}");
        let loaded = w.chat_history().unwrap().load(id);
        w.chat_loaded(id.clone(), loaded)
    }

    #[test]
    fn opening_a_chat_of_a_known_connection_starts_it_with_the_prior_transcript() {
        let scratch = Scratch::new("open");
        let mut w = workspace(&scratch);
        let id = store_chat(&w, "db", two_entries());
        let effect = open(&mut w, &id);
        let (start, _host) = session_start(effect).expect("a StartAiSession");
        assert_eq!(start.prior.as_ref().map(|p| p.entries.len()), Some(2));
        let chat = w.chat().unwrap();
        assert_eq!(chat.entries().len(), 2);
        assert_eq!(chat.history_id(), Some(&id));
        assert!(chat.is_restored());
        assert_eq!(*chat.phase(), SessionPhase::Starting);
        assert!(w.layout().chat_visible());
        assert!(!w.chat_list().is_open());
        // What was restored is not saved again.
        w.persist_chat();
        let rows = saved(&w);
        assert_eq!((rows.len(), rows[0].entries), (1, 2));
    }

    #[test]
    fn opening_a_chat_of_a_removed_connection_shows_it_without_starting() {
        let scratch = Scratch::new("removed");
        let mut w = workspace(&scratch);
        let id = store_chat(&w, "gone", two_entries());
        let effect = open(&mut w, &id);
        assert!(matches!(effect, Effect::None), "{effect:?}");
        let chat = w.chat().unwrap();
        assert_eq!(chat.entries().len(), 2);
        assert_eq!(
            *chat.phase(),
            SessionPhase::Failed(CHAT_CONNECTION_REMOVED.to_owned())
        );
        assert!(chat.conn().is_none());
        // Sending is refused and nothing can be opened in an editor.
        w.dispatch_ai(AiAction::SetChatDraft("hello".into()));
        let effect = w.dispatch_ai(AiAction::SendChat);
        assert!(matches!(effect, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(CHAT_CONNECTION_REMOVED));
        let open = w.dispatch_ai(AiAction::OpenChatSql { entry: 0, block: 0 });
        assert!(matches!(open, Effect::None));
    }

    #[test]
    fn opening_a_chat_whose_ai_is_off_shows_it_with_the_reason() {
        let scratch = Scratch::new("ai-off");
        let mut w = workspace_with_ai(&scratch, false);
        let id = store_chat(&w, "db", two_entries());
        let effect = open(&mut w, &id);
        assert!(session_start(effect).is_none());
        let chat = w.chat().unwrap();
        assert_eq!(chat.entries().len(), 2);
        assert!(matches!(chat.phase(), SessionPhase::Failed(_)));
        assert!(chat.conn().is_some());
    }

    #[test]
    fn a_chat_that_is_gone_or_unreadable_changes_nothing_but_the_notice() {
        let scratch = Scratch::new("gone");
        let mut w = workspace(&scratch);
        let id = ChatHistory::new_id();
        let effect = w.chat_loaded(id.clone(), Ok(None));
        assert!(matches!(effect, Effect::None));
        assert!(w.take_notice().is_some());
        let effect = w.chat_loaded(id, Err(ChatHistoryError::Timeout));
        assert!(matches!(effect, Effect::None));
        assert!(w.take_notice().is_some());
        assert!(w.chat().is_none());
    }

    #[test]
    fn restored_statements_are_copied_from_the_display_text_unless_values_are_hidden() {
        let scratch = Scratch::new("sql");
        let mut w = workspace(&scratch);
        let sql = |display: &str| StoredEntry::Sql {
            call_id: "c".to_owned(),
            connection: "db".to_owned(),
            display_sql: display.to_owned(),
            class: "read".to_owned(),
            approved: false,
            rows: 2,
            truncated: None,
            failed: false,
        };
        let id = store_chat(
            &w,
            "db",
            vec![
                sql("select * from customers"),
                sql("select * from users where password = '<redacted>'"),
            ],
        );
        open(&mut w, &id);
        let copy =
            |w: &mut Workspace, entry| w.dispatch_ai(AiAction::CopyChatSql { entry, block: 0 });
        match copy(&mut w, 0) {
            Effect::Copy(text) => assert_eq!(text, "select * from customers"),
            other => panic!("expected Copy, got {other:?}"),
        }
        assert!(matches!(copy(&mut w, 1), Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_SQL_HIDDEN));
        let open_hidden = w.dispatch_ai(AiAction::OpenChatSql { entry: 1, block: 0 });
        assert!(matches!(open_hidden, Effect::None));
        assert_eq!(w.take_notice().as_deref(), Some(RESTORED_SQL_HIDDEN));
    }

    #[test]
    fn the_list_opens_loads_and_closes() {
        let scratch = Scratch::new("list");
        let mut w = workspace(&scratch);
        let id = store_chat(&w, "db", two_entries());
        assert!(!w.chat_list().is_open());
        assert!(matches!(
            w.dispatch_ai(AiAction::ToggleChatHistory),
            Effect::ListChats
        ));
        assert!(w.chat_list().is_open() && w.chat_list().is_loading());
        let rows = w.chat_history().unwrap().list();
        w.chat_history_listed(rows);
        assert!(!w.chat_list().is_loading());
        let lines = w.chat_rows();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].id, id);
        assert_eq!(lines[0].title, "which table has the most rows?");
        assert!(lines[0].line.starts_with("db \u{b7} "), "{}", lines[0].line);
        assert_eq!(w.next_past_chat(), Some(id));
        assert!(matches!(
            w.dispatch_ai(AiAction::ToggleChatHistory),
            Effect::None
        ));
        assert!(!w.chat_list().is_open());
        assert_eq!(w.next_past_chat(), None);
    }

    #[test]
    fn without_a_store_the_list_says_why() {
        let mut w = Workspace::new(SidebarModel::default());
        assert!(matches!(
            w.dispatch_ai(AiAction::ToggleChatHistory),
            Effect::None
        ));
        assert_eq!(w.chat_list().error(), Some(HISTORY_OFF_NO_DIR));
        assert!(!w.chat_list().is_loading());
    }

    #[test]
    fn delete_removes_one_chat_and_the_current_one_stays_on_screen() {
        let scratch = Scratch::new("delete");
        let mut w = workspace(&scratch);
        start_chat(&mut w);
        say(&mut w, user("keep going"));
        let current = w.chat().unwrap().history_id().unwrap().clone();
        let other = store_chat(&w, "db", two_entries());
        w.dispatch_ai(AiAction::ToggleChatHistory);
        let rows = w.chat_history().unwrap().list();
        w.chat_history_listed(rows);
        assert_eq!(w.chat_rows().iter().filter(|r| r.current).count(), 1);
        // The palette never offers the current chat.
        assert_eq!(w.next_past_chat(), Some(other.clone()));
        w.dispatch_ai(AiAction::DeletePastChat(other.clone()));
        assert_eq!(w.chat_list().rows().len(), 1);
        assert_eq!(saved(&w).len(), 1);
        // Deleting the chat on screen keeps it on screen; what follows is a new conversation.
        w.dispatch_ai(AiAction::DeletePastChat(current));
        assert_eq!(saved(&w).len(), 0);
        assert_eq!(w.chat().unwrap().entries().len(), 1);
        assert!(w.chat().unwrap().history_id().is_none());
        say(&mut w, user("after the delete"));
        let rows = saved(&w);
        assert_eq!((rows.len(), rows[0].entries), (1, 1));
        assert_eq!(rows[0].title, "after the delete");
    }

    #[test]
    fn clear_asks_first_and_deletes_everything_only_when_confirmed() {
        let scratch = Scratch::new("clear");
        let mut w = workspace(&scratch);
        store_chat(&w, "db", two_entries());
        store_chat(&w, "db", two_entries());
        // The command opens the list with the question armed and deletes nothing.
        assert!(matches!(
            w.dispatch_ai(AiAction::ClearChatHistory),
            Effect::ListChats
        ));
        assert!(w.chat_list().confirming());
        assert_eq!(saved(&w).len(), 2);
        // Cancel disarms it.
        w.dispatch_ai(AiAction::CancelClearChatHistory);
        assert!(!w.chat_list().confirming());
        assert_eq!(saved(&w).len(), 2);
        // A confirm that was never asked for does nothing.
        w.dispatch_ai(AiAction::ConfirmClearChatHistory);
        assert_eq!(saved(&w).len(), 2);
        w.dispatch_ai(AiAction::ClearChatHistory);
        w.dispatch_ai(AiAction::ConfirmClearChatHistory);
        assert_eq!(saved(&w).len(), 0);
        assert!(!w.chat_list().confirming());
    }

    #[test]
    fn opening_the_current_chat_only_closes_the_list() {
        let scratch = Scratch::new("current");
        let mut w = workspace(&scratch);
        start_chat(&mut w);
        say(&mut w, user("hello"));
        let id = w.chat().unwrap().history_id().unwrap().clone();
        w.dispatch_ai(AiAction::ToggleChatHistory);
        let effect = w.dispatch_ai(AiAction::OpenPastChat(id));
        assert!(matches!(effect, Effect::None));
        assert!(!w.chat_list().is_open());
        assert_eq!(w.chat().unwrap().entries().len(), 1);
    }

    #[test]
    fn a_workspace_without_a_chat_writes_nothing() {
        let scratch = Scratch::new("nochat");
        let mut w = workspace(&scratch);
        w.persist_chat();
        assert_eq!(saved(&w).len(), 0);
    }

    /// AC9: the chat is saved in its own store; `session.json` holds what it held before.
    #[test]
    fn session_json_does_not_change_with_the_saved_chats() {
        let scratch = Scratch::new("session");
        let mut w = workspace(&scratch);
        let no_text = |_: TabId| None;
        let before = w.session_state(&no_text);
        start_chat(&mut w);
        say(&mut w, user("hello"));
        say(&mut w, assistant("hi"));
        w.dispatch_ai(AiAction::ToggleChatHistory);
        assert_eq!(w.session_state(&no_text), before);
    }

    /// Continuing a restored chat: the first request carries the saved conversation, the saved
    /// entries stay, and the new turn adds to the same stored chat.
    #[test]
    fn continuing_a_restored_chat_sends_a_new_turn_with_the_old_context() {
        let scratch = Scratch::new("continue");
        let mut w = workspace(&scratch);
        let id = store_chat(&w, "db", two_entries());
        let effect = open(&mut w, &id);
        let (start, mut host) = session_start(effect).expect("a StartAiSession");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let p = provider();
        p.push_script(vec![
            Ok(ChatEvent::TextDelta("Then customers.".to_owned())),
            Ok(ChatEvent::Usage(Usage::new(10, 5))),
            Ok(ChatEvent::Done {
                stop: StopReason::EndTurn,
            }),
        ]);
        let factory: Arc<dyn AiProviderFactory> = Arc::new(Fixed(p.clone()));
        let built = rt.block_on(build_session(
            *start,
            factory,
            Arc::new(FakeConnector::new()),
        ));
        assert!(matches!(
            w.ai_session_ready(TabId::CHAT_PANEL, built),
            Effect::None
        ));
        assert_eq!(*w.chat().unwrap().phase(), SessionPhase::Ready);
        // Nothing was sent by restoring.
        assert_eq!(p.requests().len(), 0);
        w.dispatch_ai(AiAction::SetChatDraft("and the next one?".into()));
        let Effect::RunAiTurn(job) = w.dispatch_ai(AiAction::SendChat) else {
            panic!("expected RunAiTurn");
        };
        rt.block_on(job.run());
        while w.chat().unwrap().is_running() {
            let msg = rt
                .block_on(async { tokio::time::timeout(WAIT, host.recv()).await })
                .expect("timed out")
                .expect("host channel closed");
            w.ai_session_message(TabId::CHAT_PANEL, msg);
        }
        let requests = p.requests();
        assert_eq!(requests.len(), 1);
        let sent = format!("{:?}", requests[0].messages);
        assert!(sent.contains("which table has the most rows?"), "{sent}");
        assert!(sent.contains("orders has the most."), "{sent}");
        assert!(sent.contains("and the next one?"), "{sent}");
        // The transcript continues: 2 restored + user + answer, in the same stored chat.
        let chat = w.chat().unwrap();
        assert_eq!(chat.entries().len(), 4);
        assert_eq!(chat.dropped(), 0);
        let rows = saved(&w);
        assert_eq!((rows.len(), rows[0].entries), (1, 4));
        assert_eq!(rows[0].id, id);
    }
}
