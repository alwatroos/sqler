// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A blocking facade over the history store and `queries.toml`.
//!
//! The store keeps a lock while it appends and compacts, and loading a file reads the disk, so
//! every method here may block. The view calls them on a blocking thread
//! ([`SessionBridge::blocking`](crate::workspace::SessionBridge)), never on the UI thread.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqler_config::{
    ConfigDir, HistoryEntry, HistoryOptions, HistoryQuery, HistoryStore, NewHistoryEntry,
    SavedQueries,
};

use super::model::HistorySearch;

/// The message when there is no configuration directory to keep saved queries in.
const NO_DIR: &str = "No config directory: saved queries are not available";

/// The history store and saved queries of one configuration directory. Cloning shares the store.
#[derive(Clone)]
pub struct HistoryService {
    dir: Option<ConfigDir>,
    options: HistoryOptions,
    store: Arc<Mutex<Lazy>>,
    /// The newest saved-queries generation that was written.
    written: Arc<Mutex<u64>>,
}

/// The store, opened on first use.
#[derive(Default)]
struct Lazy {
    store: Option<Arc<HistoryStore>>,
    /// Notices about the open that [`HistoryService::open`] has not returned yet.
    notices: Vec<String>,
}

impl fmt::Debug for HistoryService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HistoryService")
            .field("persistent", &self.dir.is_some())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl HistoryService {
    /// A service over `dir` (`None`: nothing is kept). It does no I/O; the store opens on the
    /// first call that needs it.
    #[must_use]
    pub fn new(dir: Option<ConfigDir>, options: HistoryOptions) -> Self {
        Self {
            dir,
            options,
            store: Arc::new(Mutex::new(Lazy::default())),
            written: Arc::new(Mutex::new(0)),
        }
    }

    /// The open store; opens it first when needed. `Ok(None)` without a configuration directory.
    fn store(&self) -> Result<Option<Arc<HistoryStore>>, String> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        let mut lazy = lock(&self.store);
        if let Some(store) = &lazy.store {
            return Ok(Some(store.clone()));
        }
        let store = dir
            .open_history(self.options)
            .map_err(|err| err.to_string())?;
        let bad = store.issues().len();
        if bad > 0 {
            let lines = if bad == 1 { "line" } else { "lines" };
            lazy.notices.push(format!(
                "history.jsonl: {bad} {lines} could not be read; they are kept in the file"
            ));
        }
        let store = Arc::new(store);
        lazy.store = Some(store.clone());
        Ok(Some(store))
    }

    /// Opens the store (once) and returns the notices for the user: one when lines of
    /// `history.jsonl` could not be read. The line content is never quoted. Later calls return
    /// no notice.
    ///
    /// # Errors
    /// The reason the history file cannot be read.
    pub fn open(&self) -> Result<Vec<String>, String> {
        self.store()?;
        Ok(std::mem::take(&mut lock(&self.store).notices))
    }

    /// Records one executed statement. Without a configuration directory this does nothing.
    ///
    /// # Errors
    /// The reason the store cannot be opened or written.
    pub fn record(&self, entry: NewHistoryEntry) -> Result<(), String> {
        match self.store()? {
            Some(store) => store
                .record(entry)
                .map(|_| ())
                .map_err(|err| err.to_string()),
            None => Ok(()),
        }
    }

    /// The entries that match `search`, newest first. A store that cannot be opened has none.
    #[must_use]
    pub fn search(&self, search: &HistorySearch) -> Vec<HistoryEntry> {
        let Ok(Some(store)) = self.store() else {
            return Vec::new();
        };
        store.search(&HistoryQuery {
            text: &search.text,
            connection: search.connection.as_ref(),
            limit: search.limit,
        })
    }

    /// Deletes the whole history. Without a configuration directory this does nothing.
    ///
    /// # Errors
    /// The reason the store cannot be opened or rewritten.
    pub fn clear(&self) -> Result<(), String> {
        match self.store()? {
            Some(store) => store.clear().map_err(|err| err.to_string()),
            None => Ok(()),
        }
    }

    /// Reads `queries.toml`; a missing file is an empty list.
    ///
    /// # Errors
    /// The reason the file cannot be read, or that there is no configuration directory.
    pub fn load_saved(&self) -> Result<SavedQueries, String> {
        let Some(dir) = &self.dir else {
            return Err(NO_DIR.to_owned());
        };
        dir.load_saved_queries().map_err(|err| err.to_string())
    }

    /// Writes `queries` as generation `generation`. A generation that is not newer than the last
    /// one written is skipped, so overlapping writes cannot bring back an older list.
    ///
    /// # Errors
    /// The reason the file cannot be written, or that there is no configuration directory.
    pub fn save_saved(&self, generation: u64, queries: &SavedQueries) -> Result<(), String> {
        let Some(dir) = &self.dir else {
            return Err(NO_DIR.to_owned());
        };
        // Held across the write, so the writes happen in the order of their locks.
        let mut written = lock(&self.written);
        if generation <= *written {
            return Ok(());
        }
        dir.save_saved_queries(queries)
            .map_err(|err| err.to_string())?;
        *written = generation;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use sqler_config::{HistoryOutcome, SavedQuery, SavedQueryDraft};
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::sidebar::SidebarModel;
    use crate::tabs::{TabConnection, TabId, TabKind, TabStrip};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sqler-hs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conns() -> Vec<sqler_config::ConnectionId> {
        let driver = DriverId::new("sqlite").unwrap();
        SidebarModel::from_connections(vec![
            ConnectionConfig::new("a", driver.clone(), ":memory:"),
            ConnectionConfig::new("b", driver, ":memory:"),
        ])
        .entries()
        .iter()
        .map(|e| e.id().clone())
        .collect()
    }

    fn tab() -> TabId {
        let connection = TabConnection {
            name: "t".to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        TabStrip::default().open(connection, TabKind::History)
    }

    fn new_entry(sql: &str, connection: &sqler_config::ConnectionId) -> NewHistoryEntry {
        NewHistoryEntry {
            sql: sql.to_owned(),
            connection: connection.clone(),
            started_at: SystemTime::now(),
            duration: Duration::from_millis(3),
            outcome: HistoryOutcome::Ok,
            rows: Some(1),
        }
    }

    fn search(text: &str, connection: Option<&sqler_config::ConnectionId>) -> HistorySearch {
        HistorySearch {
            tab: tab(),
            request: 1,
            text: text.to_owned(),
            connection: connection.cloned(),
            limit: 100,
        }
    }

    fn service(dir: &std::path::Path) -> HistoryService {
        HistoryService::new(Some(ConfigDir::new(dir)), HistoryOptions::default())
    }

    #[test]
    fn records_then_searches_newest_first() {
        let dir = temp_dir("rec");
        let ids = conns();
        let s = service(&dir);
        assert_eq!(s.open().unwrap(), Vec::<String>::new());
        s.record(new_entry("select 1", &ids[0])).unwrap();
        s.record(new_entry("select 2", &ids[0])).unwrap();
        s.record(new_entry("update t set a = 1", &ids[1])).unwrap();
        let all = s.search(&search("", None));
        let sqls: Vec<_> = all.iter().map(HistoryEntry::sql).collect();
        assert_eq!(sqls, ["update t set a = 1", "select 2", "select 1"]);
        let only_a = s.search(&search("", Some(&ids[0])));
        assert_eq!(only_a.len(), 2);
        let text = s.search(&search("UPDATE", None));
        assert_eq!(text.len(), 1);
        let mut limited = search("", None);
        limited.limit = 1;
        assert_eq!(s.search(&limited).len(), 1);
        // A second service over the same directory sees the file.
        assert_eq!(service(&dir).search(&search("", None)).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_method_opens_the_store_lazily() {
        let dir = temp_dir("lazy");
        let ids = conns();
        let s = service(&dir);
        // No I/O before the first call.
        assert!(!dir.exists());
        s.record(new_entry("select 1", &ids[0])).unwrap();
        assert!(dir.join("history.jsonl").exists());
        let other = service(&dir);
        assert_eq!(other.search(&search("", None)).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_everything() {
        let dir = temp_dir("clear");
        let ids = conns();
        let s = service(&dir);
        s.record(new_entry("select 1", &ids[0])).unwrap();
        s.clear().unwrap();
        assert_eq!(s.search(&search("", None)).len(), 0);
        assert_eq!(service(&dir).search(&search("", None)).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_directory_history_is_a_no_op_and_saved_queries_fail() {
        let s = HistoryService::new(None, HistoryOptions::default());
        let ids = conns();
        assert_eq!(s.open().unwrap(), Vec::<String>::new());
        s.record(new_entry("select 1", &ids[0])).unwrap();
        assert_eq!(s.search(&search("", None)).len(), 0);
        s.clear().unwrap();
        assert_eq!(
            s.load_saved().unwrap_err(),
            "No config directory: saved queries are not available"
        );
        assert!(s.save_saved(1, &SavedQueries::new()).is_err());
    }

    #[test]
    fn a_corrupt_line_becomes_a_notice_without_its_content() {
        let dir = temp_dir("corrupt");
        let ids = conns();
        let s = service(&dir);
        s.record(new_entry("select 1", &ids[0])).unwrap();
        let path = dir.join("history.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("this is not json, hunter2\n");
        std::fs::write(&path, text).unwrap();

        let s = service(&dir);
        let notices = s.open().unwrap();
        assert_eq!(
            notices,
            ["history.jsonl: 1 line could not be read; they are kept in the file"]
        );
        assert!(!notices[0].contains("hunter2"));
        assert!(s.open().unwrap().is_empty(), "reported once");
        assert_eq!(s.search(&search("", None)).len(), 1);
        // A notice is also kept when another call opened the store first.
        let s = service(&dir);
        s.record(new_entry("select 2", &ids[0])).unwrap();
        assert_eq!(s.open().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_queries_round_trip_and_skip_older_generations() {
        let dir = temp_dir("saved");
        let s = service(&dir);
        assert_eq!(s.load_saved().unwrap().queries().len(), 0);
        let mut q = SavedQueries::new();
        q.create(SavedQueryDraft::new("one", "select 1")).unwrap();
        s.save_saved(1, &q).unwrap();
        q.create(SavedQueryDraft::new("two", "select 2")).unwrap();
        s.save_saved(3, &q).unwrap();
        // An older snapshot arriving late does not win.
        let mut old = SavedQueries::new();
        old.create(SavedQueryDraft::new("old", "select 0")).unwrap();
        s.save_saved(2, &old).unwrap();
        s.save_saved(3, &old).unwrap();
        let loaded = s.load_saved().unwrap();
        let names: Vec<_> = loaded.queries().iter().map(SavedQuery::name).collect();
        assert_eq!(names, ["one", "two"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_queries_file_is_an_error_and_stays_untouched() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("queries.toml"), "version = [").unwrap();
        let s = service(&dir);
        assert!(s.load_saved().is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("queries.toml")).unwrap(),
            "version = ["
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
