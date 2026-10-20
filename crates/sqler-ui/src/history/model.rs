// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The state of the history tab, the saved-queries list and their dialogs.

use std::time::{Duration, SystemTime};

use sqler_config::{
    ConnectionId, HistoryEntry, HistoryOutcome, SavedQueries, SavedQuery, SavedQueryId,
};

use crate::grid::group_thousands;
use crate::tabs::TabId;

/// How many of the newest matches a history search returns.
pub const HISTORY_PAGE: usize = 500;

/// The longest SQL preview of a history row, in characters.
const PREVIEW_CHARS: usize = 200;

/// Which entries a history tab lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryScope {
    /// Only the connection the tab is bound to.
    Connection,
    /// The entries of every connection.
    All,
}

/// A search the view runs on the store (off the UI thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySearch {
    /// The history tab that asked.
    pub tab: TabId,
    /// Tells this search from an older one of the same tab; report it back unchanged.
    pub request: u64,
    /// The search text: whitespace-separated terms that must all occur in the SQL.
    pub text: String,
    /// Only entries of this connection; `None` for all connections.
    pub connection: Option<ConnectionId>,
    /// At most this many results.
    pub limit: usize,
}

/// The state of one history tab.
#[derive(Debug, Clone)]
pub struct HistoryTab {
    connection: ConnectionId,
    scope: HistoryScope,
    text: String,
    request: u64,
    results: Vec<HistoryEntry>,
    selected: Option<usize>,
    searching: bool,
}

impl HistoryTab {
    /// A tab bound to `connection`: this connection only, no search text, no results yet.
    pub(crate) fn new(connection: ConnectionId) -> Self {
        Self {
            connection,
            scope: HistoryScope::Connection,
            text: String::new(),
            request: 0,
            results: Vec::new(),
            selected: None,
            searching: false,
        }
    }

    /// The connection the tab is bound to.
    #[must_use]
    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    /// Which entries the tab lists.
    #[must_use]
    pub fn scope(&self) -> HistoryScope {
        self.scope
    }

    /// The search text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The request number of the latest search.
    #[must_use]
    pub fn request(&self) -> u64 {
        self.request
    }

    /// The entries of the latest finished search, newest first.
    #[must_use]
    pub fn results(&self) -> &[HistoryEntry] {
        &self.results
    }

    /// The selected entry (an index into [`results`](Self::results)).
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Whether a search is waiting for its answer.
    #[must_use]
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// Sets the search text; `true` if it changed.
    pub(crate) fn set_text(&mut self, text: String) -> bool {
        let changed = self.text != text;
        self.text = text;
        changed
    }

    /// Sets the scope; `true` if it changed.
    pub(crate) fn set_scope(&mut self, scope: HistoryScope) -> bool {
        let changed = self.scope != scope;
        self.scope = scope;
        changed
    }

    /// Starts a search with request number `request` for the current text and scope.
    pub(crate) fn begin_search(&mut self, tab: TabId, request: u64) -> HistorySearch {
        self.request = request;
        self.searching = true;
        HistorySearch {
            tab,
            request,
            text: self.text.clone(),
            connection: match self.scope {
                HistoryScope::Connection => Some(self.connection.clone()),
                HistoryScope::All => None,
            },
            limit: HISTORY_PAGE,
        }
    }

    /// Takes the answer of search `request`; a stale answer is ignored (`false`).
    pub(crate) fn accept(&mut self, request: u64, results: Vec<HistoryEntry>) -> bool {
        if request != self.request {
            return false;
        }
        self.results = results;
        self.searching = false;
        if self.selected.is_some_and(|i| i >= self.results.len()) {
            self.selected = None;
        }
        true
    }

    /// Selects entry `index`; `false` if there is none.
    pub(crate) fn select(&mut self, index: usize) -> bool {
        if index < self.results.len() {
            self.selected = Some(index);
            true
        } else {
            false
        }
    }

    /// Drops the results (the history was cleared) while keeping text and scope.
    pub(crate) fn forget_results(&mut self) {
        self.results.clear();
        self.selected = None;
    }
}

/// One line of the history list, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// How long ago it ran: `just now`, `N min ago`, `N h ago` or `N d ago`.
    pub when: String,
    /// The label of the connection it ran on.
    pub connection: String,
    /// `OK`, `Error`, `Cancelled` or `Other`.
    pub outcome: &'static str,
    /// The duration, and the row count when it is known: `12 ms · 1,024 rows`.
    pub detail: String,
    /// The first line of the SQL, cut to 200 characters.
    pub sql: String,
    /// Whether the stored SQL was cut when it was recorded.
    pub truncated: bool,
}

/// The display row of `entry`; `connection_label` names its connection, `now` is the clock.
#[must_use]
pub fn history_row(entry: &HistoryEntry, connection_label: &str, now: SystemTime) -> HistoryRow {
    HistoryRow {
        when: age(now
            .duration_since(entry.started_at())
            .unwrap_or(Duration::ZERO)),
        connection: connection_label.to_owned(),
        outcome: match entry.outcome() {
            HistoryOutcome::Ok => "OK",
            HistoryOutcome::Error => "Error",
            HistoryOutcome::Cancelled => "Cancelled",
            _ => "Other",
        },
        detail: detail(entry.duration(), entry.rows()),
        sql: preview(entry.sql()),
        truncated: entry.truncated(),
    }
}

fn age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn detail(duration: Duration, rows: Option<u64>) -> String {
    let time = if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{:.1} s", duration.as_secs_f64())
    };
    match rows {
        Some(n) => {
            let n = usize::try_from(n).unwrap_or(usize::MAX);
            let noun = if n == 1 { "row" } else { "rows" };
            format!("{time} · {} {noun}", group_thousands(n))
        }
        None => time,
    }
}

/// The first line of `sql` with at most [`PREVIEW_CHARS`] characters; `…` marks a cut.
fn preview(sql: &str) -> String {
    let sql = sql.trim();
    let (line, more) = match sql.split_once('\n') {
        Some((line, rest)) => (line.trim_end(), !rest.trim().is_empty()),
        None => (sql, false),
    };
    let chars = line.chars().count();
    if chars <= PREVIEW_CHARS && !more {
        return line.to_owned();
    }
    let keep = chars.min(PREVIEW_CHARS - 1);
    let mut out: String = line.chars().take(keep).collect();
    out.push('…');
    out
}

/// Whether the saved queries could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedLoad {
    /// `queries.toml` is being read.
    Loading,
    /// The list is loaded and may be changed.
    Ready,
    /// The file could not be read (the reason). Nothing is written over it.
    Failed(String),
}

/// The saved queries and what the user selected in them.
#[derive(Debug, Clone)]
pub struct SavedState {
    load: SavedLoad,
    queries: SavedQueries,
    selected: Option<SavedQueryId>,
    generation: u64,
}

impl SavedState {
    /// An empty list that is still loading.
    pub(crate) fn new() -> Self {
        Self {
            load: SavedLoad::Loading,
            queries: SavedQueries::new(),
            selected: None,
            generation: 0,
        }
    }

    /// Whether the list was loaded.
    #[must_use]
    pub fn load(&self) -> &SavedLoad {
        &self.load
    }

    /// The saved queries, in list order.
    #[must_use]
    pub fn queries(&self) -> &SavedQueries {
        &self.queries
    }

    /// The selected saved query.
    #[must_use]
    pub fn selected(&self) -> Option<&SavedQueryId> {
        self.selected.as_ref()
    }

    /// The number of the latest change; a write of an older one is skipped.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn set_load(&mut self, load: SavedLoad) {
        self.load = load;
    }

    pub(crate) fn replace(&mut self, queries: SavedQueries) {
        self.queries = queries;
        self.selected = None;
    }

    pub(crate) fn queries_mut(&mut self) -> &mut SavedQueries {
        &mut self.queries
    }

    pub(crate) fn select(&mut self, id: Option<SavedQueryId>) {
        self.selected = id;
    }

    /// Counts a change and returns its number.
    pub(crate) fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
}

/// What a name dialog is for.
#[derive(Clone, PartialEq, Eq)]
pub enum NamePurpose {
    /// Save the SQL as a query.
    Save {
        /// The editor tab the SQL came from.
        tab: TabId,
        /// The text to save.
        sql: String,
        /// The connection of the editor tab; the new query is bound to it.
        connection: Option<ConnectionId>,
        /// The saved query this tab was opened from; the same name overwrites its SQL.
        replaces: Option<SavedQueryId>,
    },
    /// Rename this saved query.
    Rename(SavedQueryId),
}

impl std::fmt::Debug for NamePurpose {
    /// Never prints the SQL, only its length.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Save {
                tab,
                sql,
                connection,
                replaces,
            } => f
                .debug_struct("Save")
                .field("tab", tab)
                .field("connection", connection)
                .field("replaces", replaces)
                .field("sql_len", &sql.len())
                .finish(),
            Self::Rename(id) => f.debug_tuple("Rename").field(id).finish(),
        }
    }
}

/// A dialog of the history feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryDialog {
    /// Asks for a name.
    Name {
        /// What the name is for.
        purpose: NamePurpose,
        /// The name typed so far.
        text: String,
        /// Why the last attempt was rejected; the dialog stays open.
        error: Option<String>,
    },
    /// Asks whether to delete a saved query.
    ConfirmDelete {
        /// The query.
        id: SavedQueryId,
        /// Its name, for the question.
        name: String,
    },
    /// Asks whether to delete the whole history.
    ConfirmClear,
}

/// The "open saved query" picker: a filter box over the saved queries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedPicker {
    filter: String,
    selected: usize,
}

impl SavedPicker {
    /// A picker with no filter and the first query selected.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The filter text.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The highlighted position in [`matches`](Self::matches).
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The queries that match the filter, in list order. Every whitespace-separated term must be
    /// a case-insensitive substring of `folder/name` (just `name` for the top level).
    #[must_use]
    pub fn matches<'a>(&self, queries: &'a SavedQueries) -> Vec<&'a SavedQuery> {
        let terms: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        queries
            .queries()
            .iter()
            .filter(|q| {
                let path = match q.folder() {
                    Some(folder) => format!("{folder}/{}", q.name()),
                    None => q.name().to_owned(),
                }
                .to_lowercase();
                terms.iter().all(|t| path.contains(t.as_str()))
            })
            .collect()
    }

    /// The highlighted query, if the filter leaves any.
    #[must_use]
    pub fn current<'a>(&self, queries: &'a SavedQueries) -> Option<&'a SavedQuery> {
        self.matches(queries).get(self.selected).copied()
    }

    pub(crate) fn set_filter(&mut self, filter: String) {
        if self.filter != filter {
            self.filter = filter;
            self.selected = 0;
        }
    }

    /// Moves the highlight by `delta` among `count` matches, stopping at both ends.
    pub(crate) fn step(&mut self, delta: isize, count: usize) {
        let last = count.saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }
}

#[cfg(test)]
mod tests {
    use sqler_config::{HistoryOptions, HistoryStore, NewHistoryEntry, SavedQueryDraft};
    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::sidebar::SidebarModel;
    use crate::tabs::{TabConnection, TabKind, TabStrip};

    fn conn() -> ConnectionId {
        let driver = DriverId::new("sqlite").unwrap();
        SidebarModel::from_connections(vec![ConnectionConfig::new("dev", driver, ":memory:")])
            .entries()[0]
            .id()
            .clone()
    }

    #[test]
    fn debug_of_the_name_dialog_never_prints_the_sql() {
        let dialog = HistoryDialog::Name {
            purpose: NamePurpose::Save {
                tab: tab_id(),
                sql: "select 'hunter2'".to_owned(),
                connection: None,
                replaces: None,
            },
            text: String::new(),
            error: None,
        };
        let text = format!("{dialog:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("sql_len"), "{text}");
    }

    fn tab_id() -> TabId {
        let connection = TabConnection {
            name: "t".to_owned(),
            driver: DriverId::new("sqlite").unwrap(),
            color: None,
            environment: None,
        };
        TabStrip::default().open(connection, TabKind::History)
    }

    /// The store keeps `HistoryEntry` fields private, so entries come from a real store.
    fn entry(
        name: &str,
        sql: &str,
        outcome: HistoryOutcome,
        duration: Duration,
        rows: Option<u64>,
        started_at: SystemTime,
    ) -> HistoryEntry {
        let dir = std::env::temp_dir().join(format!("sqler-hm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store =
            HistoryStore::open(dir.join("history.jsonl"), HistoryOptions::default()).unwrap();
        store
            .record(NewHistoryEntry {
                sql: sql.to_owned(),
                connection: conn(),
                started_at,
                duration,
                outcome,
                rows,
            })
            .unwrap();
        let found = store.search(&sqler_config::HistoryQuery::default());
        let _ = std::fs::remove_dir_all(&dir);
        found.into_iter().next().unwrap()
    }

    fn row(name: &str, sql: &str, ago: u64, duration: Duration, rows: Option<u64>) -> HistoryRow {
        let now = SystemTime::now();
        let started = now - Duration::from_secs(ago);
        let e = entry(name, sql, HistoryOutcome::Ok, duration, rows, started);
        history_row(&e, "dev", now)
    }

    #[test]
    fn ages_are_rounded_down() {
        let cases = [
            (0, "just now"),
            (59, "just now"),
            (60, "1 min ago"),
            (3599, "59 min ago"),
            (3600, "1 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 d ago"),
            (86_400 * 40, "40 d ago"),
        ];
        for (secs, text) in cases {
            assert_eq!(age(Duration::from_secs(secs)), text);
        }
        let r = row("age", "select 1", 7200, Duration::ZERO, None);
        assert_eq!(r.when, "2 h ago");
    }

    #[test]
    fn an_entry_from_the_future_is_just_now() {
        let now = SystemTime::now();
        let e = entry(
            "future",
            "select 1",
            HistoryOutcome::Ok,
            Duration::ZERO,
            None,
            now + Duration::from_secs(3600),
        );
        assert_eq!(history_row(&e, "dev", now).when, "just now");
    }

    #[test]
    fn detail_shows_time_and_known_rows() {
        let r = row("d1", "select 1", 0, Duration::from_millis(12), None);
        assert_eq!(r.detail, "12 ms");
        let r = row(
            "d2",
            "select 1",
            0,
            Duration::from_millis(1234),
            Some(1_024),
        );
        assert_eq!(r.detail, "1.2 s · 1,024 rows");
        let r = row("d3", "select 1", 0, Duration::from_millis(5), Some(1));
        assert_eq!(r.detail, "5 ms · 1 row");
        let r = row("d4", "select 1", 0, Duration::from_millis(5), Some(0));
        assert_eq!(r.detail, "5 ms · 0 rows");
    }

    #[test]
    fn outcomes_have_labels() {
        let now = SystemTime::now();
        for (name, outcome, label) in [
            ("o1", HistoryOutcome::Ok, "OK"),
            ("o2", HistoryOutcome::Error, "Error"),
            ("o3", HistoryOutcome::Cancelled, "Cancelled"),
        ] {
            let e = entry(name, "select 1", outcome, Duration::ZERO, None, now);
            let r = history_row(&e, "label", now);
            assert_eq!(r.outcome, label);
            assert_eq!(r.connection, "label");
            assert!(!r.truncated);
        }
    }

    #[test]
    fn the_preview_is_the_first_line() {
        let r = row("p1", "select 1", 0, Duration::ZERO, None);
        assert_eq!(r.sql, "select 1");
        let r = row("p2", "select 1\nfrom t", 0, Duration::ZERO, None);
        assert_eq!(r.sql, "select 1…");
        let r = row("p3", "\n  select 1  \n\n", 0, Duration::ZERO, None);
        assert_eq!(r.sql, "select 1");
    }

    #[test]
    fn the_preview_is_cut_at_200_characters() {
        assert_eq!(preview(&"a".repeat(200)), "a".repeat(200));
        let long = preview(&"é".repeat(300));
        assert_eq!(long.chars().count(), 200);
        assert!(long.ends_with('…'));
        let multi = preview(&format!("{}\nmore", "b".repeat(200)));
        assert_eq!(multi.chars().count(), 200);
        assert!(multi.ends_with('…'));
    }

    #[test]
    fn a_search_bumps_the_request_and_scopes_the_connection() {
        let id = conn();
        let tab = tab_id();
        let mut t = HistoryTab::new(id.clone());
        assert_eq!(t.scope(), HistoryScope::Connection);
        assert!(!t.searching());
        let s = t.begin_search(tab, 4);
        assert_eq!(s.connection.as_ref(), Some(&id));
        assert_eq!(s.limit, HISTORY_PAGE);
        assert_eq!(s.request, 4);
        assert!(t.searching());
        assert!(t.set_scope(HistoryScope::All));
        assert!(!t.set_scope(HistoryScope::All));
        assert!(t.set_text("select".to_owned()));
        let s = t.begin_search(tab, 5);
        assert_eq!(s.connection, None);
        assert_eq!(s.text, "select");
        assert_eq!(t.request(), 5);
    }

    #[test]
    fn a_stale_answer_is_ignored() {
        let mut t = HistoryTab::new(conn());
        let tab = tab_id();
        t.begin_search(tab, 1);
        t.begin_search(tab, 2);
        let e = entry(
            "stale",
            "select 1",
            HistoryOutcome::Ok,
            Duration::ZERO,
            None,
            SystemTime::now(),
        );
        assert!(!t.accept(1, vec![e.clone()]));
        assert!(t.searching());
        assert_eq!(t.results().len(), 0);
        assert!(t.accept(2, vec![e]));
        assert!(!t.searching());
        assert_eq!(t.results().len(), 1);
        assert!(t.select(0));
        assert!(!t.select(1));
        assert_eq!(t.selected(), Some(0));
        t.begin_search(tab, 3);
        assert!(t.accept(3, Vec::new()));
        assert_eq!(t.selected(), None);
    }

    fn saved(entries: &[(&str, Option<&str>)]) -> SavedQueries {
        let mut q = SavedQueries::new();
        for (name, folder) in entries {
            q.create(SavedQueryDraft {
                folder: folder.map(str::to_owned),
                ..SavedQueryDraft::new(*name, "select 1")
            })
            .unwrap();
        }
        q
    }

    fn names(q: &[&SavedQuery]) -> Vec<String> {
        q.iter().map(|q| q.name().to_owned()).collect()
    }

    #[test]
    fn the_picker_matches_every_term_in_the_path_in_list_order() {
        let q = saved(&[
            ("Daily report", Some("sales")),
            ("Users", None),
            ("Top customers", Some("Sales/EU")),
            ("Archive", Some("ops")),
        ]);
        let mut p = SavedPicker::new();
        assert_eq!(p.matches(&q).len(), 4);
        p.set_filter("sales".to_owned());
        assert_eq!(names(&p.matches(&q)), ["Daily report", "Top customers"]);
        p.set_filter("EU top".to_owned());
        assert_eq!(names(&p.matches(&q)), ["Top customers"]);
        p.set_filter("sales/daily".to_owned());
        assert_eq!(names(&p.matches(&q)), ["Daily report"]);
        // The folder is not part of a top-level name.
        p.set_filter("/users".to_owned());
        assert_eq!(p.matches(&q).len(), 0);
        p.set_filter("   ".to_owned());
        assert_eq!(p.matches(&q).len(), 4);
    }

    #[test]
    fn the_picker_highlight_stops_at_the_ends_and_resets_with_the_filter() {
        let q = saved(&[("a", None), ("b", None), ("c", None)]);
        let mut p = SavedPicker::new();
        p.step(-1, 3);
        assert_eq!(p.selected(), 0);
        p.step(1, 3);
        p.step(5, 3);
        assert_eq!(p.selected(), 2);
        assert_eq!(p.current(&q).unwrap().name(), "c");
        p.set_filter("b".to_owned());
        assert_eq!(p.selected(), 0);
        assert_eq!(p.current(&q).unwrap().name(), "b");
        p.step(1, 1);
        assert_eq!(p.selected(), 0);
        p.set_filter("zzz".to_owned());
        p.step(1, 0);
        assert!(p.current(&q).is_none());
    }

    #[test]
    fn saved_state_counts_changes() {
        let mut s = SavedState::new();
        assert_eq!(*s.load(), SavedLoad::Loading);
        assert_eq!(s.generation(), 0);
        assert_eq!(s.bump(), 1);
        assert_eq!(s.bump(), 2);
        s.replace(saved(&[("a", None)]));
        assert!(s.selected().is_none());
        assert_eq!(s.queries().queries().len(), 1);
    }
}
