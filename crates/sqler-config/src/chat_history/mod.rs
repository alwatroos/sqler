// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI chat history store: `chat-history.db`, a private SQLite file (ADR-0077).
//!
//! [`ChatHistory::open`] starts one private OS thread that owns a current-thread runtime and one
//! connection. Writes are fire-and-forget and committed in batches at least once per second;
//! reads block for the answer, so a UI calls them off its main thread. The thread cuts and scrubs
//! every entry it stores, so every caller goes through the same choke point.

mod caps;
mod schema;
mod scrub;
mod types;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

pub use types::{
    CHAT_HISTORY_SCHEMA_VERSION, ChatHistoryError, ChatHistoryOptions, ChatHistoryStatus,
    ConversationId, ConversationSummary, MAX_STORED_CONVERSATIONS, MAX_STORED_ENTRY_BYTES,
    MAX_STORED_HISTORY_BYTES, MAX_STORED_ROWS, MAX_STORED_TEXT_BYTES, MAX_STORED_TITLE_CHARS,
    StoredConversation, StoredEntry, StoredNotice, StoredStatus,
};
use worker::{Cmd, State};

/// The longest a read waits for the thread.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

const CLOSED: &str = "Chat history is closed.";

struct Inner {
    path: PathBuf,
    tx: Mutex<Option<Sender<Cmd>>>,
    state: Arc<State>,
}

/// A handle on the chat history store. Cheap to clone; the thread ends when the last clone is
/// dropped or [`ChatHistory::close`] is called.
#[derive(Clone)]
pub struct ChatHistory {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ChatHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatHistory")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

impl ChatHistory {
    /// Opens the store at `path`. Never fails and never blocks: the file is opened on the
    /// store's own thread, and [`ChatHistory::status`] tells how it went.
    #[must_use]
    pub fn open(path: PathBuf) -> Self {
        Self::with_options(path, ChatHistoryOptions::default())
    }

    /// Like [`ChatHistory::open`], with the flush interval and the caps set (for tests).
    #[must_use]
    pub fn with_options(path: PathBuf, options: ChatHistoryOptions) -> Self {
        let state = Arc::new(State::new());
        let (tx, rx) = channel();
        let thread_state = Arc::clone(&state);
        let thread_path = path.clone();
        let spawned = std::thread::Builder::new()
            .name("sqler-chat-history".to_owned())
            .spawn(move || worker::run(&thread_path, &thread_state, &rx, options));
        let tx = if spawned.is_ok() {
            Some(tx)
        } else {
            state.set_status(ChatHistoryStatus::Off(
                "Chat history is off: its worker could not start.".to_owned(),
            ));
            None
        };
        Self {
            inner: Arc::new(Inner {
                path,
                tx: Mutex::new(tx),
                state,
            }),
        }
    }

    /// A new conversation id: `{unix_ms:x}-{pid:x}-{counter:x}`.
    #[must_use]
    pub fn new_id() -> ConversationId {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        ConversationId(format!(
            "{ms:x}-{:x}-{:x}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// The path of the database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Whether the store works.
    #[must_use]
    pub fn status(&self) -> ChatHistoryStatus {
        self.inner
            .state
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The one-time notice (an unreadable file was moved aside), once.
    #[must_use]
    pub fn take_notice(&self) -> Option<String> {
        self.inner
            .state
            .notice
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn send(&self, cmd: Cmd) -> bool {
        let guard = self.inner.tx.lock().unwrap_or_else(PoisonError::into_inner);
        guard.as_ref().is_some_and(|tx| tx.send(cmd).is_ok())
    }

    fn off_error(&self) -> ChatHistoryError {
        match self.status() {
            ChatHistoryStatus::Off(text) => ChatHistoryError::Off(text),
            _ => ChatHistoryError::Off(CLOSED.to_owned()),
        }
    }

    /// Queues entries for a conversation. `seq` is the stable number of the entry in its chat;
    /// storing a number again replaces the entry. Returns at once.
    pub fn append(&self, id: &ConversationId, connection: &str, entries: Vec<(u64, StoredEntry)>) {
        if entries.is_empty() {
            return;
        }
        self.send(Cmd::Append {
            id: id.clone(),
            connection: connection.to_owned(),
            entries,
        });
    }

    /// Queues the removal of the entries of `id` numbered below `first_kept`.
    pub fn trim_before(&self, id: &ConversationId, first_kept: u64) {
        self.send(Cmd::Trim {
            id: id.clone(),
            first_kept,
        });
    }

    /// Queues the removal of one conversation.
    pub fn delete(&self, id: &ConversationId) {
        self.send(Cmd::Delete(id.clone()));
    }

    /// Queues the removal of every conversation.
    pub fn clear(&self) {
        self.send(Cmd::Clear);
    }

    /// The conversations, newest first. Blocks for the answer, at most 5 s: call it off the UI
    /// thread.
    ///
    /// # Errors
    /// [`ChatHistoryError::Off`] when the store is off, [`ChatHistoryError::Timeout`] when the
    /// thread does not answer, [`ChatHistoryError::Database`] when the read fails.
    pub fn list(&self) -> Result<Vec<ConversationSummary>, ChatHistoryError> {
        if let ChatHistoryStatus::Off(text) = self.status() {
            return Err(ChatHistoryError::Off(text));
        }
        let (reply, answer) = channel();
        if !self.send(Cmd::List(reply)) {
            return Err(self.off_error());
        }
        answer
            .recv_timeout(READ_TIMEOUT)
            .map_err(|_| ChatHistoryError::Timeout)?
    }

    /// One conversation with its entries, or `None` if there is none. Blocks like
    /// [`ChatHistory::list`].
    ///
    /// # Errors
    /// As for [`ChatHistory::list`].
    pub fn load(
        &self,
        id: &ConversationId,
    ) -> Result<Option<StoredConversation>, ChatHistoryError> {
        if let ChatHistoryStatus::Off(text) = self.status() {
            return Err(ChatHistoryError::Off(text));
        }
        let (reply, answer) = channel();
        if !self.send(Cmd::Load(id.clone(), reply)) {
            return Err(self.off_error());
        }
        answer
            .recv_timeout(READ_TIMEOUT)
            .map_err(|_| ChatHistoryError::Timeout)?
    }

    /// Commits everything queued. `false` on a timeout, when the store is off or when the
    /// database refused the batch.
    #[must_use]
    pub fn flush(&self, timeout: Duration) -> bool {
        let (reply, answer) = channel();
        self.send(Cmd::Flush(reply)) && answer.recv_timeout(timeout).unwrap_or(false)
    }

    /// Commits, checkpoints and closes the store, waiting at most `timeout`. Later calls do
    /// nothing, and reads fail with [`ChatHistoryError::Off`].
    pub fn close(&self, timeout: Duration) {
        let (reply, answer) = channel();
        if self.send(Cmd::Close(reply)) {
            let _ = answer.recv_timeout(timeout);
        }
        *self.inner.tx.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
