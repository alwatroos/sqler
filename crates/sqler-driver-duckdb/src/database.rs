// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Opening databases. One `DuckDB` instance per file in the process.
//!
//! `DuckDB` refuses a second instance on the same file, even inside one process. The object
//! browser, a query tab and an export all want their own session, so every session of a file
//! gets a `try_clone` of one shared connection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use duckdb::{Config, Connection};
use sqler_driver_api::DriverError;

use crate::error::{NOT_OPENED, Scrub, connect_error};

/// The in-memory target.
const MEMORY: &str = ":memory:";

/// The root connection of one file. Dropping the last `Arc` closes the instance.
pub(crate) struct Shared {
    root: Mutex<Connection>,
}

static REGISTRY: LazyLock<Mutex<HashMap<PathBuf, Weak<Shared>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// An open connection and what keeps its instance alive.
pub(crate) struct Opened {
    pub(crate) conn: Connection,
    pub(crate) scrub: Scrub,
    /// Declared after `conn`, so the connection closes before the instance can.
    _shared: Option<Arc<Shared>>,
}

/// The sandbox: no external files, no extensions, settings locked.
fn config() -> Result<Config, duckdb::Error> {
    Config::default()
        .enable_external_access(false)?
        .enable_autoload_extension(false)?
        .with("allow_community_extensions", "false")?
        .with("lock_configuration", "true")
}

fn not_opened() -> DriverError {
    DriverError::Connection(NOT_OPENED.to_owned())
}

/// The registry key: the canonical path, or the canonical parent plus the file name.
fn key_of(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let name = path.file_name()?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Some(parent.canonicalize().ok()?.join(name))
}

/// Opens `target` (`:memory:` or a file path; a missing file is created).
pub(crate) fn open(target: &str) -> Result<Opened, DriverError> {
    let cfg = config().map_err(|e| connect_error(&e))?;
    if target == MEMORY {
        let conn = Connection::open_in_memory_with_flags(cfg).map_err(|e| connect_error(&e))?;
        return Ok(Opened {
            conn,
            scrub: Scrub::default(),
            _shared: None,
        });
    }
    let path = Path::new(target);
    let key = key_of(path).ok_or_else(not_opened)?;
    let scrub = Scrub::new([target.to_owned(), key.to_string_lossy().into_owned()]);
    let mut registry = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
    registry.retain(|_, weak| weak.strong_count() > 0);
    let shared = if let Some(shared) = registry.get(&key).and_then(Weak::upgrade) {
        shared
    } else {
        let root = Connection::open_with_flags(path, cfg).map_err(|e| connect_error(&e))?;
        let shared = Arc::new(Shared {
            root: Mutex::new(root),
        });
        registry.insert(key, Arc::downgrade(&shared));
        shared
    };
    let conn = shared
        .root
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .try_clone()
        .map_err(|e| connect_error(&e))?;
    drop(registry);
    Ok(Opened {
        conn,
        scrub,
        _shared: Some(shared),
    })
}
