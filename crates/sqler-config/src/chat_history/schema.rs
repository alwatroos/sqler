// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Opening `chat-history.db`: header check, private modes, corrupt and newer files, migrations.
//!
//! Nothing here echoes file content in a text that can reach the user.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqliteSynchronous};
use sqlx::{AssertSqlSafe, ConnectOptions, Connection};

use crate::chat_history::types::CHAT_HISTORY_SCHEMA_VERSION;
use crate::fs::{create_private_file, set_private};

/// The status text when the file was written by a newer `SQLer`.
pub(crate) const OFF_NEWER: &str =
    "Chat history is off: chat-history.db was written by a newer SQLer and is left as it is.";
/// The notice when an unreadable file was moved aside.
pub(crate) const MOVED_NOTICE: &str =
    "chat-history.db could not be read; it was moved aside and a new history was started.";
/// The status text when the file cannot be used and cannot be moved aside.
pub(crate) const OFF_UNREADABLE: &str =
    "Chat history is off: chat-history.db could not be opened and was left as it is.";

/// Index `i` takes the schema from version `i` to `i + 1`. A change is a new entry, never an edit.
pub(crate) const MIGRATIONS: &[&str] = &["CREATE TABLE conversation (
        id TEXT PRIMARY KEY,
        connection TEXT NOT NULL,
        title TEXT NOT NULL,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL,
        bytes INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE TABLE entry (
        conversation TEXT NOT NULL REFERENCES conversation(id) ON DELETE CASCADE,
        seq INTEGER NOT NULL,
        kind TEXT NOT NULL,
        payload TEXT NOT NULL,
        PRIMARY KEY (conversation, seq)
    ) STRICT, WITHOUT ROWID;
    CREATE INDEX conversation_updated ON conversation(updated_ms);"];

const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const HEADER_LEN: usize = 100;
const SIDE_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// The result of opening the store.
pub(crate) enum Opened {
    /// The store works. `notice` is set when an unreadable file was moved aside.
    Ready {
        conn: SqliteConnection,
        notice: Option<String>,
    },
    /// The store is off, for the reason given.
    Off(String),
}

/// Why a file could not be opened.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Corrupt,
    Newer,
    Other,
}

/// Opens the store at `path`.
pub(crate) async fn open(path: &Path) -> Opened {
    match try_open(path).await {
        Ok(conn) => Opened::Ready { conn, notice: None },
        Err(Failure::Newer) => Opened::Off(OFF_NEWER.to_owned()),
        Err(Failure::Other) => Opened::Off(OFF_UNREADABLE.to_owned()),
        Err(Failure::Corrupt) => {
            if move_aside(path).is_err() {
                return Opened::Off(OFF_UNREADABLE.to_owned());
            }
            match try_open(path).await {
                Ok(conn) => Opened::Ready {
                    conn,
                    notice: Some(MOVED_NOTICE.to_owned()),
                },
                Err(_) => Opened::Off(OFF_UNREADABLE.to_owned()),
            }
        }
    }
}

async fn try_open(path: &Path) -> Result<SqliteConnection, Failure> {
    precheck(path)?;
    create_private_file(path).map_err(|_| Failure::Other)?;
    chmod_side_files(path);
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(2))
        .pragma("secure_delete", "ON")
        .disable_statement_logging();
    let mut conn = options.connect().await.map_err(|e| classify(&e))?;
    chmod_side_files(path);
    match check_and_migrate(&mut conn).await {
        Ok(()) => {
            chmod_side_files(path);
            Ok(conn)
        }
        Err(failure) => {
            let _ = conn.close().await;
            Err(failure)
        }
    }
}

/// Reads the file header: a non-empty file must be a SQLite database of a known version.
fn precheck(path: &Path) -> Result<(), Failure> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Failure::Other),
    };
    let len = file.metadata().map_err(|_| Failure::Other)?.len();
    if len == 0 {
        return Ok(());
    }
    let mut header = [0u8; HEADER_LEN];
    if len < HEADER_LEN as u64 || file.read_exact(&mut header).is_err() || header[..16] != MAGIC[..]
    {
        return Err(Failure::Corrupt);
    }
    let version = u32::from_be_bytes([header[60], header[61], header[62], header[63]]);
    if version > CHAT_HISTORY_SCHEMA_VERSION {
        return Err(Failure::Newer);
    }
    Ok(())
}

async fn check_and_migrate(conn: &mut SqliteConnection) -> Result<(), Failure> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| classify(&e))?;
    if version > i64::from(CHAT_HISTORY_SCHEMA_VERSION) {
        return Err(Failure::Newer);
    }
    let checks: Vec<String> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| classify(&e))?;
    if checks != ["ok"] {
        return Err(Failure::Corrupt);
    }
    migrate(conn).await.map_err(|e| classify(&e))
}

/// Brings the schema up to date in one write transaction. The version is read again after the
/// write lock is held, so a second `SQLer` starting at the same moment migrates only once.
async fn migrate(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    match migrate_locked(conn).await {
        Ok(()) => sqlx::query("COMMIT").execute(&mut *conn).await.map(|_| ()),
        Err(e) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            Err(e)
        }
    }
}

async fn migrate_locked(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *conn)
        .await?;
    let from = usize::try_from(version).unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(from) {
        sqlx::raw_sql(*sql).execute(&mut *conn).await?;
        sqlx::raw_sql(AssertSqlSafe(format!("PRAGMA user_version = {}", i + 1)))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// `SQLITE_CORRUPT` (11) and `SQLITE_NOTADB` (26), also as extended codes, mean a damaged file.
fn classify(err: &sqlx::Error) -> Failure {
    if let sqlx::Error::Database(db) = &err
        && let Some(code) = db.code().and_then(|c| c.parse::<i32>().ok())
        && matches!(code & 0xff, 11 | 26)
    {
        return Failure::Corrupt;
    }
    Failure::Other
}

fn side_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}

/// Sets mode `0600` on the `-wal`, `-shm` and `-journal` files that exist.
pub(crate) fn chmod_side_files(path: &Path) {
    for suffix in SIDE_SUFFIXES {
        let side = side_path(path, suffix);
        if side.exists() {
            let _ = set_private(&side);
        }
    }
}

/// Renames the file (and its `-wal` and `-journal`) to `<name>.corrupt-<unix secs>` and removes
/// the `-shm`.
fn move_aside(path: &Path) -> std::io::Result<()> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let tag = format!(".corrupt-{secs}");
    let target = side_path(path, &tag);
    std::fs::rename(path, &target)?;
    let _ = set_private(&target);
    for suffix in ["-wal", "-journal"] {
        let from = side_path(path, suffix);
        if from.exists() {
            let to = side_path(path, &format!("{tag}{suffix}"));
            if std::fs::rename(&from, &to).is_ok() {
                let _ = set_private(&to);
            }
        }
    }
    let _ = std::fs::remove_file(side_path(path, "-shm"));
    Ok(())
}
