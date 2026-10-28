// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The store's own thread: one current-thread runtime, one connection, batched commits.
//!
//! Every sqlx call stays inside `block_on` on this thread. Writes arrive as commands, are fitted
//! (cut and scrubbed) here, and are committed in one transaction when the deadline of the first
//! queued write passes, after 256 queued commands, on `Flush` and on `Close`.

use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use sqlx::{Connection, SqliteConnection};

use crate::chat_history::caps::fit_entry;
use crate::chat_history::schema::{self, Opened};
use crate::chat_history::types::{
    ChatHistoryError, ChatHistoryOptions, ChatHistoryStatus, ConversationId, ConversationSummary,
    MAX_STORED_TITLE_CHARS, StoredConversation, StoredEntry,
};

/// The most queued write commands before a commit is forced.
const MAX_QUEUED: usize = 256;

/// The notice when a batch could not be written: those entries are lost.
const WRITE_FAILED_NOTICE: &str =
    "Chat history could not save the latest messages; they will not be restored.";

/// What the thread shares with the handle.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) status: Mutex<ChatHistoryStatus>,
    pub(crate) notice: Mutex<Option<String>>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            status: Mutex::new(ChatHistoryStatus::Opening),
            notice: Mutex::new(None),
        }
    }

    pub(crate) fn set_status(&self, status: ChatHistoryStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

pub(crate) type ListReply = Sender<Result<Vec<ConversationSummary>, ChatHistoryError>>;
pub(crate) type LoadReply = Sender<Result<Option<StoredConversation>, ChatHistoryError>>;

/// A request to the thread.
pub(crate) enum Cmd {
    Append {
        id: ConversationId,
        connection: String,
        entries: Vec<(u64, StoredEntry)>,
    },
    Trim {
        id: ConversationId,
        first_kept: u64,
    },
    Delete(ConversationId),
    Clear,
    List(ListReply),
    Load(ConversationId, LoadReply),
    Flush(Sender<bool>),
    Close(Sender<()>),
}

impl Cmd {
    fn is_write(&self) -> bool {
        matches!(
            self,
            Self::Append { .. } | Self::Trim { .. } | Self::Delete(_) | Self::Clear
        )
    }
}

const READ_FAILED: ChatHistoryError =
    ChatHistoryError::Database("the chat history could not be read");

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The thread body.
pub(crate) fn run(
    path: &Path,
    state: &Arc<State>,
    rx: &Receiver<Cmd>,
    options: ChatHistoryOptions,
) {
    let off = |text: &str| {
        state.set_status(ChatHistoryStatus::Off(text.to_owned()));
        answer_off(rx, text);
    };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    else {
        off("Chat history is off: its worker could not start.");
        return;
    };
    match rt.block_on(schema::open(path)) {
        Opened::Off(text) => off(&text),
        Opened::Ready { conn, notice } => {
            *state.notice.lock().unwrap_or_else(PoisonError::into_inner) = notice;
            state.set_status(ChatHistoryStatus::Ready);
            let mut worker = Worker {
                rt,
                conn: Some(conn),
                options,
                pending: Vec::new(),
                deadline: None,
                last_ms: 0,
                errors: 0,
                state: Arc::clone(state),
            };
            worker.event_loop(rx);
        }
    }
}

/// Answers every request with the off text until the handle is closed or dropped.
fn answer_off(rx: &Receiver<Cmd>, text: &str) {
    for cmd in rx {
        match cmd {
            Cmd::List(reply) => {
                let _ = reply.send(Err(ChatHistoryError::Off(text.to_owned())));
            }
            Cmd::Load(_, reply) => {
                let _ = reply.send(Err(ChatHistoryError::Off(text.to_owned())));
            }
            Cmd::Flush(reply) => {
                let _ = reply.send(false);
            }
            Cmd::Close(reply) => {
                let _ = reply.send(());
                return;
            }
            _ => {}
        }
    }
}

struct Worker {
    rt: tokio::runtime::Runtime,
    conn: Option<SqliteConnection>,
    options: ChatHistoryOptions,
    pending: Vec<Cmd>,
    deadline: Option<Instant>,
    last_ms: i64,
    errors: u64,
    state: Arc<State>,
}

impl Worker {
    fn event_loop(&mut self, rx: &Receiver<Cmd>) {
        loop {
            let message = match self.deadline {
                Some(deadline) => {
                    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(cmd) if cmd.is_write() => {
                    self.pending.push(cmd);
                    let interval = self.options.flush_interval;
                    self.deadline
                        .get_or_insert_with(|| Instant::now() + interval);
                    if self.pending.len() >= MAX_QUEUED {
                        self.commit();
                    }
                }
                Ok(Cmd::List(reply)) => {
                    self.commit();
                    let _ = reply.send(self.list());
                }
                Ok(Cmd::Load(id, reply)) => {
                    self.commit();
                    let _ = reply.send(self.load(&id));
                }
                Ok(Cmd::Flush(reply)) => {
                    let ok = self.commit();
                    let _ = reply.send(ok);
                }
                Ok(Cmd::Close(reply)) => {
                    self.finish();
                    let _ = reply.send(());
                    return;
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    self.commit();
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.finish();
                    return;
                }
            }
        }
    }

    /// Commits what is queued, checkpoints and closes the connection.
    fn finish(&mut self) {
        self.commit();
        if let Some(mut conn) = self.conn.take() {
            self.rt.block_on(async {
                let _ = checkpoint(&mut conn).await;
                let _ = conn.close().await;
            });
        }
    }

    /// Commits the queue in one transaction. `false` if the database refused it.
    fn commit(&mut self) -> bool {
        self.deadline = None;
        if self.pending.is_empty() {
            return true;
        }
        let Some(conn) = self.conn.as_mut() else {
            self.pending.clear();
            return false;
        };
        let cmds = std::mem::take(&mut self.pending);
        let options = &self.options;
        let last_ms = &mut self.last_ms;
        let result = self.rt.block_on(async {
            let needs_checkpoint = apply(conn, cmds, options, last_ms).await?;
            if needs_checkpoint {
                checkpoint(conn).await?;
            }
            Ok::<(), sqlx::Error>(())
        });
        if result.is_err() {
            self.errors += 1;
            if self.errors == 1 {
                let mut notice = self
                    .state
                    .notice
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                notice.get_or_insert_with(|| WRITE_FAILED_NOTICE.to_owned());
            }
        }
        result.is_ok()
    }

    fn list(&mut self) -> Result<Vec<ConversationSummary>, ChatHistoryError> {
        let Some(conn) = self.conn.as_mut() else {
            return Err(READ_FAILED);
        };
        self.rt.block_on(read_list(conn)).map_err(|_| READ_FAILED)
    }

    fn load(
        &mut self,
        id: &ConversationId,
    ) -> Result<Option<StoredConversation>, ChatHistoryError> {
        let Some(conn) = self.conn.as_mut() else {
            return Err(READ_FAILED);
        };
        self.rt
            .block_on(read_load(conn, id))
            .map_err(|_| READ_FAILED)
    }
}

async fn checkpoint(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_all(conn)
        .await
        .map(|_| ())
}

fn big(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn kind_of(entry: &StoredEntry) -> &'static str {
    match entry {
        StoredEntry::User { .. } => "user",
        StoredEntry::Assistant { .. } => "assistant",
        StoredEntry::ToolCall { .. } => "tool_call",
        StoredEntry::Sql { .. } => "sql",
        StoredEntry::Notice { .. } => "notice",
    }
}

fn title_from(text: &str) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(MAX_STORED_TITLE_CHARS).collect()
}

/// Runs the commands in one transaction. Returns whether a checkpoint is due.
async fn apply(
    conn: &mut SqliteConnection,
    cmds: Vec<Cmd>,
    options: &ChatHistoryOptions,
    last_ms: &mut i64,
) -> Result<bool, sqlx::Error> {
    let mut tx = conn.begin().await?;
    let mut checkpoint_due = false;
    for cmd in cmds {
        match cmd {
            Cmd::Append {
                id,
                connection,
                entries,
            } => {
                let now = now_ms().max(*last_ms + 1);
                *last_ms = now;
                append(&mut tx, &id, &connection, entries, options, now).await?;
            }
            Cmd::Trim { id, first_kept } => {
                sqlx::query("DELETE FROM entry WHERE conversation = ?1 AND seq < ?2")
                    .bind(id.as_str())
                    .bind(big(first_kept))
                    .execute(&mut *tx)
                    .await?;
                update_bytes(&mut tx, &id).await?;
                checkpoint_due = true;
            }
            Cmd::Delete(id) => {
                sqlx::query("DELETE FROM conversation WHERE id = ?1")
                    .bind(id.as_str())
                    .execute(&mut *tx)
                    .await?;
                checkpoint_due = true;
            }
            Cmd::Clear => {
                sqlx::query("DELETE FROM conversation")
                    .execute(&mut *tx)
                    .await?;
                checkpoint_due = true;
            }
            _ => {}
        }
    }
    checkpoint_due |= retention(&mut tx, options).await?;
    tx.commit().await?;
    Ok(checkpoint_due)
}

async fn update_bytes(conn: &mut SqliteConnection, id: &ConversationId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE conversation SET bytes = \
         (SELECT COALESCE(SUM(length(CAST(payload AS BLOB))), 0) FROM entry WHERE conversation = ?1) \
         WHERE id = ?1",
    )
    .bind(id.as_str())
    .execute(conn)
    .await
    .map(|_| ())
}

async fn append(
    conn: &mut SqliteConnection,
    id: &ConversationId,
    connection: &str,
    entries: Vec<(u64, StoredEntry)>,
    options: &ChatHistoryOptions,
    now: i64,
) -> Result<(), sqlx::Error> {
    if entries.is_empty() {
        return Ok(());
    }
    let fitted: Vec<(u64, StoredEntry)> = entries
        .into_iter()
        .map(|(seq, entry)| (seq, fit_entry(entry, options)))
        .collect();
    let title = fitted
        .iter()
        .find_map(|(_, e)| match e {
            StoredEntry::User { text } => Some(title_from(text)),
            _ => None,
        })
        .unwrap_or_default();
    sqlx::query(
        "INSERT INTO conversation (id, connection, title, created_ms, updated_ms, bytes) \
         VALUES (?1, ?2, ?3, ?4, ?4, 0) \
         ON CONFLICT(id) DO UPDATE SET updated_ms = excluded.updated_ms, \
         title = CASE WHEN conversation.title = '' THEN excluded.title ELSE conversation.title END",
    )
    .bind(id.as_str())
    .bind(super::caps::cut_text(connection, 256))
    .bind(title)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    for (seq, entry) in &fitted {
        let Ok(payload) = serde_json::to_string(entry) else {
            continue;
        };
        sqlx::query("INSERT OR REPLACE INTO entry (conversation, seq, kind, payload) VALUES (?1, ?2, ?3, ?4)")
            .bind(id.as_str())
            .bind(big(*seq))
            .bind(kind_of(entry))
            .bind(payload)
            .execute(&mut *conn)
            .await?;
    }
    update_bytes(conn, id).await
}

/// Deletes the oldest conversations while there are too many or too many bytes. The newest
/// conversation always stays. Returns whether anything was deleted.
async fn retention(
    conn: &mut SqliteConnection,
    options: &ChatHistoryOptions,
) -> Result<bool, sqlx::Error> {
    let mut removed = false;
    loop {
        let (count, bytes): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM conversation")
                .fetch_one(&mut *conn)
                .await?;
        let count = usize::try_from(count).unwrap_or(0);
        let bytes = u64::try_from(bytes).unwrap_or(0);
        if count <= 1 || (count <= options.max_conversations && bytes <= options.max_bytes) {
            return Ok(removed);
        }
        sqlx::query(
            "DELETE FROM conversation WHERE id = \
             (SELECT id FROM conversation ORDER BY updated_ms ASC, id ASC LIMIT 1)",
        )
        .execute(&mut *conn)
        .await?;
        removed = true;
    }
}

type SummaryRow = (String, String, String, i64, i64, i64);

fn summary(row: SummaryRow) -> ConversationSummary {
    let (id, connection, title, created_ms, updated_ms, entries) = row;
    ConversationSummary {
        id: ConversationId(id),
        connection,
        title,
        created_ms,
        updated_ms,
        entries: u64::try_from(entries).unwrap_or(0),
    }
}

async fn read_list(conn: &mut SqliteConnection) -> Result<Vec<ConversationSummary>, sqlx::Error> {
    let rows: Vec<SummaryRow> = sqlx::query_as(
        "SELECT c.id, c.connection, c.title, c.created_ms, c.updated_ms, \
         (SELECT COUNT(*) FROM entry e WHERE e.conversation = c.id) \
         FROM conversation c ORDER BY c.updated_ms DESC, c.created_ms DESC, c.id DESC",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(summary).collect())
}

async fn read_load(
    conn: &mut SqliteConnection,
    id: &ConversationId,
) -> Result<Option<StoredConversation>, sqlx::Error> {
    let row: Option<SummaryRow> = sqlx::query_as(
        "SELECT c.id, c.connection, c.title, c.created_ms, c.updated_ms, 0 \
         FROM conversation c WHERE c.id = ?1",
    )
    .bind(id.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored: Vec<(i64, String)> =
        sqlx::query_as("SELECT seq, payload FROM entry WHERE conversation = ?1 ORDER BY seq")
            .bind(id.as_str())
            .fetch_all(&mut *conn)
            .await?;
    let mut summary = summary(row);
    summary.entries = stored.len() as u64;
    let mut first_seq = None;
    let mut entries = Vec::with_capacity(stored.len());
    for (seq, payload) in stored {
        // An unparseable row (a kind this version does not know) is skipped.
        if let Ok(entry) = serde_json::from_str::<StoredEntry>(&payload) {
            first_seq.get_or_insert(u64::try_from(seq).unwrap_or(0));
            entries.push(entry);
        }
    }
    Ok(Some(StoredConversation {
        summary,
        first_seq: first_seq.unwrap_or(0),
        entries,
    }))
}
