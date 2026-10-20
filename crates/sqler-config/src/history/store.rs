// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The query history store over `history.jsonl`.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use super::entry::{HistoryEntry, HistoryIssue, HistoryOptions, HistoryQuery, NewHistoryEntry};
use super::file::{parse_line, serialize_line, to_ms, truncate_sql};
use super::redact::redact_credentials;
use crate::fs::{read_optional_bytes, write_atomic};
use crate::{ConfigError, ConnectionId};

struct Stored {
    entry: HistoryEntry,
    lower: String,
}

struct Inner {
    /// Oldest first, which is file order.
    entries: VecDeque<Stored>,
    /// Raw lines that did not load; kept byte for byte on compaction.
    preserved: Vec<Vec<u8>>,
    issues: Vec<HistoryIssue>,
    /// Entry lines in the file (bad lines are not counted).
    lines_in_file: usize,
    /// The file does not end in a newline (a torn last line).
    needs_newline: bool,
    options: HistoryOptions,
}

/// The query history: a bounded, searchable list of executed statements kept
/// in `history.jsonl` (ADR-0021).
///
/// The handle is `Send + Sync`; share it between threads with an `Arc`. All
/// methods take `&self` and do blocking file I/O; async callers should use
/// `spawn_blocking`, as with `SecretStore`.
///
/// Two processes writing the same file are not coordinated.
pub struct HistoryStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Cuts to the size cap first, so redaction works on a bounded text (a
/// construct the cut leaves open is hidden to the end), then cuts again in case
/// placeholders made the text longer.
fn truncate_then_redact(sql: &str) -> (String, bool) {
    let mut cut = sql.len().min(super::MAX_HISTORY_SQL_BYTES);
    while !sql.is_char_boundary(cut) {
        cut -= 1;
    }
    let (out, t2) = truncate_sql(redact_credentials(&sql[..cut]).into_owned());
    (out, cut < sql.len() || t2)
}

impl HistoryStore {
    /// Opens the history at `path`; a missing file is an empty history.
    ///
    /// Lines that do not load (corrupt, or written by a newer `SQLer`) are
    /// reported by [`issues`](Self::issues), kept in the file, and do not stop
    /// the other lines from loading. Loading never rewrites the file.
    ///
    /// # Errors
    /// [`ConfigError::Read`] when the file exists but cannot be read.
    pub fn open(path: impl Into<PathBuf>, options: HistoryOptions) -> Result<Self, ConfigError> {
        let path = path.into();
        let options = options.clamped();
        let bytes = read_optional_bytes(&path)?.unwrap_or_default();
        let inner = load(&bytes, options);
        Ok(Self {
            path,
            inner: Mutex::new(inner),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The path of the history file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Lines that did not load when the store was opened, in file order.
    #[must_use]
    pub fn issues(&self) -> Vec<HistoryIssue> {
        self.lock().issues.clone()
    }

    /// The current options (with `max_entries` clamped).
    #[must_use]
    pub fn options(&self) -> HistoryOptions {
        self.lock().options
    }

    /// Changes the options. Older entries beyond a smaller `max_entries` leave
    /// memory at once; the file shrinks at the next compaction.
    pub fn set_options(&self, options: HistoryOptions) {
        let mut g = self.lock();
        g.options = options.clamped();
        let max = g.options.max_entries;
        trim(&mut g.entries, max);
    }

    /// The number of entries in memory.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Records an executed query. Returns `Ok(false)` and changes nothing when
    /// history is disabled.
    ///
    /// The SQL is passed through [`redact_credentials`]
    /// and cut to [`MAX_HISTORY_SQL_BYTES`](super::MAX_HISTORY_SQL_BYTES) first.
    /// Time and duration are kept at millisecond precision. When the file
    /// grows past the cap plus some slack it is compacted to the newest
    /// entries; a failed compaction is retried by a later call and does not
    /// fail this one.
    ///
    /// # Errors
    /// [`ConfigError::Write`] when the line cannot be appended. The entry
    /// stays in memory.
    pub fn record(&self, new: NewHistoryEntry) -> Result<bool, ConfigError> {
        let mut g = self.lock();
        if !g.options.enabled {
            return Ok(false);
        }
        let (sql, truncated) = truncate_then_redact(&new.sql);
        let entry = HistoryEntry {
            sql,
            connection: new.connection,
            started_at: UNIX_EPOCH + Duration::from_millis(to_ms(new.started_at)),
            duration: Duration::from_millis(
                u64::try_from(new.duration.as_millis()).unwrap_or(u64::MAX),
            ),
            outcome: new.outcome,
            rows: new.rows,
            truncated,
        };
        let line = serialize_line(&entry);
        g.entries.push_back(Stored {
            lower: entry.sql.to_lowercase(),
            entry,
        });
        let max = g.options.max_entries;
        trim(&mut g.entries, max);
        append(&self.path, &mut g, &line)?;
        let slack = (max / 10).max(100);
        if g.lines_in_file > max + slack {
            let _ = rewrite(&self.path, &mut g, |_| true);
        }
        Ok(true)
    }

    /// Entries matching `query`, newest first, at most `query.limit`.
    ///
    /// `query.text` is split on whitespace; every term must occur in the SQL,
    /// ignoring case.
    #[must_use]
    pub fn search(&self, query: &HistoryQuery<'_>) -> Vec<HistoryEntry> {
        let terms: Vec<String> = query
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let g = self.lock();
        g.entries
            .iter()
            .rev()
            .filter(|s| query.connection.is_none_or(|c| s.entry.connection == *c))
            .filter(|s| terms.iter().all(|t| s.lower.contains(t.as_str())))
            .take(query.limit)
            .map(|s| s.entry.clone())
            .collect()
    }

    /// Removes every entry and every kept bad line, atomically.
    ///
    /// # Errors
    /// [`ConfigError::Write`]; nothing changes then.
    pub fn clear(&self) -> Result<(), ConfigError> {
        let mut g = self.lock();
        write_atomic(&self.path, b"")?;
        g.entries.clear();
        g.preserved.clear();
        g.issues.clear();
        g.lines_in_file = 0;
        g.needs_newline = false;
        Ok(())
    }

    /// Removes the entries of one connection, atomically, and returns how many
    /// were removed. Kept bad lines stay.
    ///
    /// # Errors
    /// [`ConfigError::Write`]; nothing changes then.
    pub fn clear_connection(&self, connection: &ConnectionId) -> Result<usize, ConfigError> {
        let mut g = self.lock();
        let before = g.entries.len();
        if !g.entries.iter().any(|s| s.entry.connection == *connection) {
            return Ok(0);
        }
        rewrite(&self.path, &mut g, |s| s.entry.connection != *connection)?;
        Ok(before - g.entries.len())
    }
}

fn trim(entries: &mut VecDeque<Stored>, max: usize) {
    while entries.len() > max {
        entries.pop_front();
    }
}

fn load(bytes: &[u8], options: HistoryOptions) -> Inner {
    let mut inner = Inner {
        entries: VecDeque::new(),
        preserved: Vec::new(),
        issues: Vec::new(),
        lines_in_file: 0,
        needs_newline: !bytes.is_empty() && !bytes.ends_with(b"\n"),
        options,
    };
    for (i, raw) in bytes.split(|&b| b == b'\n').enumerate() {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(raw) {
            Ok(entry) => {
                inner.lines_in_file += 1;
                inner.entries.push_back(Stored {
                    lower: entry.sql.to_lowercase(),
                    entry,
                });
            }
            Err(kind) => {
                inner.issues.push(HistoryIssue { line: i + 1, kind });
                inner.preserved.push(raw.to_vec());
            }
        }
    }
    trim(&mut inner.entries, options.max_entries);
    inner
}

/// Appends one line, first ending a torn last line. Opens the file per call so
/// no handle is held across a rename (Windows).
fn append(path: &Path, g: &mut Inner, line: &[u8]) -> Result<(), ConfigError> {
    let werr = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };
    let mut buf = Vec::with_capacity(line.len() + 1);
    if g.needs_newline {
        buf.push(b'\n');
    }
    buf.extend_from_slice(line);
    let open = || OpenOptions::new().create(true).append(true).open(path);
    let mut file = match open() {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(werr)?;
            }
            open().map_err(werr)?
        }
        other => other.map_err(werr)?,
    };
    if let Err(e) = file.write_all(&buf) {
        // Part of the line may be on disk; end it before the next append.
        g.needs_newline = true;
        return Err(werr(e));
    }
    g.needs_newline = false;
    g.lines_in_file += 1;
    Ok(())
}

/// Atomically rewrites the file as the kept bad lines plus the entries that
/// pass `keep`, then drops the others from memory. Memory changes only after
/// the write succeeded.
fn rewrite(path: &Path, g: &mut Inner, keep: impl Fn(&Stored) -> bool) -> Result<(), ConfigError> {
    let mut out = Vec::new();
    for raw in &g.preserved {
        out.extend_from_slice(raw);
        out.push(b'\n');
    }
    for s in g.entries.iter().filter(|s| keep(s)) {
        out.extend_from_slice(&serialize_line(&s.entry));
    }
    write_atomic(path, &out)?;
    g.entries.retain(|s| keep(s));
    g.lines_in_file = g.entries.len();
    g.needs_newline = false;
    Ok(())
}
