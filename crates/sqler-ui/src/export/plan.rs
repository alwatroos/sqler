// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What an export is made of: where the rows come from, the dialog and its validation, and the
//! rule that decides whether a statement may be run again.
//!
//! Everything here is plain data and pure functions. The rule (ADR-0032): rows come from the rows
//! the grid already holds ([`ExportScope::Loaded`], [`ExportScope::Selection`]) or from a fresh run
//! of the statement ([`ExportScope::Fresh`]). A fresh run is allowed only for one statement that
//! `sqler-sql` classifies `Read`; anything else is never run again.

use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use sqler_driver_api::sqler_core::Query;
use sqler_export::{ExportOptions, Registry};
use sqler_sql::{Dialect, StatementClass, classify_each};

use super::job::{ExportStop, WriteSpec};
use crate::connections::ConnectRequest;
use crate::tabs::TabId;

/// The highest `rows per INSERT` the dialog accepts.
pub const MAX_ROWS_PER_INSERT: usize = 10_000;

/// What is exported: a grid tab, or one result of a query tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExportTarget {
    /// The tab.
    pub tab: TabId,
    /// `None` for a grid tab; `Some(i)` for result `i` of a query tab.
    pub result: Option<usize>,
}

/// The statement behind a result, kept to run it again.
#[derive(Clone)]
pub struct ExportOrigin {
    /// The statement text, as shown to the user.
    pub sql: String,
    /// How `sqler-sql` classified it when it was run.
    pub class: StatementClass,
    /// What is executed (placeholders rewritten, values bound).
    pub query: Query,
}

// Never the SQL text or the bound values.
impl std::fmt::Debug for ExportOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportOrigin")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// Rows and columns of the grid model, by model index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedRows {
    /// Every row and column, in model order.
    All,
    /// These rows and columns, in display order.
    Picked {
        /// Row indices.
        rows: Vec<usize>,
        /// Column indices.
        columns: Vec<usize>,
    },
}

/// What the view knows about the result on screen when the user asks for an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridFacts {
    /// Number of columns.
    pub columns: usize,
    /// Number of rows the model holds.
    pub rows: usize,
    /// Whether the result ended (every row is loaded).
    pub complete: bool,
    /// The selected cells, if any.
    pub selection: Option<LoadedRows>,
}

/// Which rows the file gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportScope {
    /// Every row the grid holds.
    Loaded,
    /// The selected cells only.
    Selection,
    /// Every row of the result, read again from the database.
    Fresh,
}

/// The `CSV` field separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// Tab.
    Tab,
    /// `|`
    Pipe,
}

impl Delimiter {
    /// Every delimiter the dialog offers.
    pub const ALL: [Delimiter; 4] = [Self::Comma, Self::Semicolon, Self::Tab, Self::Pipe];

    /// The byte written between fields.
    #[must_use]
    pub fn byte(self) -> u8 {
        match self {
            Self::Comma => b',',
            Self::Semicolon => b';',
            Self::Tab => b'\t',
            Self::Pipe => b'|',
        }
    }

    /// A name for a button.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Comma => "Comma",
            Self::Semicolon => "Semicolon",
            Self::Tab => "Tab",
            Self::Pipe => "Pipe",
        }
    }
}

/// Whether the result can be read again from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshRun {
    /// Every row is loaded already.
    NotNeeded,
    /// The statement is a single read.
    Allowed,
    /// Not allowed; the text tells the user why.
    Refused(String),
}

/// What the dialog edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportForm {
    /// The id of the format.
    pub format: String,
    /// Which rows.
    pub scope: ExportScope,
    /// `CSV` separator.
    pub delimiter: Delimiter,
    /// `CSV` header row.
    pub header: bool,
    /// The text of a `NULL` (`CSV` and Markdown).
    pub null_text: String,
    /// `SQL`: the table.
    pub table: String,
    /// `SQL`: the schema (empty for none).
    pub schema: String,
    /// `SQL`: rows per `INSERT`, as typed.
    pub rows_per_insert: String,
}

/// The `NULL` text a format starts with.
fn default_null_text(format: &str) -> &'static str {
    if format == sqler_export::MARKDOWN {
        "NULL"
    } else {
        ""
    }
}

impl ExportForm {
    /// The form of a new dialog.
    #[must_use]
    pub fn new(scope: ExportScope) -> Self {
        Self {
            format: sqler_export::CSV.to_owned(),
            scope,
            delimiter: Delimiter::Comma,
            header: true,
            null_text: default_null_text(sqler_export::CSV).to_owned(),
            table: String::new(),
            schema: String::new(),
            rows_per_insert: "100".to_owned(),
        }
    }
}

/// A format as the dialog lists it: id, label, file extension.
pub type FormatEntry = (&'static str, &'static str, &'static str);

/// The export dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDialog {
    /// What is exported.
    pub target: ExportTarget,
    /// The compiled-in formats.
    pub formats: Vec<FormatEntry>,
    /// The values being edited.
    pub form: ExportForm,
    /// Rows the grid holds.
    pub loaded_rows: usize,
    /// Whether the result ended.
    pub complete: bool,
    /// The selected cells, if any.
    pub selection: Option<LoadedRows>,
    /// Whether a fresh run is possible.
    pub fresh: FreshRun,
    /// Why the last submit failed.
    pub error: Option<String>,
    /// Tells this dialog from an earlier one, so a view can reset its inputs.
    pub generation: u64,
    /// The start of the suggested file name.
    pub stem: String,
}

/// The formats compiled into this build, in registry order.
#[must_use]
pub fn available_formats() -> Vec<FormatEntry> {
    Registry::with_builtin()
        .formats()
        .map(|f| (f.id(), f.label(), f.extension()))
        .collect()
}

impl ExportDialog {
    /// A dialog for `target` with the defaults: `CSV`, and a fresh run when it is allowed.
    #[must_use]
    pub fn new(
        target: ExportTarget,
        facts: &GridFacts,
        fresh: FreshRun,
        stem: String,
        generation: u64,
    ) -> Self {
        let scope = if fresh == FreshRun::Allowed {
            ExportScope::Fresh
        } else {
            ExportScope::Loaded
        };
        Self {
            target,
            formats: available_formats(),
            form: ExportForm::new(scope),
            loaded_rows: facts.rows,
            complete: facts.complete,
            selection: facts.selection.clone(),
            fresh,
            error: None,
            generation,
            stem,
        }
    }

    /// Picks a format; an id that is not compiled in is ignored. A `NULL` text still at the
    /// default of the old format moves to the default of the new one.
    pub fn set_format(&mut self, id: &str) {
        if !self.formats.iter().any(|(f, _, _)| *f == id) {
            return;
        }
        if self.form.null_text == default_null_text(&self.form.format) {
            default_null_text(id).clone_into(&mut self.form.null_text);
        }
        id.clone_into(&mut self.form.format);
        self.error = None;
    }

    /// Picks which rows to export.
    pub fn set_scope(&mut self, scope: ExportScope) {
        self.form.scope = scope;
        self.error = None;
    }

    /// Picks the `CSV` separator.
    pub fn set_delimiter(&mut self, delimiter: Delimiter) {
        self.form.delimiter = delimiter;
    }

    /// Turns the `CSV` header row on or off.
    pub fn set_header(&mut self, header: bool) {
        self.form.header = header;
    }

    /// The file extension of the chosen format.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        self.formats
            .iter()
            .find(|(id, _, _)| *id == self.form.format)
            .map_or("txt", |(_, _, ext)| ext)
    }
}

/// Whether the statement behind a result may be run again to export it.
///
/// Only one statement that `sqler-sql` classifies `Read` passes, and only when it was already
/// classified `Read` when it ran. Without a dialect nothing is known about the syntax: never a
/// read.
///
/// # Errors
///
/// The text to show the user.
pub fn fresh_gate(dialect: Option<Dialect>, origin: Option<&ExportOrigin>) -> Result<(), String> {
    let refuse = |class: StatementClass| {
        Err(format!(
            "This statement is not read-only (class: {}); only the loaded rows can be exported.",
            class.as_str()
        ))
    };
    let Some(origin) = origin else {
        return Err(
            "This result has no statement to run again; only the loaded rows can be exported."
                .to_owned(),
        );
    };
    let Some(dialect) = dialect else {
        return refuse(StatementClass::Unknown);
    };
    let segments = classify_each(dialect, &origin.sql);
    if segments.len() != 1 {
        return Err(
            "This text is not a single statement; only the loaded rows can be exported.".to_owned(),
        );
    }
    let found = segments[0].1;
    if found != StatementClass::Read {
        return refuse(found);
    }
    if origin.class != StatementClass::Read {
        return refuse(origin.class);
    }
    Ok(())
}

/// Where the rows of a job come from.
// One job lives at a time, so the size difference costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum ExportSource {
    /// Rows the grid holds, copied on the UI thread.
    Loaded(LoadedRows),
    /// A fresh run of the statement on a short-lived session.
    Fresh {
        /// The connection to open.
        request: ConnectRequest,
        /// The statement.
        query: Query,
    },
}

/// A job for the view to start.
pub struct ExportJob {
    /// The tab the export belongs to.
    pub tab: TabId,
    /// Tells this job from an earlier one.
    pub id: u64,
    /// What is exported.
    pub target: ExportTarget,
    /// Where the rows come from.
    pub source: ExportSource,
    /// How and where they are written.
    pub spec: WriteSpec,
    /// Stops the job.
    pub stop: ExportStop,
}

// Never the statement, the connection or the path.
impl std::fmt::Debug for ExportJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportJob")
            .field("tab", &self.tab)
            .field("id", &self.id)
            .field("fresh", &matches!(self.source, ExportSource::Fresh { .. }))
            .finish_non_exhaustive()
    }
}

/// How far a running export has come, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportStatus {
    /// The job.
    pub id: u64,
    /// The name of the target file (no folder).
    pub file_name: String,
    /// Rows written so far.
    pub rows: u64,
    /// Bytes written so far.
    pub bytes: u64,
    /// Whether a cancel is on its way.
    pub cancelling: bool,
}

impl ExportStatus {
    /// `Exporting to out.csv… 1,234 rows`, or `Cancelling export…`.
    #[must_use]
    pub fn status_text(&self) -> String {
        if self.cancelling {
            return "Cancelling export…".to_owned();
        }
        format!("Exporting to {}… {}", self.file_name, rows_text(self.rows))
    }
}

/// `1 row`, `1,234 rows`.
#[must_use]
pub fn rows_text(rows: u64) -> String {
    let n = usize::try_from(rows).unwrap_or(usize::MAX);
    let digits = crate::grid::group_thousands(n);
    if rows == 1 {
        format!("{digits} row")
    } else {
        format!("{digits} rows")
    }
}

/// Checks the form and turns it into the id of the format and its options. Does not look at the
/// scope.
///
/// # Errors
///
/// The text to show in the dialog.
pub fn validate(
    form: &ExportForm,
    dialect: Option<Dialect>,
) -> Result<(String, ExportOptions), String> {
    let registry = Registry::with_builtin();
    let Some(format) = registry.get(&form.format) else {
        return Err("This format is not available in this build.".to_owned());
    };
    let mut options = ExportOptions::default();
    options.csv.delimiter = form.delimiter.byte();
    options.csv.header = form.header;
    options.csv.null_text.clone_from(&form.null_text);
    options.markdown.null_text.clone_from(&form.null_text);
    if format.id() == sqler_export::SQL {
        let table = form.table.trim();
        if table.is_empty() {
            return Err("Enter the table name for the INSERT statements.".to_owned());
        }
        let Some(dialect) = dialect else {
            return Err(
                "The SQL format does not support the driver of this connection.".to_owned(),
            );
        };
        let per = form.rows_per_insert.trim().parse::<usize>().ok();
        let Some(per) = per.filter(|n| (1..=MAX_ROWS_PER_INSERT).contains(n)) else {
            return Err(format!(
                "Rows per INSERT must be a number from 1 to {MAX_ROWS_PER_INSERT}."
            ));
        };
        options.sql.dialect = dialect;
        table.clone_into(&mut options.sql.table);
        let schema = form.schema.trim();
        options.sql.schema = (!schema.is_empty()).then(|| schema.to_owned());
        options.sql.rows_per_statement = NonZeroUsize::new(per).unwrap_or(NonZeroUsize::MIN);
    }
    // The format knows best what it accepts: ask it, so the dialog shows the reason.
    if let Err(err) = format.create(&options, Box::new(io::sink())) {
        return Err(format!("These options cannot be used: {err}"));
    }
    Ok((form.format.clone(), options))
}

/// Replaces every char outside `[A-Za-z0-9._-]` with `_` and cuts the name to 64 chars.
#[must_use]
pub fn file_stem(raw: &str) -> String {
    let mut stem: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    // A leading dot would hide the file.
    if stem.starts_with('.') {
        stem.replace_range(..1, "_");
    }
    if stem.is_empty() {
        "export".clone_into(&mut stem);
    }
    stem
}

/// The name of the file `path` points to, for the status bar and the notices.
#[must_use]
pub fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(|| "file".to_owned(), |n| n.to_string_lossy().into_owned())
}

/// A folder to start the save dialog in, as a path.
#[must_use]
pub fn start_directory(last: Option<&PathBuf>) -> PathBuf {
    last.cloned()
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(sql: &str, class: StatementClass) -> ExportOrigin {
        ExportOrigin {
            sql: sql.to_owned(),
            class,
            query: Query::new(sql),
        }
    }

    fn gate(dialect: Option<Dialect>, sql: &str, class: StatementClass) -> Result<(), String> {
        fresh_gate(dialect, Some(&origin(sql, class)))
    }

    #[test]
    fn gate_lets_one_read_through() {
        let d = Some(Dialect::Sqlite);
        assert!(gate(d, "select 1", StatementClass::Read).is_ok());
        assert!(
            gate(
                d,
                "with a as (select 1) select * from a",
                StatementClass::Read
            )
            .is_ok()
        );
    }

    #[test]
    fn gate_refuses_everything_else() {
        let d = Some(Dialect::Sqlite);
        let msg = gate(d, "delete from t", StatementClass::Write).unwrap_err();
        assert_eq!(
            msg,
            "This statement is not read-only (class: write); only the loaded rows can be exported."
        );
        let msg = gate(d, "create table t (a int)", StatementClass::Ddl).unwrap_err();
        assert!(msg.contains("class: ddl"), "{msg}");
        let msg = gate(d, "begin", StatementClass::Other).unwrap_err();
        assert!(msg.contains("class: other"), "{msg}");
        let msg = gate(d, "frobnicate", StatementClass::Unknown).unwrap_err();
        assert!(msg.contains("class: unknown"), "{msg}");
        // A statement that was not a read when it ran stays refused, whatever the text says now.
        let msg = gate(d, "select 1", StatementClass::Write).unwrap_err();
        assert!(msg.contains("class: write"), "{msg}");
    }

    #[test]
    fn gate_needs_a_dialect_an_origin_and_one_statement() {
        let msg = gate(None, "select 1", StatementClass::Read).unwrap_err();
        assert!(msg.contains("class: unknown"), "{msg}");
        let msg = fresh_gate(Some(Dialect::Sqlite), None).unwrap_err();
        assert!(msg.contains("no statement to run again"), "{msg}");
        let msg = gate(
            Some(Dialect::Sqlite),
            "select 1; delete from t",
            StatementClass::Read,
        )
        .unwrap_err();
        assert!(msg.contains("not a single statement"), "{msg}");
        let msg = gate(Some(Dialect::Sqlite), "  ", StatementClass::Read).unwrap_err();
        assert!(msg.contains("not a single statement"), "{msg}");
    }

    #[test]
    fn origin_debug_shows_the_class_only() {
        let o = origin("select secret_column from t", StatementClass::Read);
        let text = format!("{o:?}");
        assert!(text.contains("Read"), "{text}");
        assert!(!text.contains("secret_column"), "{text}");
    }

    fn facts(complete: bool) -> GridFacts {
        GridFacts {
            columns: 2,
            rows: 10,
            complete,
            selection: None,
        }
    }

    fn dialog(fresh: FreshRun, complete: bool) -> ExportDialog {
        let target = ExportTarget {
            tab: crate::tabs::TabStrip::default().open(
                crate::tabs::TabConnection {
                    name: "dev".to_owned(),
                    driver: sqler_driver_api::sqler_core::DriverId::new("sqlite").unwrap(),
                    color: None,
                    environment: None,
                },
                crate::tabs::TabKind::Query,
            ),
            result: None,
        };
        ExportDialog::new(target, &facts(complete), fresh, "r".to_owned(), 1)
    }

    #[test]
    fn dialog_defaults() {
        let d = dialog(FreshRun::Allowed, false);
        assert_eq!(d.form.scope, ExportScope::Fresh);
        assert_eq!(d.form.format, "csv");
        assert!(d.form.header);
        assert_eq!(d.form.delimiter, Delimiter::Comma);
        assert_eq!(d.form.rows_per_insert, "100");
        let ids: Vec<_> = d.formats.iter().map(|f| f.0).collect();
        assert_eq!(&ids[..4], ["csv", "json", "sql", "markdown"]);
        let d = dialog(FreshRun::Refused("no".into()), false);
        assert_eq!(d.form.scope, ExportScope::Loaded);
        let d = dialog(FreshRun::NotNeeded, true);
        assert_eq!(d.form.scope, ExportScope::Loaded);
        assert_eq!(d.extension(), "csv");
    }

    #[test]
    fn setters() {
        let mut d = dialog(FreshRun::NotNeeded, true);
        d.set_format("nope");
        assert_eq!(d.form.format, "csv");
        d.set_format("markdown");
        assert_eq!(d.form.format, "markdown");
        assert_eq!(d.form.null_text, "NULL");
        assert_eq!(d.extension(), "md");
        d.set_format("csv");
        assert_eq!(d.form.null_text, "");
        d.form.null_text = "-".to_owned();
        d.set_format("markdown");
        assert_eq!(d.form.null_text, "-");
        d.set_delimiter(Delimiter::Tab);
        d.set_header(false);
        d.set_scope(ExportScope::Selection);
        assert_eq!(d.form.delimiter, Delimiter::Tab);
        assert!(!d.form.header);
        assert_eq!(d.form.scope, ExportScope::Selection);
    }

    #[test]
    fn validation_maps_the_form_to_options() {
        let mut form = ExportForm::new(ExportScope::Loaded);
        form.delimiter = Delimiter::Semicolon;
        form.header = false;
        form.null_text = "NA".to_owned();
        let (id, options) = validate(&form, None).unwrap();
        assert_eq!(id, "csv");
        assert_eq!(options.csv.delimiter, b';');
        assert!(!options.csv.header);
        assert_eq!(options.csv.null_text, "NA");
        assert_eq!(options.markdown.null_text, "NA");
    }

    #[test]
    fn validation_errors() {
        let mut form = ExportForm::new(ExportScope::Loaded);
        form.format = "nope".to_owned();
        assert!(validate(&form, None).unwrap_err().contains("not available"));

        let mut form = ExportForm::new(ExportScope::Loaded);
        form.format = "sql".to_owned();
        assert!(
            validate(&form, Some(Dialect::Sqlite))
                .unwrap_err()
                .contains("table name")
        );
        form.table = "t".to_owned();
        assert!(
            validate(&form, None)
                .unwrap_err()
                .contains("does not support")
        );
        for bad in ["", "0", "10001", "x", "-1"] {
            form.rows_per_insert = bad.to_owned();
            let msg = validate(&form, Some(Dialect::Sqlite)).unwrap_err();
            assert!(msg.contains("1 to 10000"), "{bad}: {msg}");
        }
        form.rows_per_insert = " 10000 ".to_owned();
        form.schema = " main ".to_owned();
        let (_, options) = validate(&form, Some(Dialect::Sqlite)).unwrap();
        assert_eq!(options.sql.rows_per_statement.get(), 10_000);
        assert_eq!(options.sql.schema.as_deref(), Some("main"));
        assert_eq!(options.sql.table, "t");
        assert_eq!(options.sql.dialect, Dialect::Sqlite);

        // The format refuses a NULL text that holds the delimiter.
        let mut form = ExportForm::new(ExportScope::Loaded);
        form.null_text = ",".to_owned();
        assert!(
            validate(&form, None)
                .unwrap_err()
                .contains("cannot be used")
        );
    }

    #[test]
    fn stems() {
        assert_eq!(file_stem("Results: my table/x"), "Results__my_table_x");
        assert_eq!(file_stem(".hidden"), "_hidden");
        assert_eq!(file_stem(""), "export");
        assert_eq!(file_stem(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn status_texts() {
        let mut s = ExportStatus {
            id: 1,
            file_name: "out.csv".to_owned(),
            rows: 1234,
            bytes: 9,
            cancelling: false,
        };
        assert_eq!(s.status_text(), "Exporting to out.csv… 1,234 rows");
        s.rows = 1;
        assert_eq!(s.status_text(), "Exporting to out.csv… 1 row");
        s.cancelling = true;
        assert_eq!(s.status_text(), "Cancelling export…");
    }
}
