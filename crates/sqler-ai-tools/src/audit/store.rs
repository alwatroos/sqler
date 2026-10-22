// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The audit file: append, rotate and read.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use sqler_config::ConfigDir;

use super::entry::AuditEntry;
use super::query::{AuditCursor, AuditPage, AuditQuery};
use super::wire::{LineIssue, parse_line, serialize_line};
use super::{AUDIT_FILE_NAME, AuditSink};

const MIN_FILE_BYTES: u64 = 64 * 1024;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_KEEP: u32 = 20;
/// The most bytes one `read` loads, newest first; older bytes are not scanned.
const READ_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// A failed audit file operation. Never echoes file content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    /// The file or its directory could not be read, written or rotated.
    #[error("audit log I/O failed for {}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        source: io::Error,
    },
}

/// Size and retention of the audit file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditOptions {
    /// Rotate before the current file would exceed this. Default 10 MiB, clamped 64 KiB..=1 GiB.
    pub max_file_bytes: u64,
    /// How many rotated files to keep. Default 2, clamped 1..=20.
    pub keep_files: u32,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            max_file_bytes: 10 * 1024 * 1024,
            keep_files: 2,
        }
    }
}

impl AuditOptions {
    /// Sets the size at which the file rotates (clamped to 64 KiB..=1 GiB).
    #[must_use]
    pub fn with_max_file_bytes(mut self, bytes: u64) -> Self {
        self.max_file_bytes = bytes.clamp(MIN_FILE_BYTES, MAX_FILE_BYTES);
        self
    }

    /// Sets how many rotated files are kept (clamped to 1..=20).
    #[must_use]
    pub fn with_keep_files(mut self, keep: u32) -> Self {
        self.keep_files = keep.clamp(1, MAX_KEEP);
        self
    }

    fn clamped(self) -> Self {
        Self::default()
            .with_max_file_bytes(self.max_file_bytes)
            .with_keep_files(self.keep_files)
    }
}

/// The audit log file. Share it between threads with an `Arc`; all methods take `&self`.
///
/// Creating it does no I/O. Each append opens the file, so a deleted file or directory is
/// recreated. Methods are blocking; async hosts use `spawn_blocking`.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    options: AuditOptions,
    io: Mutex<()>,
    failed: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl AuditLog {
    /// The log in `<config dir>/ai-audit.jsonl` with default options. No I/O.
    #[must_use]
    pub fn open(dir: &ConfigDir) -> Self {
        Self::open_at(dir.root().join(AUDIT_FILE_NAME), AuditOptions::default())
    }

    /// The log at an explicit path. No I/O.
    #[must_use]
    pub fn open_at(path: impl Into<PathBuf>, options: AuditOptions) -> Self {
        Self {
            path: path.into(),
            options: options.clamped(),
            io: Mutex::new(()),
            failed: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// The current file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many [`AuditSink::record`] calls failed to write.
    #[must_use]
    pub fn failed_writes(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    /// The text of the last write failure, if any.
    #[must_use]
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn rotated(&self, k: u32) -> PathBuf {
        let stem = self
            .path
            .file_stem()
            .map_or_else(|| "ai-audit".into(), std::ffi::OsStr::to_os_string);
        let mut name = stem;
        name.push(format!(".{k}"));
        if let Some(ext) = self.path.extension() {
            name.push(".");
            name.push(ext);
        }
        self.path.with_file_name(name)
    }

    fn rotate(&self) -> io::Result<()> {
        let keep = self.options.keep_files;
        remove_if_exists(&self.rotated(keep))?;
        for k in (1..keep).rev() {
            match fs::rename(self.rotated(k), self.rotated(k + 1)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        fs::rename(&self.path, self.rotated(1))
    }

    /// Takes the cross-process advisory lock on `<file>.lock`; `None` if it cannot be had.
    fn lock_file(&self) -> Option<fs::File> {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".lock");
        let file = open_private(Path::new(&name)).ok()?;
        file.lock().ok()?;
        Some(file)
    }

    /// Appends one entry as one line, rotating first if the file would grow past its cap.
    ///
    /// # Errors
    /// [`AuditError::Io`] if the directory, file or write fails.
    pub fn append(&self, entry: &AuditEntry) -> Result<(), AuditError> {
        let line = serialize_line(entry);
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| AuditError::Io { path, source }
        };
        let _guard = self.io.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir).map_err(io_err(dir))?;
        }
        // Held to the end of the call: serializes the size check, rotation and write across
        // processes (the app and `sqler-cli mcp`). Best effort: without it, appends still work.
        let _file_lock = self.lock_file();
        let len = fs::metadata(&self.path).map_or(0, |m| m.len());
        if len > 0 && len + line.len() as u64 > self.options.max_file_bytes {
            // If rotation fails, the line goes to the current file anyway.
            let _ = self.rotate();
        }
        let mut file = open_private(&self.path).map_err(io_err(&self.path))?;
        let len = file.metadata().map_err(io_err(&self.path))?.len();
        let mut buf = Vec::with_capacity(line.len() + 1);
        if len > 0 {
            let mut last = [0_u8; 1];
            file.seek(SeekFrom::End(-1)).map_err(io_err(&self.path))?;
            file.read_exact(&mut last).map_err(io_err(&self.path))?;
            if last[0] != b'\n' {
                buf.push(b'\n');
            }
        }
        buf.extend_from_slice(&line);
        file.write_all(&buf).map_err(io_err(&self.path))
    }

    /// Reads entries newest first: the current file from its end, then `.1`, `.2` and so on.
    ///
    /// # Errors
    /// [`AuditError::Io`] if a file exists but cannot be read.
    pub fn read(&self, query: &AuditQuery) -> Result<AuditPage, AuditError> {
        let mut files = vec![self.path.clone()];
        files.extend((1..=self.options.keep_files).map(|k| self.rotated(k)));
        let mut contents = Vec::new();
        let mut budget = READ_BUDGET_BYTES;
        {
            // Only the loading of the bytes holds the lock; the scan of up to 64 MiB does not,
            // so an open audit view never stalls a tool call that appends.
            let _guard = self.io.lock().unwrap_or_else(PoisonError::into_inner);
            for path in files {
                if budget == 0 {
                    break;
                }
                match read_tail(&path, budget) {
                    Ok(Some(bytes)) => {
                        budget = budget.saturating_sub(bytes.len() as u64);
                        contents.push(bytes);
                    }
                    Ok(None) => {}
                    Err(source) => return Err(AuditError::Io { path, source }),
                }
            }
        }
        Ok(scan(&contents, query))
    }
}

/// `record` is a short blocking append (one small write under the lock). It never waits on a
/// `read`, which releases the lock before it scans.
impl AuditSink for AuditLog {
    fn record(&self, entry: &AuditEntry) {
        if let Err(e) = self.append(entry) {
            self.failed.fetch_add(1, Ordering::Relaxed);
            *self
                .last_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(e.to_string());
        }
    }
}

/// Creates the directory (and parents), owner-only on Unix. An existing directory is kept as is.
/// Windows relies on the ACLs the profile directory already carries.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Opens the audit file for append, creating it owner-only (0600) on Unix; a looser existing
/// file is tightened. Windows relies on the profile ACLs.
fn open_private(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file.metadata()?.permissions().mode();
        if mode & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(file)
}

/// The last `budget` bytes of a file, cut to whole lines; `None` if the file is missing.
fn read_tail(path: &Path, budget: u64) -> io::Result<Option<Vec<u8>>> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let take = len.min(budget);
    file.seek(SeekFrom::Start(len - take))?;
    let mut bytes = Vec::with_capacity(usize::try_from(take).unwrap_or(0));
    file.take(take).read_to_end(&mut bytes)?;
    if take < len {
        // The first line is cut mid-way: drop it.
        let start = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..start);
    }
    Ok(Some(bytes))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Every non-blank line of the files, newest first.
fn lines_newest_first(contents: &[Vec<u8>]) -> impl Iterator<Item = &[u8]> {
    contents.iter().flat_map(|bytes| {
        bytes
            .split(|&b| b == b'\n')
            .rev()
            .filter(|l| !l.iter().all(u8::is_ascii_whitespace))
    })
}

fn scan(contents: &[Vec<u8>], query: &AuditQuery) -> AuditPage {
    let text = query.text.as_ref().map(|t| t.to_lowercase());
    // A cursor whose id is gone (rotated away) falls back to a time cut.
    let cursor_found = query.cursor.as_ref().is_some_and(|c| {
        lines_newest_first(contents).any(|l| parse_line(l).is_ok_and(|e| e.id == c.id))
    });
    let mut passed = query.cursor.is_none() || !cursor_found;
    let mut page = AuditPage {
        entries: Vec::new(),
        next: None,
        skipped_lines: 0,
        newer_lines: 0,
    };
    for line in lines_newest_first(contents) {
        let entry = match parse_line(line) {
            Ok(e) => e,
            Err(LineIssue::Malformed) => {
                page.skipped_lines += 1;
                continue;
            }
            Err(LineIssue::Newer) => {
                page.newer_lines += 1;
                continue;
            }
        };
        if let Some(c) = &query.cursor {
            if cursor_found {
                if !passed {
                    passed = entry.id == c.id;
                    continue;
                }
            } else if entry.ts_ms >= c.ts_ms {
                continue;
            }
        }
        if !query.matches(&entry, text.as_deref()) {
            continue;
        }
        page.entries.push(entry);
        if page.entries.len() >= query.limit {
            page.next = page
                .entries
                .last()
                .map(|e| AuditCursor::new(e.ts_ms, e.id.clone()));
            break;
        }
    }
    page
}
