// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The editor AI actions (ADR-0052): generate SQL, explain a query, fix a failed statement.
//!
//! Plain Rust, no `GPUI`. [`EditorAssist`] holds the state of every query tab; the workspace
//! (`workspace/assist.rs`) gates a request and returns an effect, the view runs [`run_assist`]
//! and reports back. Generated SQL only ever enters the editor through an `EditorEdit`; nothing
//! here runs it.

mod job;
mod source;
#[cfg(feature = "gui")]
pub(crate) mod view;

use std::collections::HashMap;
use std::ops::Range;

use sqler_sql::Dialect;

use super::indicator::AiStatus;
use crate::editor::{EditorBuffer, TextEdit};
use crate::query::{BufferSnapshot, RunMode, plan};
use crate::tabs::TabId;

#[cfg(test)]
pub(crate) use job::tests as job_tests;
pub use job::{AssistJob, run_assist};
pub use source::EditorSource;

/// AI is off for the connection of the tab.
pub const AI_OFF: &str = "AI is off for this connection. Turn it on in the AI view.";
/// No provider and model is set up.
pub const NO_MODEL: &str = "No AI provider is set up. Add one in Settings > AI.";
/// `ai.toml` could not be read.
pub const AI_CONFIG_BROKEN: &str = "ai.toml could not be read; fix it to use AI.";
/// The provider kind is not compiled in.
pub const NOT_BUILT: &str = "This AI provider is not available in this build.";
/// A request already runs on the tab.
pub const BUSY: &str = "An AI request is already running in this tab.";
/// The active tab is not a SQL editor.
pub const NO_EDITOR: &str = "Open a SQL editor first.";
/// The tab has no failed statement.
pub const NO_FAILURE: &str = "No failed statement to fix in this tab.";
/// The cursor is not in a statement.
pub const NO_STATEMENT: &str = "No statement at the cursor.";
/// The failed statement is no longer in the editor, or is there twice.
pub const FIX_STALE: &str =
    "The statement changed in the editor since it failed; the fix was not applied.";
/// The generated SQL holds a redacted value.
pub const REDACTED_SQL: &str = "The answer contains a redacted value and was not inserted. Copy it and fill the value in yourself.";
/// The fix equals the statement.
pub const NOT_APPLICABLE_SAME: &str = "The proposed fix is the same as the statement.";
/// The fix holds a redacted value.
pub const NOT_APPLICABLE_REDACTED: &str =
    "The fix contains a redacted value and cannot be applied.";
/// A request was cancelled.
pub const CANCELLED: &str = "Cancelled.";

/// What an assist request does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistKind {
    /// Natural language to SQL.
    Generate,
    /// Explain a statement.
    Explain,
    /// Fix a failed statement.
    Fix,
}

impl AssistKind {
    /// The use case that picks the model.
    #[must_use]
    pub fn use_case(self) -> sqler_config::UseCase {
        match self {
            Self::Generate => sqler_config::UseCase::NlToSql,
            Self::Explain => sqler_config::UseCase::Explain,
            Self::Fix => sqler_config::UseCase::Fix,
        }
    }
}

/// One request. `Debug` never prints the question, the SQL or the error text.
#[derive(Clone, PartialEq, Eq)]
pub enum AssistRequest {
    /// Write SQL for a question.
    Generate {
        /// The question.
        question: String,
    },
    /// Explain a statement.
    Explain {
        /// The statement.
        sql: String,
    },
    /// Fix a failed statement.
    Fix {
        /// The statement.
        sql: String,
        /// The driver's error text.
        error: String,
        /// The position in `QueryTab::results()`.
        statement: usize,
    },
}

impl AssistRequest {
    /// The kind of the request.
    #[must_use]
    pub fn kind(&self) -> AssistKind {
        match self {
            Self::Generate { .. } => AssistKind::Generate,
            Self::Explain { .. } => AssistKind::Explain,
            Self::Fix { .. } => AssistKind::Fix,
        }
    }
}

impl std::fmt::Debug for AssistRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generate { question } => f
                .debug_struct("Generate")
                .field("len", &question.len())
                .finish(),
            Self::Explain { sql } => f.debug_struct("Explain").field("len", &sql.len()).finish(),
            Self::Fix { statement, .. } => f
                .debug_struct("Fix")
                .field("statement", statement)
                .finish_non_exhaustive(),
        }
    }
}

/// What a finished request produced. `Debug` never prints the text.
#[derive(Clone, PartialEq, Eq)]
pub enum AssistOutcome {
    /// Generated SQL.
    Sql {
        /// The statement.
        sql: String,
        /// The model's reasoning.
        rationale: String,
        /// The tables the answer was grounded in.
        tables: Vec<String>,
        /// Whether it was put into the editor.
        inserted: bool,
    },
    /// An explanation.
    Explanation {
        /// The explained statement.
        sql: String,
        /// The explanation.
        text: String,
    },
    /// A proposed fix.
    Fix {
        /// The position in `QueryTab::results()`.
        statement: usize,
        /// The statement as it failed.
        original: String,
        /// The fixed statement.
        sql: String,
        /// A unified diff.
        diff: String,
        /// The model's reasoning.
        rationale: String,
        /// Whether the fix may be applied.
        applicable: bool,
        /// Whether the fix equals the statement.
        unchanged: bool,
    },
}

impl std::fmt::Debug for AssistOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sql { inserted, .. } => f
                .debug_struct("Sql")
                .field("inserted", inserted)
                .finish_non_exhaustive(),
            Self::Explanation { .. } => f.write_str("Explanation { .. }"),
            Self::Fix {
                statement,
                applicable,
                ..
            } => f
                .debug_struct("Fix")
                .field("statement", statement)
                .field("applicable", applicable)
                .finish_non_exhaustive(),
        }
    }
}

/// Where the assistant of one tab stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AssistState {
    /// Nothing asked yet.
    #[default]
    Idle,
    /// A request runs.
    Running {
        /// The request id.
        id: u64,
        /// What it does.
        kind: AssistKind,
    },
    /// The last request answered.
    Done(AssistOutcome),
    /// The fix was applied to the editor.
    FixApplied,
    /// The last request failed.
    Failed {
        /// What it did.
        kind: AssistKind,
        /// Why (never a key).
        message: String,
    },
    /// The user cancelled the last request.
    Cancelled(AssistKind),
}

/// The open prompt bar. `Debug` prints the length only.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct PromptBar {
    /// The question typed so far.
    pub text: String,
}

impl std::fmt::Debug for PromptBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptBar")
            .field("len", &self.text.len())
            .finish()
    }
}

/// The assistant of one tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabAssist {
    /// The prompt bar; open while `Some`.
    pub prompt: Option<PromptBar>,
    /// The state of the last request.
    pub state: AssistState,
}

/// The assistant of every query tab.
#[derive(Debug, Default)]
pub struct EditorAssist {
    tabs: HashMap<TabId, TabAssist>,
    next_id: u64,
}

impl EditorAssist {
    /// The assistant of `tab`, if it ever had one.
    #[must_use]
    pub fn get(&self, tab: TabId) -> Option<&TabAssist> {
        self.tabs.get(&tab)
    }

    /// The assistant of `tab`, created on first use.
    pub(crate) fn entry(&mut self, tab: TabId) -> &mut TabAssist {
        self.tabs.entry(tab).or_default()
    }

    /// A fresh request id.
    pub(crate) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The tab whose request `id` is running.
    #[must_use]
    pub fn tab_of(&self, id: u64) -> Option<TabId> {
        self.tabs.iter().find_map(|(tab, a)| {
            matches!(a.state, AssistState::Running { id: running, .. } if running == id)
                .then_some(*tab)
        })
    }
}

/// The edit that inserts generated SQL at the cursor, on its own lines. A selection is not
/// replaced.
#[must_use]
pub fn insert_edit(buf: &EditorBuffer, sql: &str) -> TextEdit {
    let sql = sql.trim();
    let mut at = buf.cursor.min(buf.text.len());
    while !buf.text.is_char_boundary(at) {
        at -= 1;
    }
    let line_start = at == 0 || buf.text[..at].ends_with('\n');
    let line_end = buf.text[at..].is_empty() || buf.text[at..].starts_with('\n');
    let prefix = if line_start { "" } else { "\n" };
    let suffix = if line_end { "" } else { "\n" };
    let start = at + prefix.len();
    TextEdit {
        range: at..at,
        text: format!("{prefix}{sql}{suffix}"),
        select_after: start..start + sql.len(),
    }
}

/// Whether the match at `at..at + len` is a whole statement: it starts at the
/// text start or after whitespace/`;`, and (unless it ends with `;` itself) only
/// whitespace and then the end of the text or a `;` follow it.
fn on_statement_boundary(text: &str, at: usize, len: usize) -> bool {
    let starts = text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace() || c == ';');
    let end = at + len;
    let ends = text[..end].ends_with(';') || {
        let rest = text[end..].trim_start();
        rest.is_empty() || rest.starts_with(';')
    };
    starts && ends
}

/// The edit that replaces the failed statement `original` with `fixed`.
///
/// # Errors
/// [`FIX_STALE`] unless the trimmed `original` is in `text` exactly once as a whole statement.
pub fn fix_edit(text: &str, original: &str, fixed: &str) -> Result<TextEdit, &'static str> {
    let needle = original.trim();
    if needle.is_empty() {
        return Err(FIX_STALE);
    }
    let mut found = text
        .match_indices(needle)
        .filter(|&(at, m)| on_statement_boundary(text, at, m.len()));
    let (start, _) = found.next().ok_or(FIX_STALE)?;
    if found.next().is_some() {
        return Err(FIX_STALE);
    }
    let fixed = fixed.trim();
    Ok(TextEdit {
        range: start..start + needle.len(),
        text: fixed.to_owned(),
        select_after: start..start + fixed.len(),
    })
}

/// The statement to explain: the trimmed selection, else the statement at the cursor.
#[must_use]
pub fn explain_target(dialect: Option<Dialect>, buf: &BufferSnapshot) -> Option<String> {
    let selected: Option<&str> = (!buf.selection.is_empty())
        .then(|| buf.text.get(Range::clone(&buf.selection)))
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(sql) = selected {
        return Some(sql.to_owned());
    }
    plan(dialect, buf, RunMode::Statement)
        .ok()?
        .statements
        .into_iter()
        .next()
        .map(|s| s.sql)
        .filter(|s| !s.trim().is_empty())
}

/// How a diff line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTone {
    /// An unchanged line.
    Context,
    /// A line only the fix has.
    Added,
    /// A line only the original has.
    Removed,
    /// `@@`, `---` or `+++`.
    Header,
}

/// The lines of a unified diff with their tone.
#[must_use]
pub fn diff_lines(diff: &str) -> Vec<(DiffTone, &str)> {
    // The file headers (`---`/`+++`) come only before the first `@@` hunk header;
    // after it every `-`/`+` line is content, even one such as `--- note`.
    let mut in_hunk = false;
    diff.lines()
        .map(|line| {
            let tone = if line.starts_with("@@") {
                in_hunk = true;
                DiffTone::Header
            } else if !in_hunk && (line.starts_with("---") || line.starts_with("+++")) {
                DiffTone::Header
            } else if line.starts_with('+') {
                DiffTone::Added
            } else if line.starts_with('-') {
                DiffTone::Removed
            } else {
                DiffTone::Context
            };
            (tone, line)
        })
        .collect()
}

/// The running line: `Generating with ollama · local…`.
#[must_use]
pub fn status_line(kind: AssistKind, status: Option<&AiStatus>) -> String {
    let (verb, noun) = match kind {
        AssistKind::Generate => ("Generating", "Generating…"),
        AssistKind::Explain => ("Explaining", "Explaining…"),
        AssistKind::Fix => ("Fixing", "Fixing…"),
    };
    match status {
        Some(s @ AiStatus::On { .. }) => format!("{verb} with {}…", s.label()),
        _ => noun.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::ProviderKind;

    fn buf(text: &str, cursor: usize) -> EditorBuffer {
        EditorBuffer::new(text, cursor)
    }

    fn apply(text: &str, e: &TextEdit) -> String {
        let mut s = text.to_owned();
        s.replace_range(e.range.clone(), &e.text);
        s
    }

    #[test]
    fn insert_mid_line_goes_on_its_own_lines() {
        let b = buf("select 1 from t", 8);
        let e = insert_edit(&b, "select 2");
        let out = apply(&b.text, &e);
        assert_eq!(out, "select 1\nselect 2\n from t");
        assert_eq!(&out[e.select_after.clone()], "select 2");
    }

    #[test]
    fn insert_at_a_line_start_adds_no_prefix() {
        let b = buf("-- note\nselect 1", 8);
        let e = insert_edit(&b, "select 2;\n");
        let out = apply(&b.text, &e);
        assert_eq!(out, "-- note\nselect 2;\nselect 1");
        assert_eq!(&out[e.select_after.clone()], "select 2;");
    }

    #[test]
    fn insert_in_an_empty_buffer_and_at_the_end() {
        let e = insert_edit(&buf("", 0), "select 1");
        assert_eq!(e.text, "select 1");
        assert_eq!(e.select_after, 0..8);
        let b = buf("select 1", 8);
        let e = insert_edit(&b, "select 2");
        assert_eq!(apply(&b.text, &e), "select 1\nselect 2");
        // Before a newline: no suffix.
        let b = buf("a\nb", 1);
        assert_eq!(apply(&b.text, &insert_edit(&b, "x")), "a\nx\nb");
    }

    #[test]
    fn insert_keeps_the_selection_text() {
        let mut b = buf("select 1 from t", 8);
        b.selection = 0..8;
        let e = insert_edit(&b, "select 2");
        assert_eq!(e.range, 8..8);
        assert!(apply(&b.text, &e).starts_with("select 1\n"));
    }

    #[test]
    fn fix_replaces_exactly_one_match() {
        let text = "-- x\nselect nme from t;\n";
        let e = fix_edit(text, "select nme from t;", "select name from t;").unwrap();
        let out = apply(text, &e);
        assert_eq!(out, "-- x\nselect name from t;\n");
        assert_eq!(&out[e.select_after], "select name from t;");
    }

    #[test]
    fn fix_refuses_none_two_and_changed_text() {
        assert_eq!(fix_edit("select 1", "select 2", "x"), Err(FIX_STALE));
        assert_eq!(
            fix_edit("select 1;\nselect 1;", "select 1", "x"),
            Err(FIX_STALE)
        );
        assert_eq!(fix_edit("select 1", "  ", "x"), Err(FIX_STALE));
        assert_eq!(fix_edit("", "select 1", "x"), Err(FIX_STALE));
    }

    #[test]
    fn fix_requires_statement_boundaries() {
        // A prefix of a longer statement is not a match.
        assert_eq!(
            fix_edit("delete from t where id = 1", "delete from t", "x"),
            Err(FIX_STALE)
        );
        assert_eq!(
            fix_edit("delete from t where id = 1;", "delete from t;", "x"),
            Err(FIX_STALE)
        );
        // Suffix of a longer token run.
        assert_eq!(fix_edit("xselect 1", "select 1", "y"), Err(FIX_STALE));
        // Two identical statements.
        assert_eq!(
            fix_edit("select 1; select 1;", "select 1;", "x"),
            Err(FIX_STALE)
        );
        // Exactly once between other statements, with and without semicolons.
        let text = "select 0;\nselect nme from t;\nselect 2;";
        let e = fix_edit(text, "select nme from t", "select name from t").unwrap();
        assert_eq!(apply(text, &e), "select 0;\nselect name from t;\nselect 2;");
        let e = fix_edit(text, "select nme from t;", "select name from t;").unwrap();
        assert_eq!(apply(text, &e), "select 0;\nselect name from t;\nselect 2;");
        // A short statement that is also a prefix elsewhere counts once.
        let text = "delete from t where id = 1;\ndelete from t;";
        let e = fix_edit(text, "delete from t", "delete from t limit 1").unwrap();
        assert_eq!(
            apply(text, &e),
            "delete from t where id = 1;\ndelete from t limit 1;"
        );
        // The editor text changed since the request.
        assert_eq!(
            fix_edit("select nme from t where a = 1;", "select nme from t", "x"),
            Err(FIX_STALE)
        );
    }

    #[test]
    fn explain_target_prefers_the_selection() {
        let text = "select 1;\nselect 2;";
        let sel = BufferSnapshot::with_selection(text, 10..18);
        assert_eq!(
            explain_target(Some(Dialect::Sqlite), &sel).as_deref(),
            Some("select 2")
        );
        let cur = BufferSnapshot::at_cursor(text, 3);
        assert_eq!(
            explain_target(Some(Dialect::Sqlite), &cur).as_deref(),
            Some("select 1")
        );
        let blank = BufferSnapshot::with_selection("   \n", 0..3);
        assert_eq!(explain_target(Some(Dialect::Sqlite), &blank), None);
        assert_eq!(
            explain_target(None, &BufferSnapshot::at_cursor("select 3", 0)).as_deref(),
            Some("select 3")
        );
    }

    #[test]
    fn diff_tones() {
        let lines = diff_lines("--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n ctx");
        let tones: Vec<DiffTone> = lines.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            tones,
            [
                DiffTone::Header,
                DiffTone::Header,
                DiffTone::Header,
                DiffTone::Removed,
                DiffTone::Added,
                DiffTone::Context
            ]
        );
        assert_eq!(lines[3].1, "-old");
    }

    #[test]
    fn removed_comment_line_is_not_a_header() {
        let lines = diff_lines("--- a\n+++ b\n@@ -1,2 +1,1 @@\n--- note\n+++ x\n-old");
        let tones: Vec<DiffTone> = lines.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            tones,
            [
                DiffTone::Header,
                DiffTone::Header,
                DiffTone::Header,
                DiffTone::Removed,
                DiffTone::Added,
                DiffTone::Removed
            ]
        );
    }

    #[test]
    fn status_line_names_the_provider() {
        let on = AiStatus::On {
            provider: "ollama".into(),
            kind: ProviderKind::Ollama,
            model: "m".into(),
            local: true,
            also_remote: false,
            rows: false,
        };
        assert_eq!(
            status_line(AssistKind::Generate, Some(&on)),
            "Generating with ollama \u{b7} local…"
        );
        assert_eq!(
            status_line(AssistKind::Explain, Some(&on)),
            "Explaining with ollama \u{b7} local…"
        );
        assert_eq!(
            status_line(AssistKind::Fix, Some(&on)),
            "Fixing with ollama \u{b7} local…"
        );
        assert_eq!(status_line(AssistKind::Fix, None), "Fixing…");
        assert_eq!(
            status_line(AssistKind::Generate, Some(&AiStatus::Off)),
            "Generating…"
        );
    }

    #[test]
    fn debug_hides_texts() {
        let r = AssistRequest::Generate {
            question: "CANARY-Q".into(),
        };
        let o = AssistOutcome::Explanation {
            sql: "CANARY-S".into(),
            text: "CANARY-T".into(),
        };
        let p = PromptBar {
            text: "CANARY-P".into(),
        };
        let text = format!("{r:?}{o:?}{p:?}");
        assert!(!text.contains("CANARY"), "{text}");
    }
}
