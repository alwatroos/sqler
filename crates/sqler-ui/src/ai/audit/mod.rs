// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI audit log viewer model (ADR-0037, ADR-0050): filters, pages and rows.
//!
//! Plain Rust, no `GPUI`. The viewer shows only what `sqler-ai-tools` already scrubbed when it
//! wrote the entry: statement SQL, tool, connection name, enums and counts. It never shows the
//! `arguments` (apart from the `sql` string) and never the `error.message`. `Debug` of every
//! type here leaves the SQL out.

use sqler_ai_tools::{
    ApprovalOutcome, AuditCursor, AuditEntry, AuditPage, AuditQuery, CallOutcome, Surface, names,
};

use crate::grid::{group_thousands, human_bytes};

#[cfg(any(test, feature = "screenshots"))]
pub mod fixture;

/// How many entries one page holds.
pub const AUDIT_PAGE: usize = 200;

/// Every tool name, in the order of the tool schema.
pub const AUDIT_TOOLS: &[&str] = &[
    names::LIST_CONNECTIONS,
    names::LIST_SCHEMAS,
    names::LIST_TABLES,
    names::DESCRIBE_TABLE,
    names::DESCRIBE_FUNCTION,
    names::SEARCH_SCHEMA,
    names::SAMPLE_ROWS,
    names::RUN_QUERY,
    names::EXPLAIN_QUERY,
    names::OPEN_IN_EDITOR,
    names::OPEN_RESULTS_TAB,
    names::SEARCH_DATA,
];

/// The surfaces the filter offers.
pub const AUDIT_SURFACES: [Surface; 4] = [
    Surface::Assist,
    Surface::Session,
    Surface::Mcp,
    Surface::Agent,
];

/// The approval outcomes the filter offers.
pub const AUDIT_DECISIONS: [ApprovalOutcome; 5] = [
    ApprovalOutcome::Approved,
    ApprovalOutcome::Denied,
    ApprovalOutcome::Refused,
    ApprovalOutcome::NotNeeded,
    ApprovalOutcome::NotAsked,
];

/// The most characters of SQL a row shows.
const SQL_PREVIEW_CHARS: usize = 200;

/// The viewer filters; `None` means all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditFilters {
    /// Only this connection id.
    pub connection: Option<String>,
    /// Only this surface.
    pub surface: Option<Surface>,
    /// Only this tool.
    pub tool: Option<String>,
    /// Only this call-level approval outcome.
    pub decision: Option<ApprovalOutcome>,
}

impl AuditFilters {
    /// Whether no filter is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One edit of the filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditFilterChange {
    /// The connection id.
    Connection(Option<String>),
    /// The surface.
    Surface(Option<Surface>),
    /// The tool.
    Tool(Option<String>),
    /// The approval outcome.
    Decision(Option<ApprovalOutcome>),
}

/// A read the view has to run on the blocking pool.
pub struct AuditRead {
    /// The request number to report back with.
    pub request: u64,
    /// What to read.
    pub query: AuditQuery,
}

impl std::fmt::Debug for AuditRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditRead")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

/// The answer to an [`AuditRead`].
#[derive(Default)]
pub struct AuditBatch {
    /// The entries, newest first.
    pub entries: Vec<AuditEntry>,
    /// The cursor of the next page; `None` at the end.
    pub next: Option<AuditCursor>,
    /// Lines that did not parse.
    pub skipped_lines: usize,
    /// Lines written by a newer version.
    pub newer_lines: usize,
}

impl From<AuditPage> for AuditBatch {
    fn from(p: AuditPage) -> Self {
        Self {
            entries: p.entries,
            next: p.next,
            skipped_lines: p.skipped_lines,
            newer_lines: p.newer_lines,
        }
    }
}

/// Where the list stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AuditLoad {
    /// Nothing was read yet (the view was restored at startup).
    #[default]
    NotLoaded,
    /// A read is running.
    Loading,
    /// The last read succeeded.
    Ready,
    /// The last read failed with this text.
    Failed(String),
}

/// The audit viewer state.
#[derive(Default)]
pub struct AuditPanel {
    filters: AuditFilters,
    load: AuditLoad,
    entries: Vec<AuditEntry>,
    next: Option<AuditCursor>,
    skipped: usize,
    newer: usize,
    selected: Option<usize>,
    request: u64,
    pending_more: bool,
    bound: Option<String>,
    page: Option<usize>,
}

// Never the SQL, the arguments or the error texts.
impl std::fmt::Debug for AuditPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditPanel")
            .field("filters", &self.filters)
            .field("entries", &self.entries.len())
            .field("selected", &self.selected)
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

impl AuditPanel {
    /// The filters.
    #[must_use]
    pub fn filters(&self) -> &AuditFilters {
        &self.filters
    }

    /// Where the list stands.
    #[must_use]
    pub fn load(&self) -> &AuditLoad {
        &self.load
    }

    /// The loaded entries, newest first.
    #[must_use]
    pub fn entries(&self) -> &[AuditEntry] {
        &self.entries
    }

    /// Whether another page can be read.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.next.is_some()
    }

    /// The selected row.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Lines skipped (unreadable) and lines from a newer version, in the last read.
    #[must_use]
    pub fn notes(&self) -> (usize, usize) {
        (self.skipped, self.newer)
    }

    /// The connection id the connection filter was bound to when the view opened.
    #[must_use]
    pub fn bound(&self) -> Option<&str> {
        self.bound.as_deref()
    }

    /// Binds the connection filter to the connection the user works on (`None` = all).
    pub(crate) fn bind(&mut self, connection: Option<String>) {
        self.filters.connection.clone_from(&connection);
        self.bound = connection;
    }

    fn page_size(&self) -> usize {
        self.page.unwrap_or(AUDIT_PAGE)
    }

    /// Smaller pages for tests.
    #[cfg(test)]
    pub(crate) fn set_page_size(&mut self, n: usize) {
        self.page = Some(n);
    }

    /// Starts a read: the first page, or the next one when `more` and there is a cursor. Every
    /// call supersedes the reads before it.
    pub fn begin(&mut self, more: bool) -> AuditRead {
        let more = more && self.next.is_some();
        if !more {
            // The old cursor belongs to the old filters: no load-more until the first page lands.
            self.next = None;
        }
        self.request += 1;
        self.pending_more = more;
        self.load = AuditLoad::Loading;
        let mut query = AuditQuery::new().with_limit(self.page_size());
        if let Some(c) = &self.filters.connection {
            query = query.with_connection(c.clone());
        }
        if let Some(s) = self.filters.surface {
            query = query.with_surface(s);
        }
        if let Some(t) = &self.filters.tool {
            query = query.with_tool(t.clone());
        }
        if let Some(d) = self.filters.decision {
            query = query.with_approval(d);
        }
        if more && let Some(c) = &self.next {
            query = query.with_cursor(c.clone());
        }
        AuditRead {
            request: self.request,
            query,
        }
    }

    /// Takes the answer of a read. Returns `false` (and changes nothing) when a newer read has
    /// started since.
    pub fn finish(&mut self, request: u64, r: Result<AuditBatch, String>) -> bool {
        if request != self.request {
            return false;
        }
        match r {
            Ok(batch) => {
                if self.pending_more {
                    self.entries.extend(batch.entries);
                } else {
                    self.entries = batch.entries;
                    self.selected = None;
                }
                self.next = batch.next;
                self.skipped = batch.skipped_lines;
                self.newer = batch.newer_lines;
                self.load = AuditLoad::Ready;
            }
            Err(text) => {
                if !self.pending_more {
                    // A failed first page must not leave the old rows (and a
                    // copyable selection) under the new filters.
                    self.entries.clear();
                    self.selected = None;
                    self.next = None;
                }
                self.load = AuditLoad::Failed(text);
            }
        }
        true
    }

    /// Edits one filter. Returns whether it changed.
    pub fn set_filter(&mut self, change: AuditFilterChange) -> bool {
        let before = self.filters.clone();
        match change {
            AuditFilterChange::Connection(c) => self.filters.connection = c,
            AuditFilterChange::Surface(s) => self.filters.surface = s,
            AuditFilterChange::Tool(t) => self.filters.tool = t,
            AuditFilterChange::Decision(d) => self.filters.decision = d,
        }
        before != self.filters
    }

    /// Selects a row; out of range clears the selection.
    pub fn select(&mut self, ix: usize) {
        self.selected = (ix < self.entries.len()).then_some(ix);
    }

    /// The SQL of the selected entry, as stored.
    #[must_use]
    pub fn selected_sql(&self) -> Option<String> {
        self.entries.get(self.selected?).and_then(entry_sql)
    }

    /// Replaces the list (screenshots and tests) and drops any read in flight.
    #[cfg(any(test, feature = "screenshots"))]
    pub(crate) fn set_fixture(&mut self, entries: Vec<AuditEntry>) {
        self.request += 1;
        self.entries = entries;
        self.next = None;
        self.selected = None;
        self.skipped = 0;
        self.newer = 0;
        self.load = AuditLoad::Ready;
    }
}

/// How the tone of a decision is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditTone {
    /// Neutral.
    Muted,
    /// Approved.
    Success,
    /// Denied or refused.
    Danger,
}

/// What one list row shows.
#[derive(Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// How long ago the call started.
    pub when: String,
    /// The surface.
    pub surface: &'static str,
    /// The connection name, or `—`.
    pub connection: String,
    /// The tool.
    pub tool: String,
    /// The first line of the first statement, cut.
    pub sql: String,
    /// How many statements follow the first.
    pub more_statements: usize,
    /// The approval outcome.
    pub decision: &'static str,
    /// The tone of the decision.
    pub tone: AuditTone,
    /// Rows and size, or the error code.
    pub result: String,
}

// Never the SQL text.
impl std::fmt::Debug for AuditRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditRow")
            .field("surface", &self.surface)
            .field("connection", &self.connection)
            .field("tool", &self.tool)
            .field("sql_len", &self.sql.len())
            .field("decision", &self.decision)
            .finish_non_exhaustive()
    }
}

/// A surface for people.
#[must_use]
pub fn surface_label(s: Surface) -> &'static str {
    match s {
        Surface::Assist => "In-app assistant",
        Surface::Session => "In-app session",
        Surface::Mcp => "MCP",
        Surface::Agent => "ACP agent",
        Surface::Unspecified => "Unspecified",
        _ => "Other",
    }
}

/// An approval outcome for people.
#[must_use]
pub fn decision_label(d: ApprovalOutcome) -> &'static str {
    match d {
        ApprovalOutcome::NotNeeded => "Not needed",
        ApprovalOutcome::Approved => "Approved",
        ApprovalOutcome::Denied => "Denied",
        ApprovalOutcome::Refused => "Refused",
        ApprovalOutcome::NotAsked => "Not asked",
        _ => "Other",
    }
}

/// A wire name for people: `cancelled` becomes `Cancelled`, `not_asked` becomes `Not asked`.
#[must_use]
pub fn wire_label(wire: &str) -> String {
    let text = wire.replace('_', " ");
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}

fn age(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn sql_argument(e: &AuditEntry) -> Option<&str> {
    e.arguments.get("sql").and_then(|v| v.as_str())
}

/// The list row of an entry. `now_ms` is the current Unix time in milliseconds.
#[must_use]
pub fn audit_row(e: &AuditEntry, now_ms: u64) -> AuditRow {
    let first = e
        .statements
        .first()
        .map(|s| s.sql.as_str())
        .or_else(|| sql_argument(e))
        .unwrap_or("");
    let sql: String = first
        .trim_start()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(SQL_PREVIEW_CHARS)
        .collect();
    let tone = match e.approval {
        ApprovalOutcome::Denied | ApprovalOutcome::Refused => AuditTone::Danger,
        ApprovalOutcome::Approved => AuditTone::Success,
        _ => AuditTone::Muted,
    };
    let result = match e.outcome {
        CallOutcome::Ok => format!(
            "{} rows · {}",
            group_thousands(usize::try_from(e.rows).unwrap_or(usize::MAX)),
            human_bytes(usize::try_from(e.result_bytes).unwrap_or(usize::MAX))
        ),
        CallOutcome::Error => format!(
            "Error: {}",
            e.error.as_ref().map_or("", |f| f.code.as_str())
        ),
        other => wire_label(other.as_str()),
    };
    AuditRow {
        when: age(now_ms.saturating_sub(e.ts_ms) / 1000),
        surface: surface_label(e.surface),
        connection: e
            .connection
            .as_ref()
            .map_or_else(|| "—".to_owned(), |c| c.name.clone()),
        tool: e.tool.clone(),
        sql,
        more_statements: e.statements.len().saturating_sub(1),
        decision: decision_label(e.approval),
        tone,
        result,
    }
}

/// The SQL of an entry for copying: the statements joined with `;` and a line break, each
/// ending in `;`. Without statements, the `sql` argument as stored. Never other arguments.
#[must_use]
pub fn entry_sql(e: &AuditEntry) -> Option<String> {
    let parts: Vec<&str> = e
        .statements
        .iter()
        .map(|s| {
            s.sql
                .trim_end_matches(|c: char| c == ';' || c.is_whitespace())
        })
        .filter(|s| !s.trim().is_empty())
        .collect();
    if parts.is_empty() {
        return sql_argument(e)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
    }
    Some(format!("{};", parts.join(";\n")))
}

#[cfg(test)]
mod tests;
