// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Copy and copy-as: the selected cells of a results grid as text for the clipboard.
//!
//! [`CopyJob`] turns the selection into an owned slice of the result. It reads display rows and
//! display columns through [`DisplayMap`], so column order, pinning and any row order apply, and
//! renders the slice with the `sqler-export` writers (the same code as file export): TSV is the
//! CSV writer with a tab delimiter, plus Markdown, JSON and SQL `INSERT`. Gathering is sliced
//! ([`CopyJob::gather`]) so the UI thread stays responsive; [`CopyJob::render`] owns its data and
//! can run on another thread.
//!
//! The copied text can hold secrets. Nothing here logs it, and the `Debug` output of the job and
//! of its result only shows counts.

use std::fmt;
use std::io::{self, Write};

use sqler_config::Platform;
use sqler_driver_api::sqler_core::{Column, DbObject, Row, Value};
use sqler_export::{
    CsvFormat, ExportControl, ExportError, ExportOptions, Exporter, Format, JsonFormat, LineEnding,
    MarkdownFormat, SqlFormat,
};
use sqler_sql::Dialect;

use super::cell::group_thousands;
use super::model::{GridModel, IngestState};
use super::selection::{DisplayMap, Selection};
use crate::command::{Command, CommandError, CommandId, CommandRegistry};
use crate::keymap::KeyInput;

/// The table name `INSERT` statements use when the grid does not show a table.
pub const PLACEHOLDER_TABLE: &str = "table_name";
/// Cells gathered per UI turn.
pub const GATHER_SLICE_CELLS: usize = 50_000;
/// At or below this many cells the copy gathers and renders inline.
pub const SYNC_CELLS: usize = 10_000;
/// At or above this many cells a successful copy is announced.
pub const NOTIFY_CELLS: usize = 100_000;

/// What to copy the selection as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    /// Tab-separated values, no header.
    Tsv,
    /// Tab-separated values with the column names on the first line.
    TsvWithHeaders,
    /// A Markdown table.
    Markdown,
    /// A JSON array of objects.
    Json,
    /// SQL `INSERT` statements.
    Insert,
}

impl CopyFormat {
    /// Every format, in menu order.
    pub const ALL: [CopyFormat; 5] = [
        Self::Tsv,
        Self::TsvWithHeaders,
        Self::Markdown,
        Self::Json,
        Self::Insert,
    ];

    fn id_str(self) -> &'static str {
        match self {
            Self::Tsv => "grid:copy",
            Self::TsvWithHeaders => "grid:copy-with-headers",
            Self::Markdown => "grid:copy-as-markdown",
            Self::Json => "grid:copy-as-json",
            Self::Insert => "grid:copy-as-insert",
        }
    }

    /// The ID of the command that runs this copy.
    ///
    /// # Panics
    ///
    /// Never in practice: the IDs are constants checked by a unit test.
    #[must_use]
    #[allow(clippy::expect_used)] // constant IDs, validated by the `ids_are_valid` test
    pub fn command_id(self) -> CommandId {
        CommandId::new(self.id_str()).expect("constant command ID is valid")
    }

    /// The palette title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Tsv => "Results: Copy",
            Self::TsvWithHeaders => "Results: Copy with Headers",
            Self::Markdown => "Results: Copy as Markdown",
            Self::Json => "Results: Copy as JSON",
            Self::Insert => "Results: Copy as SQL INSERT",
        }
    }

    /// The label in the grid context menu.
    #[must_use]
    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Tsv => "Copy",
            Self::TsvWithHeaders => "Copy with Headers",
            Self::Markdown => "Copy as Markdown",
            Self::Json => "Copy as JSON",
            Self::Insert => "Copy as SQL INSERT",
        }
    }

    /// The short name used in messages.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tsv | Self::TsvWithHeaders => "TSV",
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
            Self::Insert => "SQL INSERT",
        }
    }

    /// The format a command ID stands for, if it is a copy command.
    #[must_use]
    pub fn from_command_id(id: &CommandId) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.command_id() == *id)
    }
}

/// Registers the copy commands.
///
/// # Errors
///
/// Returns [`CommandError::Duplicate`] if one of the IDs is already registered.
pub fn register_copy_commands(registry: &mut CommandRegistry) -> Result<(), CommandError> {
    for f in CopyFormat::ALL {
        registry.register(Command::new(f.id_str(), f.title())?)?;
    }
    Ok(())
}

/// The copy format a key stands for while the data table has the focus: Mod+C copies, Mod+Shift+C
/// copies with headers. `mod` is Cmd on macOS and Ctrl elsewhere. The key is claimed only when
/// `table_focused` and no overlay is open, so the SQL editor's own copy is never affected.
#[must_use]
pub fn copy_key_format(
    input: &KeyInput,
    platform: Platform,
    table_focused: bool,
    overlay_open: bool,
    editing: bool,
) -> Option<CopyFormat> {
    if !table_focused || overlay_open || editing || input.alt || input.key != "c" {
        return None;
    }
    let (modifier, other) = match platform {
        Platform::MacOs => (input.cmd, input.ctrl),
        Platform::Other => (input.ctrl, input.cmd),
    };
    if !modifier || other {
        return None;
    }
    Some(if input.shift {
        CopyFormat::TsvWithHeaders
    } else {
        CopyFormat::Tsv
    })
}

/// Where `INSERT` statements point: the dialect and the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertTarget {
    /// The quoting dialect.
    pub dialect: Dialect,
    /// The schema (the database on `MySQL`), if any.
    pub schema: Option<String>,
    /// The table name.
    pub table: String,
    /// The connection has no SQL dialect, so `PostgreSQL` quoting was assumed.
    pub assumed_dialect: bool,
}

impl InsertTarget {
    /// The placeholder table [`PLACEHOLDER_TABLE`]; an unknown dialect falls back to `PostgreSQL`.
    #[must_use]
    pub fn placeholder(dialect: Option<Dialect>) -> Self {
        Self {
            dialect: dialect.unwrap_or(Dialect::Postgres),
            schema: None,
            table: PLACEHOLDER_TABLE.to_owned(),
            assumed_dialect: dialect.is_none(),
        }
    }

    /// The real table or view behind a data tab.
    #[must_use]
    pub fn for_object(dialect: Option<Dialect>, object: &DbObject) -> Self {
        let resolved = dialect.unwrap_or(Dialect::Postgres);
        // The same choice as `sqler_sql::dialect::qualifier`, which is private.
        let path = &object.parent;
        let schema = match resolved {
            Dialect::MySql => path.schema.as_ref().or(path.database.as_ref()),
            _ => path.schema.as_ref(),
        };
        Self {
            dialect: resolved,
            schema: schema.cloned(),
            table: object.name.clone(),
            assumed_dialect: dialect.is_none(),
        }
    }
}

/// The caps of one copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyLimits {
    /// The most cells one copy may hold.
    pub max_cells: usize,
    /// The most bytes of text one copy may produce.
    pub max_bytes: usize,
}

impl CopyLimits {
    /// 1,000,000 cells and 50 MiB.
    pub const DEFAULT: Self = Self {
        max_cells: 1_000_000,
        max_bytes: 50 * 1024 * 1024,
    };
}

/// Why a copy did not happen. The messages are for the user and never hold cell values.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CopyError {
    /// There is no selection, or the grid has no cells.
    #[error("Select cells to copy first.")]
    NothingSelected,
    /// The selection is over the cell cap.
    #[error(
        "The selection has {} cells; copying is limited to 1,000,000 cells. Select fewer rows or columns.",
        group_thousands(*cells)
    )]
    TooManyCells {
        /// The cells selected.
        cells: usize,
    },
    /// The text would be over the byte cap.
    #[error(
        "The copied text would be larger than 50 MiB; nothing was copied. Select fewer rows or columns."
    )]
    TooLarge,
    /// A writer failed.
    #[error("Could not copy: {0}")]
    Render(String),
}

/// The text of a finished copy.
pub struct CopyOutput {
    /// The text for the clipboard.
    pub text: String,
    /// The rows in it.
    pub rows: usize,
    /// The format it was rendered as.
    pub format: CopyFormat,
    /// The result was still arriving, so rows may be missing.
    pub partial: bool,
    /// `INSERT` used `PostgreSQL` quoting because the connection has no dialect.
    pub assumed_dialect: bool,
}

impl fmt::Debug for CopyOutput {
    /// Prints the size only: the text can hold secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopyOutput")
            .field("bytes", &self.text.len())
            .field("rows", &self.rows)
            .field("format", &self.format)
            .field("partial", &self.partial)
            .finish_non_exhaustive()
    }
}

/// The notification for a finished copy, if one is due: large or partial copies and an assumed
/// dialect. `cells` is [`CopyJob::cells`].
#[must_use]
pub fn success_notice(out: &CopyOutput, cells: usize) -> Option<String> {
    if cells < NOTIFY_CELLS && !out.partial && !out.assumed_dialect {
        return None;
    }
    let mut text = format!(
        "Copied {} rows as {}.",
        group_thousands(out.rows),
        out.format.label()
    );
    if out.partial {
        text.push_str(" Only the rows fetched so far were copied.");
    }
    if out.assumed_dialect {
        text.push_str(" This connection has no SQL dialect; the INSERT uses PostgreSQL quoting.");
    }
    Some(text)
}

/// A selection turned into owned data, ready to render. It is `Send`.
pub struct CopyJob {
    format: CopyFormat,
    target: InsertTarget,
    limits: CopyLimits,
    columns: Vec<Column>,
    model_cols: Vec<usize>,
    model_rows: Vec<usize>,
    /// Index into `model_rows` of the next row to gather.
    next: usize,
    rows: Vec<Row>,
    partial: bool,
}

impl fmt::Debug for CopyJob {
    /// Prints counts only: the rows can hold secrets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopyJob")
            .field("format", &self.format)
            .field("columns", &self.columns.len())
            .field("rows", &self.model_rows.len())
            .field("gathered", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl CopyJob {
    /// Plans the copy of the selection. No row is read yet.
    ///
    /// # Errors
    ///
    /// [`CopyError::NothingSelected`] without a selection, [`CopyError::TooManyCells`] over the
    /// cell cap (checked before anything is allocated).
    pub fn new(
        model: &GridModel,
        map: &DisplayMap<'_>,
        sel: &Selection,
        format: CopyFormat,
        target: InsertTarget,
        limits: CopyLimits,
    ) -> Result<Self, CopyError> {
        let rect = sel.rect(map).ok_or(CopyError::NothingSelected)?;
        let cells = rect.rows.len().saturating_mul(rect.cols.len());
        if cells == 0 {
            return Err(CopyError::NothingSelected);
        }
        if cells > limits.max_cells {
            return Err(CopyError::TooManyCells { cells });
        }
        let mut model_cols = Vec::with_capacity(rect.cols.len());
        let mut columns = Vec::with_capacity(rect.cols.len());
        for display in rect.cols.clone() {
            if let Some(col) = map.columns.model_column(display)
                && let Some(column) = model.columns().get(col)
            {
                model_cols.push(col);
                columns.push(column.clone());
            }
        }
        if model_cols.is_empty() {
            return Err(CopyError::NothingSelected);
        }
        let model_rows: Vec<usize> = rect
            .rows
            .clone()
            .filter_map(|d| map.rows.model_row(d))
            .collect();
        let partial = rect.rows.end == map.rows.len()
            && matches!(
                model.state(),
                IngestState::Waiting | IngestState::Streaming | IngestState::Paused
            );
        Ok(Self {
            format,
            target,
            limits,
            columns,
            model_cols,
            model_rows,
            next: 0,
            rows: Vec::new(),
            partial,
        })
    }

    /// The cells the copy holds.
    #[must_use]
    pub fn cells(&self) -> usize {
        self.model_rows.len().saturating_mul(self.model_cols.len())
    }

    /// Reads the next rows from the model, about `max_cells` cells worth. Returns `true` once
    /// every row is in. A row the model does not have is skipped.
    pub fn gather(&mut self, model: &GridModel, max_cells: usize) -> bool {
        let per_row = self.model_cols.len().max(1);
        let count = (max_cells / per_row).max(1);
        let end = self.next.saturating_add(count).min(self.model_rows.len());
        for &row_ix in &self.model_rows[self.next..end] {
            let Some(row) = model.row(row_ix) else {
                continue;
            };
            self.rows.push(Row(self
                .model_cols
                .iter()
                .map(|&c| row.get(c).map_or(Value::Null, |cell| cell.to_value()))
                .collect()));
        }
        self.next = end;
        self.next >= self.model_rows.len()
    }

    /// Renders the gathered rows.
    ///
    /// # Errors
    ///
    /// [`CopyError::TooLarge`] over the byte cap, [`CopyError::Render`] if a writer fails.
    pub fn render(self) -> Result<CopyOutput, CopyError> {
        let mut options = ExportOptions::default();
        match self.format {
            CopyFormat::Tsv | CopyFormat::TsvWithHeaders => {
                options.csv.delimiter = b'\t';
                options.csv.header = self.format == CopyFormat::TsvWithHeaders;
                "NULL".clone_into(&mut options.csv.null_text);
                options.csv.line_ending = LineEnding::Lf;
                options.csv.neutralize_formulas = false;
            }
            CopyFormat::Markdown | CopyFormat::Json => {}
            CopyFormat::Insert => {
                options.sql.dialect = self.target.dialect;
                options.sql.schema.clone_from(&self.target.schema);
                options.sql.table.clone_from(&self.target.table);
            }
        }
        let format: &dyn Format = match self.format {
            CopyFormat::Tsv | CopyFormat::TsvWithHeaders => &CsvFormat,
            CopyFormat::Markdown => &MarkdownFormat,
            CopyFormat::Json => &JsonFormat,
            CopyFormat::Insert => &SqlFormat,
        };
        // The TSV newline after the last row is trimmed below, so it does not count.
        let tsv = matches!(self.format, CopyFormat::Tsv | CopyFormat::TsvWithHeaders);
        let mut capped = CappedVec::new(self.limits.max_bytes.saturating_add(usize::from(tsv)));
        let written = write_all(format, &options, &self.columns, &self.rows, &mut capped);
        if capped.exceeded {
            return Err(CopyError::TooLarge);
        }
        written.map_err(|e| CopyError::Render(e.to_string()))?;
        let mut text = String::from_utf8(capped.bytes)
            .map_err(|_| CopyError::Render("the text is not valid UTF-8".to_owned()))?;
        if tsv && text.ends_with('\n') {
            text.pop();
        }
        Ok(CopyOutput {
            text,
            rows: self.rows.len(),
            format: self.format,
            partial: self.partial,
            assumed_dialect: self.target.assumed_dialect && self.format == CopyFormat::Insert,
        })
    }
}

fn write_all(
    format: &dyn Format,
    options: &ExportOptions,
    columns: &[Column],
    rows: &[Row],
    sink: &mut CappedVec,
) -> Result<(), ExportError> {
    let mut exporter = Exporter::new(format, options, Box::new(sink), ExportControl::default())?;
    exporter.begin(columns)?;
    exporter.write_batch(rows)?;
    exporter.finish()?;
    Ok(())
}

/// Plans, gathers and renders in one call.
///
/// # Errors
///
/// Any [`CopyError`] of [`CopyJob::new`] and [`CopyJob::render`].
pub fn copy_now(
    model: &GridModel,
    map: &DisplayMap<'_>,
    sel: &Selection,
    format: CopyFormat,
    target: InsertTarget,
    limits: CopyLimits,
) -> Result<CopyOutput, CopyError> {
    let mut job = CopyJob::new(model, map, sel, format, target, limits)?;
    job.gather(model, usize::MAX);
    job.render()
}

/// A byte sink that refuses to grow past a cap.
struct CappedVec {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl CappedVec {
    fn new(max: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max,
            exceeded: false,
        }
    }
}

impl Write for CappedVec {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(buf.len()) > self.max {
            self.exceeded = true;
            return Err(io::Error::other("copy size limit reached"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use sqler_driver_api::sqler_core::{DataType, ObjectKind, ObjectPath};
    use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
    use sqler_export::export_to_vec;

    use super::super::layout::ColumnLayout;
    use super::super::model::{GridLimits, grid_channel_with};
    use super::super::selection::{CellPos, IdentityRows, RowOrder};
    use super::super::spawn_ingest;
    use super::*;

    struct Scripted(std::vec::IntoIter<QueryEvent>);

    impl QueryStream for Scripted {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            Box::pin(std::future::ready(self.0.next().map(Ok)))
        }
    }

    fn wait_for(model: &mut GridModel, want: &IngestState) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while model.state() != want {
            assert!(Instant::now() < deadline, "stuck in {:?}", model.state());
            if model.poll(usize::MAX) == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn mixed_columns() -> Vec<Column> {
        vec![
            Column::new("id", DataType::Integer),
            Column::new("score", DataType::Float),
            Column::new("ok", DataType::Boolean),
            Column::new("note", DataType::Text),
            Column::new("doc", DataType::Json),
            Column::new("blob", DataType::Binary),
        ]
    }

    fn mixed_rows() -> Vec<Row> {
        vec![
            Row(vec![
                Value::Int(1),
                Value::Float(1.5),
                Value::Bool(true),
                Value::Text("say \"hi\"\tthere\nbye".into()),
                Value::Json(r#"{"a":[1,"x"]}"#.into()),
                Value::Bytes(vec![0x00, 0xff]),
            ]),
            Row(vec![
                Value::Int(2),
                Value::Null,
                Value::Bool(false),
                Value::Text("zażółć 🦀".into()),
                Value::Null,
                Value::Null,
            ]),
            Row(vec![
                Value::Int(3),
                Value::Float(-0.25),
                Value::Null,
                Value::Null,
                Value::Json("[]".into()),
                Value::Bytes(vec![]),
            ]),
        ]
    }

    /// A finished model holding `rows`.
    fn model_with(columns: Vec<Column>, rows: Vec<Row>) -> GridModel {
        let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        let events = vec![
            QueryEvent::Columns(columns),
            QueryEvent::Rows(rows),
            QueryEvent::Done {
                rows_affected: None,
            },
        ];
        drop(spawn_ingest(Scripted(events.into_iter()), sink).unwrap());
        wait_for(
            &mut model,
            &IngestState::Done {
                rows_affected: None,
            },
        );
        model
    }

    fn mixed_model() -> GridModel {
        model_with(mixed_columns(), mixed_rows())
    }

    fn layout(n: usize) -> ColumnLayout {
        ColumnLayout::new(vec![100.0; n])
    }

    fn pg() -> InsertTarget {
        InsertTarget::placeholder(Some(Dialect::Postgres))
    }

    fn select_all() -> Selection {
        let mut s = Selection::default();
        s.select_all();
        s
    }

    fn all(model: &GridModel, format: CopyFormat) -> Result<CopyOutput, CopyError> {
        let cols = layout(model.columns().len());
        let rows = IdentityRows(model.row_count());
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        copy_now(
            model,
            &map,
            &select_all(),
            format,
            pg(),
            CopyLimits::DEFAULT,
        )
    }

    fn key(mods: &str, key: &str) -> KeyInput {
        KeyInput {
            ctrl: mods.contains('c'),
            alt: mods.contains('a'),
            shift: mods.contains('s'),
            cmd: mods.contains('m'),
            key: key.into(),
        }
    }

    /// The reference output: `export_to_vec` with the options `render` is specified to use.
    fn reference(
        format: CopyFormat,
        columns: &[Column],
        rows: &[Row],
        target: &InsertTarget,
    ) -> String {
        let mut o = ExportOptions::default();
        let f: &dyn Format = match format {
            CopyFormat::Tsv | CopyFormat::TsvWithHeaders => {
                o.csv.delimiter = b'\t';
                o.csv.header = format == CopyFormat::TsvWithHeaders;
                o.csv.null_text = "NULL".into();
                o.csv.line_ending = LineEnding::Lf;
                &CsvFormat
            }
            CopyFormat::Markdown => &MarkdownFormat,
            CopyFormat::Json => &JsonFormat,
            CopyFormat::Insert => {
                o.sql.dialect = target.dialect;
                o.sql.schema.clone_from(&target.schema);
                o.sql.table.clone_from(&target.table);
                &SqlFormat
            }
        };
        let mut text = String::from_utf8(export_to_vec(f, &o, columns, rows).unwrap()).unwrap();
        if matches!(format, CopyFormat::Tsv | CopyFormat::TsvWithHeaders) {
            assert_eq!(text.pop(), Some('\n'));
        }
        text
    }

    #[test]
    fn command_ids_titles_and_labels() {
        let mut seen = HashSet::new();
        for f in CopyFormat::ALL {
            let id = f.command_id();
            assert!(id.to_string().starts_with("grid:"), "{f:?}");
            assert!(seen.insert(id.clone()), "duplicate {f:?}");
            assert_eq!(CopyFormat::from_command_id(&id), Some(f));
        }
        let ids: Vec<_> = CopyFormat::ALL.map(|f| f.command_id().to_string()).to_vec();
        assert_eq!(
            ids,
            [
                "grid:copy",
                "grid:copy-with-headers",
                "grid:copy-as-markdown",
                "grid:copy-as-json",
                "grid:copy-as-insert"
            ]
        );
        let titles: HashSet<_> = CopyFormat::ALL.iter().map(|f| f.title()).collect();
        assert_eq!(titles.len(), 5);
        assert_eq!(CopyFormat::Insert.title(), "Results: Copy as SQL INSERT");
        assert_eq!(CopyFormat::TsvWithHeaders.menu_label(), "Copy with Headers");
        assert_eq!(CopyFormat::Tsv.label(), "TSV");
        assert_eq!(CopyFormat::Insert.label(), "SQL INSERT");
        let other = CommandId::new("grid:fetch-more").unwrap();
        assert_eq!(CopyFormat::from_command_id(&other), None);
    }

    #[test]
    fn commands_register_without_clashing_with_the_grid_actions() {
        let mut r = CommandRegistry::new();
        crate::grid::register_commands(&mut r).unwrap();
        register_copy_commands(&mut r).unwrap();
        for f in CopyFormat::ALL {
            assert_eq!(r.get(&f.command_id()).unwrap().title(), f.title());
        }
        assert!(register_copy_commands(&mut r).is_err());
    }

    #[test]
    fn copy_keys_on_both_platforms() {
        for (platform, m, other) in [(Platform::MacOs, "m", "c"), (Platform::Other, "c", "m")] {
            let k = |mods: &str, focused: bool, overlay: bool| {
                copy_key_format(&key(mods, "c"), platform, focused, overlay, false)
            };
            assert_eq!(k(m, true, false), Some(CopyFormat::Tsv));
            assert_eq!(
                k(&format!("{m}s"), true, false),
                Some(CopyFormat::TsvWithHeaders)
            );
            assert_eq!(k(m, false, false), None, "table not focused");
            assert_eq!(k(m, true, true), None, "overlay open");
            assert_eq!(
                copy_key_format(&key(m, "c"), platform, true, false, true),
                None,
                "a cell editor is open: its input owns copy"
            );
            assert_eq!(k(&format!("{m}a"), true, false), None, "alt");
            assert_eq!(k(other, true, false), None, "other modifier");
            assert_eq!(k(&format!("{m}{other}"), true, false), None);
            assert_eq!(k("", true, false), None, "no modifier");
            assert_eq!(k("s", true, false), None);
            assert_eq!(
                copy_key_format(&key(m, "x"), platform, true, false, false),
                None,
                "another key"
            );
        }
    }

    #[test]
    fn insert_targets() {
        let p = InsertTarget::placeholder(Some(Dialect::Sqlite));
        assert_eq!(p.table, "table_name");
        assert_eq!(p.table, PLACEHOLDER_TABLE);
        assert_eq!(p.dialect, Dialect::Sqlite);
        assert!(!p.assumed_dialect);
        let unknown = InsertTarget::placeholder(None);
        assert_eq!(unknown.dialect, Dialect::Postgres);
        assert!(unknown.assumed_dialect);

        let path = ObjectPath::root().in_database("shop");
        let obj = DbObject::new(ObjectKind::Table, path, "t");
        let my = InsertTarget::for_object(Some(Dialect::MySql), &obj);
        assert_eq!(my.schema.as_deref(), Some("shop"));
        assert_eq!(my.table, "t");
        let pgt = InsertTarget::for_object(Some(Dialect::Postgres), &obj);
        assert_eq!(pgt.schema, None, "Postgres ignores the database");
        let path = ObjectPath::root().in_database("db").in_schema("public");
        let obj = DbObject::new(ObjectKind::View, path, "v");
        assert_eq!(
            InsertTarget::for_object(Some(Dialect::MySql), &obj)
                .schema
                .as_deref(),
            Some("public")
        );
        assert!(InsertTarget::for_object(None, &obj).assumed_dialect);
    }

    #[test]
    fn every_format_matches_the_export_writers() {
        let model = mixed_model();
        for format in CopyFormat::ALL {
            let out = all(&model, format).unwrap();
            assert_eq!(
                out.text,
                reference(format, &mixed_columns(), &mixed_rows(), &pg()),
                "{format:?}"
            );
            assert_eq!(out.rows, 3);
            assert_eq!(out.format, format);
            assert!(!out.partial && !out.assumed_dialect);
        }
    }

    #[test]
    fn key_literals() {
        let model = mixed_model();
        let tsv = all(&model, CopyFormat::Tsv).unwrap().text;
        let lines: Vec<&str> = tsv.split('\n').collect();
        // The text cell holds a newline inside quotes, so it spans two lines.
        assert!(tsv.starts_with("1\t1.5\ttrue\t\"say \"\"hi\"\"\tthere\nbye\"\t"));
        assert!(!tsv.ends_with('\n'));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("2\tNULL\tfalse\tzażółć 🦀\tNULL\tNULL"))
        );
        let with = all(&model, CopyFormat::TsvWithHeaders).unwrap().text;
        assert!(with.starts_with("id\tscore\tok\tnote\tdoc\tblob\n1\t"));
        let md = all(&model, CopyFormat::Markdown).unwrap().text;
        assert!(md.starts_with("| id | score |"), "{md}");
        assert!(md.contains("NULL"));
        let json = all(&model, CopyFormat::Json).unwrap().text;
        assert!(json.contains("null"), "{json}");
        let sql = all(&model, CopyFormat::Insert).unwrap().text;
        assert!(sql.contains("INSERT INTO \"table_name\""), "{sql}");
        assert!(sql.contains("NULL"));
    }

    #[test]
    fn a_single_cell_has_no_newline() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let mut s = Selection::default();
        s.click(CellPos { row: 1, col: 3 }, false);
        let out = copy_now(&model, &map, &s, CopyFormat::Tsv, pg(), CopyLimits::DEFAULT).unwrap();
        assert_eq!(out.text, "zażółć 🦀");
        s.click(CellPos { row: 1, col: 1 }, false);
        let out = copy_now(&model, &map, &s, CopyFormat::Tsv, pg(), CopyLimits::DEFAULT).unwrap();
        assert_eq!(out.text, "NULL");
    }

    #[test]
    fn insert_uses_the_target() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let path = ObjectPath::root().in_database("shop");
        let target = InsertTarget::for_object(
            Some(Dialect::MySql),
            &DbObject::new(ObjectKind::Table, path, "t"),
        );
        let out = copy_now(
            &model,
            &map,
            &select_all(),
            CopyFormat::Insert,
            target.clone(),
            CopyLimits::DEFAULT,
        )
        .unwrap();
        assert!(out.text.contains("INSERT INTO `shop`.`t`"), "{}", out.text);
        assert_eq!(
            out.text,
            reference(CopyFormat::Insert, &mixed_columns(), &mixed_rows(), &target)
        );
        let assumed = InsertTarget::placeholder(None);
        let out = copy_now(
            &model,
            &map,
            &select_all(),
            CopyFormat::Insert,
            assumed,
            CopyLimits::DEFAULT,
        )
        .unwrap();
        assert!(out.assumed_dialect);
        assert!(out.text.contains("\"table_name\""));
    }

    #[test]
    fn column_layout_changes_the_order() {
        let model = mixed_model();
        let mut cols = layout(6);
        assert!(cols.move_display(5, 0)); // blob first
        assert!(cols.pin(3)); // note pinned: order note blob id score ok doc
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let out = copy_now(
            &model,
            &map,
            &select_all(),
            CopyFormat::TsvWithHeaders,
            pg(),
            CopyLimits::DEFAULT,
        )
        .unwrap();
        let order: Vec<usize> = (0..6).filter_map(|d| cols.model_column(d)).collect();
        let columns: Vec<Column> = order.iter().map(|&c| mixed_columns()[c].clone()).collect();
        let data: Vec<Row> = mixed_rows()
            .into_iter()
            .map(|r| Row(order.iter().map(|&c| r.0[c].clone()).collect()))
            .collect();
        assert_eq!(
            out.text,
            reference(CopyFormat::TsvWithHeaders, &columns, &data, &pg())
        );
        assert!(out.text.starts_with("note\t"), "{}", out.text);
    }

    struct Reversed(usize);

    impl RowOrder for Reversed {
        fn len(&self) -> usize {
            self.0
        }
        fn model_row(&self, display: usize) -> Option<usize> {
            (display < self.0).then(|| self.0 - 1 - display)
        }
        fn display_row(&self, model: usize) -> Option<usize> {
            (model < self.0).then(|| self.0 - 1 - model)
        }
    }

    #[test]
    fn row_order_changes_the_rows_and_the_selection_kinds_work() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = Reversed(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let copy = |s: &Selection| {
            copy_now(&model, &map, s, CopyFormat::Json, pg(), CopyLimits::DEFAULT).unwrap()
        };
        let reversed: Vec<Row> = mixed_rows().into_iter().rev().collect();
        assert_eq!(
            copy(&select_all()).text,
            reference(CopyFormat::Json, &mixed_columns(), &reversed, &pg())
        );

        // Display rows 0..=1 are model rows 2 and 1.
        let mut s = Selection::default();
        s.click(CellPos { row: 1, col: 0 }, false);
        s.select_row();
        s.click(CellPos { row: 2, col: 0 }, true);
        let out = copy(&s);
        assert_eq!(out.rows, 2);
        assert_eq!(
            out.text,
            reference(CopyFormat::Json, &mixed_columns(), &reversed[..2], &pg())
        );

        // A column selection takes every row of that column.
        let mut s = Selection::default();
        s.click(CellPos { row: 0, col: 0 }, false);
        s.select_column();
        let out = copy(&s);
        assert_eq!(out.rows, 3);
        let id_rows: Vec<Row> = reversed.iter().map(|r| Row(vec![r.0[0].clone()])).collect();
        assert_eq!(
            out.text,
            reference(CopyFormat::Json, &mixed_columns()[..1], &id_rows, &pg())
        );
    }

    #[test]
    fn limits_and_errors() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let small = CopyLimits {
            max_cells: 5,
            max_bytes: usize::MAX,
        };
        let err =
            CopyJob::new(&model, &map, &select_all(), CopyFormat::Tsv, pg(), small).unwrap_err();
        assert!(matches!(err, CopyError::TooManyCells { cells: 18 }));
        assert_eq!(
            err.to_string(),
            "The selection has 18 cells; copying is limited to 1,000,000 cells. Select fewer rows or columns."
        );
        let tiny = CopyLimits {
            max_cells: usize::MAX,
            max_bytes: 20,
        };
        let err = copy_now(&model, &map, &select_all(), CopyFormat::Json, pg(), tiny).unwrap_err();
        assert!(matches!(err, CopyError::TooLarge));
        assert_eq!(
            err.to_string(),
            "The copied text would be larger than 50 MiB; nothing was copied. Select fewer rows or columns."
        );
        let err = copy_now(
            &model,
            &map,
            &Selection::default(),
            CopyFormat::Tsv,
            pg(),
            CopyLimits::DEFAULT,
        )
        .unwrap_err();
        assert!(matches!(err, CopyError::NothingSelected));
        assert_eq!(err.to_string(), "Select cells to copy first.");
        assert_eq!(
            CopyError::Render("x".into()).to_string(),
            "Could not copy: x"
        );
    }

    #[test]
    fn the_byte_cap_does_not_count_the_trimmed_tsv_newline() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let len = copy_now(
            &model,
            &map,
            &select_all(),
            CopyFormat::Tsv,
            pg(),
            CopyLimits::DEFAULT,
        )
        .unwrap()
        .text
        .len();
        let at = |max_bytes| {
            let limits = CopyLimits {
                max_cells: usize::MAX,
                max_bytes,
            };
            copy_now(&model, &map, &select_all(), CopyFormat::Tsv, pg(), limits)
        };
        assert_eq!(at(len).unwrap().text.len(), len);
        assert!(matches!(at(len - 1), Err(CopyError::TooLarge)));
    }

    #[test]
    fn an_empty_grid_has_nothing_to_copy() {
        let model = model_with(mixed_columns(), vec![]);
        assert!(matches!(
            all(&model, CopyFormat::Tsv),
            Err(CopyError::NothingSelected)
        ));
    }

    #[test]
    fn a_paused_model_copies_what_arrived_and_says_so() {
        let (mut model, sink) = grid_channel_with(GridLimits::rows(2));
        let events = vec![
            QueryEvent::Columns(vec![Column::new("a", DataType::Integer)]),
            QueryEvent::Rows((0..3).map(|i| Row(vec![Value::Int(i)])).collect()),
            QueryEvent::Rows((3..6).map(|i| Row(vec![Value::Int(i)])).collect()),
            QueryEvent::Done {
                rows_affected: None,
            },
        ];
        let _handle = spawn_ingest(Scripted(events.into_iter()), sink).unwrap();
        wait_for(&mut model, &IngestState::Paused);
        let out = all(&model, CopyFormat::Tsv).unwrap();
        assert!(out.partial);
        assert_eq!(out.text, "0\n1");
        assert_eq!(
            success_notice(&out, 2).as_deref(),
            Some("Copied 2 rows as TSV. Only the rows fetched so far were copied.")
        );
        // A selection that stops short of the last row is not partial.
        let cols = layout(1);
        let rows = IdentityRows(model.row_count());
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let mut s = Selection::default();
        s.click(CellPos { row: 0, col: 0 }, false);
        let out = copy_now(&model, &map, &s, CopyFormat::Tsv, pg(), CopyLimits::DEFAULT).unwrap();
        assert!(!out.partial);
    }

    #[test]
    fn gather_in_slices_equals_one_call() {
        let model = mixed_model();
        let cols = layout(6);
        let rows = IdentityRows(3);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        for format in CopyFormat::ALL {
            let mut job = CopyJob::new(
                &model,
                &map,
                &select_all(),
                format,
                pg(),
                CopyLimits::DEFAULT,
            )
            .unwrap();
            assert_eq!(job.cells(), 18);
            let mut calls = 1;
            while !job.gather(&model, 2) {
                calls += 1;
            }
            assert_eq!(calls, 3, "one row per call at least");
            assert_eq!(
                job.render().unwrap().text,
                all(&model, format).unwrap().text,
                "{format:?}"
            );
        }
    }

    #[test]
    fn the_job_is_send_and_debug_hides_the_data() {
        fn assert_send<T: Send>() {}
        assert_send::<CopyJob>();
        assert_send::<CopyOutput>();
        let model = model_with(
            vec![Column::new("s", DataType::Text)],
            vec![Row(vec![Value::Text("hunter2".into())])],
        );
        let cols = layout(1);
        let rows = IdentityRows(1);
        let map = DisplayMap {
            rows: &rows,
            columns: &cols,
        };
        let mut job = CopyJob::new(
            &model,
            &map,
            &select_all(),
            CopyFormat::Tsv,
            pg(),
            CopyLimits::DEFAULT,
        )
        .unwrap();
        job.gather(&model, usize::MAX);
        assert!(!format!("{job:?}").contains("hunter2"));
        let out = job.render().unwrap();
        assert!(!format!("{out:?}").contains("hunter2"));
        assert_eq!(out.text, "hunter2");
    }

    #[test]
    fn success_notices() {
        let out = |partial, assumed, format, rows| CopyOutput {
            text: String::new(),
            rows,
            format,
            partial,
            assumed_dialect: assumed,
        };
        assert_eq!(
            success_notice(&out(false, false, CopyFormat::Tsv, 5), 99_999),
            None
        );
        assert_eq!(
            success_notice(&out(false, false, CopyFormat::Json, 12_345), NOTIFY_CELLS).as_deref(),
            Some("Copied 12,345 rows as JSON.")
        );
        assert_eq!(
            success_notice(&out(true, false, CopyFormat::Markdown, 2), 4).as_deref(),
            Some("Copied 2 rows as Markdown. Only the rows fetched so far were copied.")
        );
        assert_eq!(
            success_notice(&out(false, true, CopyFormat::Insert, 2), 4).as_deref(),
            Some(
                "Copied 2 rows as SQL INSERT. This connection has no SQL dialect; the INSERT uses PostgreSQL quoting."
            )
        );
    }
}
