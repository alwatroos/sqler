// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridEditor`: the headless editing state of one results grid.
//!
//! It decides whether the grid is editable, runs the cell editor, holds the [`StagedEdits`], and
//! keeps what the grid shows after an apply (the overlay). It only reads the `GridModel`, which is
//! never changed: every shown cell is the model's cell, the staged value, or the applied value.
//!
//! Rows are *grid rows*. A row below `model.row_count()` is a model row; above come the rows
//! inserted by the last apply (read-only until the data is reloaded) and then the staged new rows.
//! `CellPos` is in model space (a sort or filter only permutes the view, ADR-0030), so a cell's
//! row is the key of its staged edit whatever is on screen.
//!
//! Nothing here formats a value with `{:?}`: `Debug` of every type in this module prints counts and
//! kinds only.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqler_driver_api::sqler_core::{Column, DataType, DbObject, ObjectKind, Row, Table, Value};
use sqler_sql::Dialect;

use super::apply::{ApplyFailure, ApplyOutcome};
use super::editability::{
    EditRequest, EditSource, Editability, ReadOnlyReason, RowIdentity, check_editable,
};
use super::input::{EditorKind, edit_text, editor_kind, parse_cell};
use super::plan::ApplyPlan;
use super::staged::{EditError, ModelRow, NewRowId, RowRef, StagedEdits};
use crate::grid::{CellPos, GridModel, IngestState, cell_text};
use crate::tabs::TabId;

/// Shown when the outcome of `COMMIT` is unknown; Apply stays blocked until Discard.
pub const COMMIT_UNKNOWN: &str = "The outcome is uncertain: re-read before retrying.";

/// Part of every text the preview and the confirmation show.
pub const LAST_WRITER_WINS: &str = "Rows are found by their key and the values that were read. \
A change someone else made to such a row since then is overwritten (last writer wins); a keyless \
match fails instead and nothing is written.";

/// The warning shown before the user opts into editing a result that has no usable key.
pub const KEYLESS_WARNING: &str = "This result has no usable key. Rows will be found by all their \
values that were read. If two rows are identical, or a row changed since it was read, the apply \
fails and rolls back, and nothing is written.";

const NO_SOURCE: &str = "This grid has no source table.";
const NOT_LOADED: &str = "Wait until the rows have loaded.";
const APPLYING: &str = "Changes are being applied.";
const APPLIED_ROW: &str =
    "This row was changed by the last apply. Reload the data to edit it again.";
const APPLIED_INSERT: &str =
    "Rows added by the last apply are read-only until the data is reloaded.";

/// The preview text of a plan: the statements with their values, then [`LAST_WRITER_WINS`].
#[must_use]
pub fn preview_text(plan: &ApplyPlan) -> String {
    format!("{}\n{LAST_WRITER_WINS}\n", plan.render_preview())
}

/// Identifies one grid: a table-data tab has one (`result: None`), a query tab one per result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridKey {
    /// The tab.
    pub tab: TabId,
    /// The position among the grids of the tab's run; `None` for a table-data tab.
    pub result: Option<usize>,
}

/// Where the grid's rows come from.
#[derive(Clone, PartialEq)]
pub enum EditOrigin {
    /// A table-data tab showing this object.
    TableData(DbObject),
    /// The result of this SQL text.
    QueryResult(String),
}

impl std::fmt::Debug for EditOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TableData(object) => f
                .debug_struct("TableData")
                .field("kind", &object.kind)
                .finish_non_exhaustive(),
            Self::QueryResult(sql) => f
                .debug_struct("QueryResult")
                .field("sql_len", &sql.len())
                .finish(),
        }
    }
}

/// What the workspace knows about a grid's source.
#[derive(Clone)]
pub struct EditContext {
    /// The connection's dialect.
    pub dialect: Option<Dialect>,
    /// Where the rows come from.
    pub origin: EditOrigin,
    /// The catalog entry of the source table, if loaded.
    pub table: Option<Arc<Table>>,
    /// What the catalog says the named relation is, if it was found (a view is not editable).
    pub relation_kind: Option<ObjectKind>,
    /// The schema the server resolved the unqualified table name to (the schema probe).
    pub resolved_schema: Option<String>,
    /// Whether the connection is `prod`. Only for a badge: the gate is derived again from the
    /// connection when the apply is requested and confirmed.
    pub production: bool,
    /// Why an apply cannot start now (a query is running, an apply is in flight, an open
    /// transaction); `None` when it can.
    pub apply_block: Option<String>,
}

impl std::fmt::Debug for EditContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditContext")
            .field("dialect", &self.dialect)
            .field("origin", &self.origin)
            .field("table_loaded", &self.table.is_some())
            .field("relation_kind", &self.relation_kind)
            .field("resolved_schema", &self.resolved_schema)
            .field("production", &self.production)
            .field("apply_blocked", &self.apply_block.is_some())
            .finish()
    }
}

/// Whether and why a grid can be edited.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditState {
    /// The columns have not arrived yet.
    Loading,
    /// There is nothing to edit here (no source table).
    Unavailable(String),
    /// Read-only, for this reason.
    ReadOnly(ReadOnlyReason),
    /// Cells can be edited.
    Editable,
}

/// How the grid marks a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CellMark {
    /// Nothing special.
    None,
    /// The value has a pending edit.
    Changed,
    /// A cell of a row staged for insertion.
    New,
    /// A cell of a new row that was left empty (the server's default applies).
    Unset,
    /// The row is staged for deletion.
    Deleted,
    /// The row was deleted by the last apply.
    DeletedApplied,
    /// The last apply failed at this row.
    Failed,
}

/// What the grid draws for one cell.
#[derive(Clone, PartialEq, Eq)]
pub struct DisplayCell {
    /// The text (at most `MAX_CELL_CHARS` characters; `NULL` for a null value).
    pub text: String,
    /// How the cell is marked.
    pub mark: CellMark,
    /// Whether the value is `NULL`.
    pub null: bool,
}

impl std::fmt::Debug for DisplayCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayCell")
            .field("text_len", &self.text.len())
            .field("mark", &self.mark)
            .field("null", &self.null)
            .finish()
    }
}

/// What the user entered in the cell editor.
#[derive(Clone, PartialEq, Eq)]
pub enum EditorInput {
    /// Typed text, parsed by the column type.
    Text(String),
    /// The toggle of a boolean column.
    Bool(bool),
    /// "Set NULL".
    Null,
}

impl std::fmt::Debug for EditorInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => f.debug_struct("Text").field("len", &t.len()).finish(),
            Self::Bool(_) => f.write_str("Bool"),
            Self::Null => f.write_str("Null"),
        }
    }
}

/// An open cell editor.
#[derive(Clone)]
pub struct CellEditor {
    /// The cell, by grid row and model column.
    pub pos: CellPos,
    /// The column's name in the result.
    pub column: String,
    /// The type the input is parsed as.
    pub data_type: DataType,
    /// Which editor to show.
    pub kind: EditorKind,
    /// Whether the column accepts `NULL`.
    pub nullable: bool,
    /// What the editor starts with: the current value (the pending one if there is one).
    pub initial: EditorInput,
}

impl std::fmt::Debug for CellEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellEditor")
            .field("pos", &self.pos)
            .field("kind", &self.kind)
            .field("nullable", &self.nullable)
            .field("initial", &self.initial)
            .finish_non_exhaustive()
    }
}

/// What the grid shows after a commit, until the data is reloaded.
#[derive(Default)]
struct Overlay {
    /// Values written to model cells.
    cells: HashMap<(usize, usize), Value>,
    /// Model rows that have an applied update.
    updated: HashSet<usize>,
    /// Model rows that were deleted.
    deleted: HashSet<usize>,
    /// Rows that were inserted, in order; `None` is a cell that was left to the server.
    inserted: Vec<Vec<Option<Value>>>,
}

impl Overlay {
    fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }
}

/// A checked edit target: the cell exists, the grid is editable and the row may be changed.
enum Target {
    /// A model row with its original values.
    Model(usize, Row),
    /// A staged new row.
    New(NewRowId),
}

/// The editing state of one grid.
pub struct GridEditor {
    ctx: Option<EditContext>,
    columns: Vec<Column>,
    allow_keyless: bool,
    state: EditState,
    staged: Option<StagedEdits>,
    editing: Option<CellEditor>,
    overlay: Overlay,
    failed: Option<RowRef>,
    applying: Option<Vec<RowRef>>,
    apply_block: Option<String>,
}

impl Default for GridEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GridEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridEditor")
            .field("state", &self.state)
            .field("pending", &self.pending())
            .field("editing", &self.editing.is_some())
            .field("applying", &self.applying.is_some())
            .field("applied_rows", &self.overlay.inserted.len())
            .finish_non_exhaustive()
    }
}

impl GridEditor {
    /// An editor with no source: nothing can be edited until [`Self::set_context`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            ctx: None,
            columns: Vec::new(),
            allow_keyless: false,
            state: EditState::Unavailable(NO_SOURCE.to_owned()),
            staged: None,
            editing: None,
            overlay: Overlay::default(),
            failed: None,
            applying: None,
            apply_block: None,
        }
    }

    // ---- state ----

    /// Sets what is known about the grid's source and decides editability.
    ///
    /// While edits are staged the target is frozen: only the context's other fields change, and
    /// the decision is taken again when the edits are gone. Call it again when the catalog, the
    /// schema probe or the apply block change.
    pub fn set_context(&mut self, ctx: EditContext, columns: &[Column]) {
        self.ctx = Some(ctx);
        self.columns = columns.to_vec();
        self.reevaluate();
    }

    /// The result's columns arrived or changed: decides again with the stored context.
    pub fn set_columns(&mut self, columns: &[Column]) {
        self.columns = columns.to_vec();
        self.reevaluate();
    }

    fn reevaluate(&mut self) {
        if self.pending() > 0 || self.applying.is_some() {
            return;
        }
        let (state, staged) = self.evaluate();
        self.state = state;
        self.staged = staged;
        if self.staged.is_none() {
            self.editing = None;
        }
    }

    fn evaluate(&self) -> (EditState, Option<StagedEdits>) {
        let Some(ctx) = &self.ctx else {
            return (EditState::Unavailable(NO_SOURCE.to_owned()), None);
        };
        if self.columns.is_empty() {
            return (EditState::Loading, None);
        }
        let source = match &ctx.origin {
            EditOrigin::TableData(object) => EditSource::TableData { object },
            EditOrigin::QueryResult(sql) => EditSource::QueryResult { sql },
        };
        if let (EditOrigin::QueryResult(_), Some(kind)) = (&ctx.origin, ctx.relation_kind)
            && kind != ObjectKind::Table
        {
            return (
                EditState::ReadOnly(ReadOnlyReason::NotATable { kind }),
                None,
            );
        }
        let request = EditRequest {
            dialect: ctx.dialect,
            source,
            table: ctx.table.as_deref(),
            unique_keys: &[],
            columns: &self.columns,
            allow_keyless: self.allow_keyless,
            resolved_schema: ctx.resolved_schema.as_deref(),
        };
        match check_editable(&request) {
            Editability::Editable(target) => (EditState::Editable, Some(StagedEdits::new(target))),
            Editability::ReadOnly(reason) => (EditState::ReadOnly(reason), None),
        }
    }

    /// Whether and why the grid can be edited, as of the last [`Self::set_context`]. The rows may
    /// still be loading: see [`Self::state_for`].
    #[must_use]
    pub fn state(&self) -> &EditState {
        &self.state
    }

    /// [`Self::state`], or [`EditState::Loading`] while the rows are still arriving.
    #[must_use]
    pub fn state_for(&self, model: &GridModel) -> EditState {
        match (&self.state, model.state()) {
            (EditState::Editable, IngestState::Waiting | IngestState::Streaming) => {
                EditState::Loading
            }
            (state, _) => state.clone(),
        }
    }

    /// The sentence that says why the grid is read-only; `None` when editable or still loading.
    #[must_use]
    pub fn read_only_message(&self) -> Option<String> {
        match &self.state {
            EditState::ReadOnly(reason) => Some(reason.message()),
            EditState::Unavailable(message) => Some(message.clone()),
            EditState::Loading | EditState::Editable => None,
        }
    }

    /// Opts into editing a result that has no usable key: rows are then found by all their
    /// values. Only for a reason that [`ReadOnlyReason::can_opt_in`] allows; returns whether the
    /// grid is editable now. The view shows [`KEYLESS_WARNING`] first.
    pub fn allow_keyless(&mut self, columns: &[Column]) -> bool {
        let EditState::ReadOnly(reason) = &self.state else {
            return false;
        };
        if !reason.can_opt_in() || self.pending() > 0 {
            return false;
        }
        self.allow_keyless = true;
        if !columns.is_empty() {
            self.columns = columns.to_vec();
        }
        self.reevaluate();
        self.state == EditState::Editable
    }

    /// Whether the grid is editable by matching rows on all their values (the opt-in).
    #[must_use]
    pub fn is_keyless(&self) -> bool {
        self.staged
            .as_ref()
            .is_some_and(|s| matches!(s.target().identity(), RowIdentity::AllColumns))
    }

    /// The context last set, for the badge and the apply block.
    #[must_use]
    pub fn context(&self) -> Option<&EditContext> {
        self.ctx.as_ref()
    }

    // ---- counts ----

    /// The number of statements the pending changes make.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.staged.as_ref().map_or(0, StagedEdits::change_count)
    }

    /// Rows shown below the model's: the rows of the last apply, then the staged new rows.
    #[must_use]
    pub fn extra_rows(&self) -> usize {
        self.overlay.inserted.len() + self.staged.as_ref().map_or(0, |s| s.inserts.len())
    }

    /// The status text of the pending changes, `3 pending: 1 edited, 1 deleted, 1 new`.
    #[must_use]
    pub fn status_text(&self) -> Option<String> {
        let staged = self.staged.as_ref()?;
        if staged.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for (n, what) in [
            (staged.updates.len(), "edited"),
            (staged.deletes.len(), "deleted"),
            (staged.inserts.len(), "new"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        Some(format!(
            "{} pending: {}",
            staged.change_count(),
            parts.join(", ")
        ))
    }

    /// Whether an apply is in flight.
    #[must_use]
    pub fn is_applying(&self) -> bool {
        self.applying.is_some()
    }

    /// Why Apply is refused right now, if it is: the unknown outcome of a commit first, then the
    /// workspace's reason.
    #[must_use]
    pub fn apply_blocked(&self) -> Option<&str> {
        self.apply_block
            .as_deref()
            .or_else(|| self.ctx.as_ref().and_then(|c| c.apply_block.as_deref()))
    }

    // ---- checks ----

    fn check_can_edit(&self, model: &GridModel) -> Result<&StagedEdits, String> {
        match &self.state {
            EditState::Editable => {}
            EditState::Loading => return Err(NOT_LOADED.to_owned()),
            EditState::Unavailable(message) => return Err(message.clone()),
            EditState::ReadOnly(reason) => return Err(reason.message()),
        }
        if self.applying.is_some() {
            return Err(APPLYING.to_owned());
        }
        if matches!(
            model.state(),
            IngestState::Waiting | IngestState::Streaming | IngestState::Failed(_)
        ) {
            return Err(NOT_LOADED.to_owned());
        }
        self.staged.as_ref().ok_or_else(|| NO_SOURCE.to_owned())
    }

    /// Type, nullability and name of result column `col`: the catalog's, else the result's.
    fn column_meta(&self, staged: &StagedEdits, col: usize) -> Option<(DataType, bool, String)> {
        let result = self.columns.get(col)?;
        let target = staged.target();
        let name = target.table_column(col);
        let info = target.table().columns.iter().find(|c| c.name == name);
        Some(match info {
            Some(info) => (info.data_type, info.nullable, result.name.clone()),
            None => (
                result.data_type,
                result.nullable.unwrap_or(true),
                result.name.clone(),
            ),
        })
    }

    fn null_key_message(staged: &StagedEdits, original: &Row) -> Option<String> {
        let RowIdentity::Key { columns } = staged.target().identity() else {
            return None;
        };
        columns
            .iter()
            .find(|&&c| matches!(original.get(c), Some(Value::Null) | None))
            .map(|&c| {
                format!(
                    "This row has no value in key column {}, so it cannot be found again.",
                    staged.target().table_column(c)
                )
            })
    }

    /// Checks that `pos` may be changed and returns what to change.
    fn target_of(&self, model: &GridModel, pos: CellPos) -> Result<Target, String> {
        let staged = self.check_can_edit(model)?;
        let (data_type, _, _) = self
            .column_meta(staged, pos.col)
            .ok_or_else(|| "There is no such column.".to_owned())?;
        if editor_kind(data_type) == EditorKind::ReadOnlyBinary {
            return Err("Binary cells are read-only.".to_owned());
        }
        let base = model.row_count();
        if pos.row < base {
            if self.overlay.updated.contains(&pos.row) || self.overlay.deleted.contains(&pos.row) {
                return Err(APPLIED_ROW.to_owned());
            }
            if staged.is_deleted(ModelRow(pos.row)) {
                return Err("This row is marked for deletion. Undo the deletion first.".to_owned());
            }
            let original = model
                .row(pos.row)
                .map(|r| r.to_row())
                .ok_or_else(|| "There is no such row.".to_owned())?;
            if let Some(message) = Self::null_key_message(staged, &original) {
                return Err(message);
            }
            return Ok(Target::Model(pos.row, original));
        }
        let index = pos.row - base;
        if index < self.overlay.inserted.len() {
            return Err(APPLIED_INSERT.to_owned());
        }
        staged
            .inserts
            .get(index - self.overlay.inserted.len())
            .map(|(id, _)| Target::New(*id))
            .ok_or_else(|| "There is no such row.".to_owned())
    }

    fn user_text(error: &EditError) -> String {
        match error {
            EditError::NullKey { column, .. } => format!(
                "This row has no value in key column {column}, so it cannot be found again."
            ),
            EditError::RowDeleted(_) => {
                "This row is marked for deletion. Undo the deletion first.".to_owned()
            }
            EditError::NothingStaged => "There are no pending changes.".to_owned(),
            other => other.to_string(),
        }
    }

    // ---- the cell editor ----

    /// Opens the editor on `pos`.
    ///
    /// # Errors
    ///
    /// A sentence for the user when the grid is read-only or loading, an apply is running, the
    /// column is binary, the row is deleted, was changed by the last apply, was added by it, or
    /// has no value in a key column.
    pub fn start_edit(&mut self, model: &GridModel, pos: CellPos) -> Result<&CellEditor, String> {
        let target = self.target_of(model, pos)?;
        let Some(staged) = self.staged.as_ref() else {
            return Err(NO_SOURCE.to_owned());
        };
        let Some((data_type, nullable, column)) = self.column_meta(staged, pos.col) else {
            return Err("There is no such column.".to_owned());
        };
        let kind = editor_kind(data_type);
        let current: Option<Value> = match &target {
            Target::Model(row, original) => staged
                .staged_value(ModelRow(*row), pos.col)
                .cloned()
                .or_else(|| original.get(pos.col).cloned()),
            Target::New(id) => staged
                .inserts
                .iter()
                .find(|(i, _)| i == id)
                .and_then(|(_, cells)| cells.get(pos.col).cloned().flatten()),
        };
        let initial = match current {
            Some(Value::Null) => EditorInput::Null,
            Some(Value::Bool(b)) if kind == EditorKind::Boolean => EditorInput::Bool(b),
            Some(v) => EditorInput::Text(edit_text(&v)),
            None if kind == EditorKind::Boolean => EditorInput::Bool(false),
            None => EditorInput::Text(String::new()),
        };
        Ok(&*self.editing.insert(CellEditor {
            pos,
            column,
            data_type,
            kind,
            nullable,
            initial,
        }))
    }

    /// The open cell editor, if any.
    #[must_use]
    pub fn editing(&self) -> Option<&CellEditor> {
        self.editing.as_ref()
    }

    /// Closes the cell editor without staging anything.
    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// Parses `input` for the open editor's column and stages it. On an error the editor stays
    /// open so the user can fix the input.
    ///
    /// # Errors
    ///
    /// A sentence for the user: no editor is open, the input is not valid for the type, `NULL`
    /// for a `NOT NULL` column, or the cell can no longer be changed.
    pub fn commit_edit(&mut self, model: &GridModel, input: EditorInput) -> Result<(), String> {
        let Some(editor) = self.editing.clone() else {
            return Err("No cell is being edited.".to_owned());
        };
        let value = match input {
            EditorInput::Null => {
                if !editor.nullable {
                    return Err(format!("Column {} is NOT NULL.", editor.column));
                }
                Value::Null
            }
            EditorInput::Bool(b) => {
                if editor.kind != EditorKind::Boolean {
                    return Err("This column is not a boolean.".to_owned());
                }
                Value::Bool(b)
            }
            EditorInput::Text(text) => parse_cell(editor.data_type, &text)?,
        };
        let target = match self.target_of(model, editor.pos) {
            Ok(target) => target,
            Err(message) => {
                self.editing = None;
                return Err(message);
            }
        };
        self.stage(target, editor.pos.col, value)?;
        self.editing = None;
        Ok(())
    }

    fn stage(&mut self, target: Target, col: usize, value: Value) -> Result<(), String> {
        let Some(staged) = self.staged.as_mut() else {
            return Err(NO_SOURCE.to_owned());
        };
        let result = match target {
            Target::Model(row, original) => staged.set_cell(ModelRow(row), &original, col, value),
            Target::New(id) => staged.set_new_cell(id, col, Some(value)),
        };
        result.map_err(|e| Self::user_text(&e))?;
        self.failed = None;
        Ok(())
    }

    /// Stages `NULL` in every cell of `cells`, or in none of them.
    ///
    /// # Errors
    ///
    /// A sentence for the user: a cell cannot be changed, is binary, or is in a `NOT NULL`
    /// column.
    pub fn set_null(&mut self, model: &GridModel, cells: &[CellPos]) -> Result<usize, String> {
        let mut targets = Vec::with_capacity(cells.len());
        for &pos in cells {
            let target = self.target_of(model, pos)?;
            let staged = self.check_can_edit(model)?;
            if let Some((_, nullable, column)) = self.column_meta(staged, pos.col)
                && !nullable
            {
                return Err(format!("Column {column} is NOT NULL."));
            }
            targets.push((target, pos.col));
        }
        let count = targets.len();
        for (target, col) in targets {
            self.stage(target, col, Value::Null)?;
        }
        self.editing = None;
        Ok(count)
    }

    // ---- rows ----

    /// Marks `rows` (grid rows) for deletion. When every selected model row is already marked,
    /// the marks are taken back instead. A selected new row is removed. Rows the last apply
    /// changed are skipped.
    ///
    /// Returns how many rows changed.
    ///
    /// # Errors
    ///
    /// A sentence for the user when nothing can be changed, or a row has no value in a key
    /// column. Nothing is changed then.
    pub fn delete_rows(&mut self, model: &GridModel, rows: &[usize]) -> Result<usize, String> {
        let staged = self.check_can_edit(model)?;
        let base = model.row_count();
        let applied = self.overlay.inserted.len();
        let mut rows: Vec<usize> = rows.to_vec();
        rows.sort_unstable();
        rows.dedup();
        let mut model_rows = Vec::new();
        let mut new_ids = Vec::new();
        let mut skipped = 0usize;
        for row in rows {
            if row < base {
                if self.overlay.updated.contains(&row) || self.overlay.deleted.contains(&row) {
                    skipped += 1;
                } else {
                    model_rows.push(row);
                }
            } else if row - base < applied {
                skipped += 1;
            } else if let Some((id, _)) = staged.inserts.get(row - base - applied) {
                new_ids.push(*id);
            }
        }
        if model_rows.is_empty() && new_ids.is_empty() {
            return Err(if skipped > 0 {
                APPLIED_ROW.to_owned()
            } else {
                "Select a row first.".to_owned()
            });
        }
        let undo =
            !model_rows.is_empty() && model_rows.iter().all(|&r| staged.is_deleted(ModelRow(r)));
        let mut snapshots = Vec::new();
        if !undo {
            for &row in &model_rows {
                if staged.is_deleted(ModelRow(row)) {
                    continue;
                }
                let original = model
                    .row(row)
                    .map(|r| r.to_row())
                    .ok_or_else(|| "There is no such row.".to_owned())?;
                if let Some(message) = Self::null_key_message(staged, &original) {
                    return Err(message);
                }
                snapshots.push((row, original));
            }
        }
        let Some(staged) = self.staged.as_mut() else {
            return Err(NO_SOURCE.to_owned());
        };
        let mut changed = new_ids.len();
        for id in new_ids {
            staged.remove_new_row(id);
        }
        if undo {
            for &row in &model_rows {
                staged.undelete_row(ModelRow(row));
            }
            changed += model_rows.len();
        } else {
            for (row, original) in snapshots {
                staged
                    .delete_row(ModelRow(row), &original)
                    .map_err(|e| Self::user_text(&e))?;
                changed += 1;
            }
        }
        self.editing = None;
        self.failed = None;
        Ok(changed)
    }

    /// Stages an empty new row at the end and returns its grid row.
    ///
    /// # Errors
    ///
    /// A sentence for the user when the grid is read-only, loading or applying.
    pub fn add_row(&mut self, model: &GridModel) -> Result<usize, String> {
        self.check_can_edit(model)?;
        let base = model.row_count() + self.overlay.inserted.len();
        let Some(staged) = self.staged.as_mut() else {
            return Err(NO_SOURCE.to_owned());
        };
        staged.add_row();
        self.failed = None;
        Ok(base + staged.inserts.len() - 1)
    }

    /// Takes back the pending changes in `cells`: a cell edit, a deletion mark, a value of a new
    /// row. Does nothing for cells without a pending change.
    pub fn revert(&mut self, model: &GridModel, cells: &[CellPos]) {
        let Some(staged) = self.staged.as_mut() else {
            return;
        };
        let base = model.row_count();
        let applied = self.overlay.inserted.len();
        for pos in cells {
            if pos.row < base {
                staged.undelete_row(ModelRow(pos.row));
                staged.revert_cell(ModelRow(pos.row), pos.col);
            } else if pos.row - base >= applied
                && let Some((id, _)) = staged.inserts.get(pos.row - base - applied)
            {
                let id = *id;
                let _ = staged.set_new_cell(id, pos.col, None);
            }
        }
        self.failed = None;
    }

    /// Drops every pending change, the open editor, the failure mark and the apply block.
    pub fn discard(&mut self) {
        if let Some(staged) = self.staged.as_mut() {
            staged.clear();
        }
        self.editing = None;
        self.failed = None;
        self.apply_block = None;
    }

    // ---- apply ----

    /// The plan for the pending changes.
    ///
    /// # Errors
    ///
    /// [`EditError::NothingStaged`], or [`EditError::Dml`] when a statement cannot be generated
    /// (a new row without a value).
    pub fn plan(&self) -> Result<ApplyPlan, EditError> {
        self.staged
            .as_ref()
            .ok_or(EditError::NothingStaged)
            .and_then(StagedEdits::plan)
    }

    /// An apply starts: `rows` is the row of each data statement, in order. Edits are refused
    /// until [`Self::finish_apply`].
    pub fn begin_apply(&mut self, rows: Vec<RowRef>) {
        self.applying = Some(rows);
        self.editing = None;
        self.failed = None;
    }

    /// The apply ended: shows the result, and returns the message for the user.
    ///
    /// * `Committed`: the pending changes are moved into the overlay and cleared.
    /// * `RolledBack`: the row of the failing statement is marked `Failed`; the edits are kept.
    /// * `NotStarted`: the edits are kept.
    /// * `CommitUnknown`: the edits are kept and Apply is blocked with [`COMMIT_UNKNOWN`] until
    ///   [`Self::discard`] (or a new grid).
    pub fn finish_apply(&mut self, outcome: &ApplyOutcome) -> String {
        let rows = self.applying.take().unwrap_or_default();
        match outcome {
            ApplyOutcome::Committed { applied } => {
                self.commit_overlay();
                self.failed = None;
                format!("Applied {applied} change(s).")
            }
            ApplyOutcome::NotStarted { message } => {
                format!(
                    "The apply did not start: {}. Your changes are kept.",
                    message.trim_end_matches('.')
                )
            }
            ApplyOutcome::RolledBack {
                at,
                failure,
                rollback_error,
            } => {
                self.failed = at.and_then(|i| rows.get(i).copied());
                let mut text = format!(
                    "Nothing was written: {}. {}",
                    failure_text(failure, *at),
                    rollback_note(rollback_error.as_deref())
                );
                text.push_str("Your changes are kept.");
                text
            }
            ApplyOutcome::CommitUnknown {
                failure,
                rollback_error,
            } => {
                self.apply_block = Some(COMMIT_UNKNOWN.to_owned());
                format!(
                    "{COMMIT_UNKNOWN} ({}). {}Your changes are kept.",
                    failure_text(failure, None),
                    rollback_note(rollback_error.as_deref())
                )
            }
        }
    }

    fn commit_overlay(&mut self) {
        let Some(staged) = self.staged.as_mut() else {
            return;
        };
        for (row, update) in &staged.updates {
            for (col, value) in &update.cells {
                self.overlay.cells.insert((row.0, *col), value.clone());
            }
            self.overlay.updated.insert(row.0);
        }
        for row in staged.deletes.keys() {
            self.overlay.deleted.insert(row.0);
        }
        for (_, cells) in &staged.inserts {
            self.overlay.inserted.push(cells.clone());
        }
        staged.clear();
    }

    // ---- display ----

    /// What the grid draws for the cell at grid row `row`, model column `col`.
    #[must_use]
    pub fn display_cell(&self, model: &GridModel, row: usize, col: usize) -> DisplayCell {
        let base = model.row_count();
        if row < base {
            return self.model_cell(model, row, col);
        }
        let index = row - base;
        let applied = self.overlay.inserted.len();
        if index < applied {
            return DisplayCell::of_new(self.overlay.inserted[index].get(col), false);
        }
        match self
            .staged
            .as_ref()
            .and_then(|s| s.inserts.get(index - applied))
        {
            Some((id, cells)) => {
                let mut shown = DisplayCell::of_new(cells.get(col), true);
                if self.failed == Some(RowRef::New(*id)) {
                    shown.mark = CellMark::Failed;
                }
                shown
            }
            None => DisplayCell::empty(),
        }
    }

    fn model_cell(&self, model: &GridModel, row: usize, col: usize) -> DisplayCell {
        let Some(cell) = model.row(row).and_then(|r| r.get(col)) else {
            return DisplayCell::empty();
        };
        let mut shown = DisplayCell {
            text: cell_text(&cell),
            mark: CellMark::None,
            null: cell.is_null(),
        };
        if self.overlay.is_empty() && self.pending() == 0 && self.failed.is_none() {
            return shown;
        }
        if self.overlay.deleted.contains(&row) {
            shown.mark = CellMark::DeletedApplied;
        } else if let Some(value) = self.overlay.cells.get(&(row, col)) {
            shown.set_value(value);
        }
        if let Some(staged) = &self.staged {
            if staged.is_deleted(ModelRow(row)) {
                shown.mark = CellMark::Deleted;
            } else if let Some(value) = staged.staged_value(ModelRow(row), col) {
                shown.set_value(value);
                shown.mark = CellMark::Changed;
            }
        }
        if self.failed == Some(RowRef::Model(ModelRow(row))) {
            shown.mark = CellMark::Failed;
        }
        shown
    }
}

impl DisplayCell {
    fn empty() -> Self {
        Self {
            text: String::new(),
            mark: CellMark::None,
            null: false,
        }
    }

    fn set_value(&mut self, value: &Value) {
        self.text = cell_text(value);
        self.null = value.is_null();
    }

    /// A cell of a new row: `None` was left to the server's default.
    fn of_new(cell: Option<&Option<Value>>, staged: bool) -> Self {
        match cell {
            Some(Some(value)) => Self {
                text: cell_text(value),
                mark: if staged {
                    CellMark::New
                } else {
                    CellMark::None
                },
                null: value.is_null(),
            },
            _ => Self {
                text: String::new(),
                mark: CellMark::Unset,
                null: false,
            },
        }
    }
}

fn failure_text(failure: &ApplyFailure, at: Option<usize>) -> String {
    let position = at.map_or_else(String::new, |i| format!("statement {} ", i + 1));
    match failure {
        ApplyFailure::RowCount { expected, actual } => match actual {
            Some(n) => format!(
                "{position}affected {n} row(s) instead of {expected}, so it was rolled back; the \
                 row was changed or removed since it was read, or it is not unique"
            ),
            None => format!("{position}did not report how many rows it affected"),
        },
        ApplyFailure::Driver(message) => format!("{position}failed: {message}"),
        ApplyFailure::Cancelled => "the apply was cancelled".to_owned(),
    }
}

fn rollback_note(rollback_error: Option<&str>) -> String {
    rollback_error.map_or_else(String::new, |e| {
        format!("The rollback also failed ({e}); the session may still be in a transaction. ")
    })
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::{ColumnInfo, ObjectPath, PrimaryKey};

    use super::*;
    use crate::grid::edit::testing::{model_of, row};

    fn object(kind: ObjectKind) -> DbObject {
        DbObject::new(kind, ObjectPath::root().in_schema("main"), "t")
    }

    fn keyed(extra: Vec<ColumnInfo>) -> Arc<Table> {
        let mut columns = vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("name", DataType::Text),
        ];
        columns.extend(extra);
        Arc::new(
            Table::new(object(ObjectKind::Table))
                .with_columns(columns)
                .with_primary_key(PrimaryKey::new(vec!["id".to_owned()])),
        )
    }

    fn keyless() -> Arc<Table> {
        Arc::new(Table::new(object(ObjectKind::Table)).with_columns(vec![
            ColumnInfo::new("id", DataType::Integer).with_nullable(false),
            ColumnInfo::new("name", DataType::Text),
        ]))
    }

    fn ctx_for(sql: &str, table: Option<Arc<Table>>) -> EditContext {
        EditContext {
            dialect: Some(Dialect::Sqlite),
            origin: EditOrigin::QueryResult(sql.to_owned()),
            table,
            relation_kind: None,
            resolved_schema: Some("main".to_owned()),
            production: false,
            apply_block: None,
        }
    }

    fn cols() -> Vec<Column> {
        vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
        ]
    }

    fn editor_with(table: Arc<Table>) -> GridEditor {
        let mut e = GridEditor::new();
        e.set_context(ctx_for("SELECT * FROM t", Some(table)), &cols());
        e
    }

    fn editor() -> GridEditor {
        let e = editor_with(keyed(vec![]));
        assert_eq!(e.state(), &EditState::Editable);
        e
    }

    fn model() -> GridModel {
        model_of(
            vec![
                row(1, Some("one")),
                row(2, Some("two")),
                row(3, Some("three")),
            ],
            IngestState::Done {
                rows_affected: None,
            },
        )
    }

    fn at(row: usize, col: usize) -> CellPos {
        CellPos { row, col }
    }

    fn edit_text_cell(e: &mut GridEditor, m: &GridModel, row: usize, col: usize, text: &str) {
        e.start_edit(m, at(row, col)).unwrap();
        e.commit_edit(m, EditorInput::Text(text.to_owned()))
            .unwrap();
    }

    fn shown(e: &GridEditor, m: &GridModel, row: usize, col: usize) -> (String, CellMark) {
        let d = e.display_cell(m, row, col);
        (d.text, d.mark)
    }

    #[test]
    fn state_follows_the_context() {
        let mut e = GridEditor::new();
        assert_eq!(
            e.state(),
            &EditState::Unavailable("This grid has no source table.".to_owned())
        );
        assert!(e.read_only_message().is_some());
        // The columns have not arrived.
        e.set_context(ctx_for("SELECT * FROM t", Some(keyed(vec![]))), &[]);
        assert_eq!(e.state(), &EditState::Loading);
        assert_eq!(e.read_only_message(), None);
        // They arrive later.
        e.set_columns(&cols());
        assert_eq!(e.state(), &EditState::Editable);
        // Rows still streaming.
        let (model, _sink) = crate::grid::grid_channel();
        assert_eq!(e.state_for(&model), EditState::Loading);
        assert!(e.start_edit(&model, at(0, 0)).is_err());
        // A view.
        let mut c = ctx_for("SELECT * FROM t", Some(keyed(vec![])));
        c.relation_kind = Some(ObjectKind::View);
        e.set_context(c, &cols());
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::NotATable {
                kind: ObjectKind::View
            })
        );
        assert!(e.read_only_message().unwrap().contains("view"));
        // The table is not loaded.
        e.set_context(ctx_for("SELECT * FROM t", None), &cols());
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::TableNotLoaded)
        );
        // The probe found no schema, or another one.
        let mut c = ctx_for("SELECT * FROM t", Some(keyed(vec![])));
        c.resolved_schema = None;
        e.set_context(c, &cols());
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::TableMismatch)
        );
        let mut c = ctx_for("SELECT * FROM t", Some(keyed(vec![])));
        c.resolved_schema = Some("temp".to_owned());
        e.set_context(c, &cols());
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::TableMismatch)
        );
        // A join.
        e.set_context(
            ctx_for("SELECT * FROM t JOIN u ON 1 = 1", Some(keyed(vec![]))),
            &cols(),
        );
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::NotSingleTable)
        );
    }

    #[test]
    fn keyless_opt_in_only_for_no_key_and_key_not_in_result() {
        let m = model();
        let mut e = editor_with(keyless());
        assert_eq!(e.state(), &EditState::ReadOnly(ReadOnlyReason::NoKey));
        assert!(e.start_edit(&m, at(0, 1)).is_err());
        assert!(e.allow_keyless(&cols()));
        assert_eq!(e.state(), &EditState::Editable);
        assert!(e.is_keyless());
        assert!(e.start_edit(&m, at(0, 1)).is_ok());

        // The key is not in the result.
        let mut e = GridEditor::new();
        e.set_context(
            ctx_for("SELECT name FROM t", Some(keyed(vec![]))),
            &[Column::new("name", DataType::Text)],
        );
        assert!(matches!(
            e.state(),
            EditState::ReadOnly(ReadOnlyReason::KeyNotInResult { .. })
        ));
        assert!(e.allow_keyless(&[Column::new("name", DataType::Text)]));

        // Reasons that never opt in.
        let mut e = GridEditor::new();
        e.set_context(
            ctx_for("SELECT * FROM t JOIN u ON 1 = 1", Some(keyless())),
            &cols(),
        );
        assert!(!e.allow_keyless(&cols()));
        assert!(matches!(
            e.state(),
            EditState::ReadOnly(ReadOnlyReason::NotSingleTable)
        ));
        let mut c = ctx_for("SELECT * FROM t", Some(keyless()));
        c.relation_kind = Some(ObjectKind::View);
        let mut e = GridEditor::new();
        e.set_context(c, &cols());
        assert!(!e.allow_keyless(&cols()));
        // Already editable: nothing to opt into.
        let mut e = editor();
        assert!(!e.allow_keyless(&cols()));
        assert!(!e.is_keyless());
    }

    #[test]
    fn edit_stages_marks_and_counts() {
        let m = model();
        let mut e = editor();
        let ed = e.start_edit(&m, at(1, 1)).unwrap();
        assert_eq!(ed.initial, EditorInput::Text("two".to_owned()));
        assert_eq!(ed.kind, EditorKind::Text);
        assert!(ed.nullable);
        assert!(e.editing().is_some());
        e.commit_edit(&m, EditorInput::Text("TWO".to_owned()))
            .unwrap();
        assert!(e.editing().is_none());
        assert_eq!(shown(&e, &m, 1, 1), ("TWO".to_owned(), CellMark::Changed));
        assert_eq!(shown(&e, &m, 1, 0), ("2".to_owned(), CellMark::None));
        assert_eq!(shown(&e, &m, 0, 1), ("one".to_owned(), CellMark::None));
        assert_eq!(e.pending(), 1);
        assert_eq!(e.status_text().as_deref(), Some("1 pending: 1 edited"));
        // The pending value is what the editor starts with next time.
        let ed = e.start_edit(&m, at(1, 1)).unwrap();
        assert_eq!(ed.initial, EditorInput::Text("TWO".to_owned()));
        // Back to the original: the change goes away.
        e.commit_edit(&m, EditorInput::Text("two".to_owned()))
            .unwrap();
        assert_eq!(e.pending(), 0);
        assert_eq!(e.status_text(), None);
        assert_eq!(shown(&e, &m, 1, 1), ("two".to_owned(), CellMark::None));
    }

    #[test]
    fn invalid_input_keeps_the_editor_open_and_escape_cancels() {
        let m = model();
        let mut e = editor();
        e.start_edit(&m, at(0, 0)).unwrap();
        let err = e
            .commit_edit(&m, EditorInput::Text("abc".to_owned()))
            .unwrap_err();
        assert!(!err.contains("abc"));
        assert!(e.editing().is_some());
        assert_eq!(e.pending(), 0);
        e.cancel_edit();
        assert!(e.editing().is_none());
        assert_eq!(e.pending(), 0);
        assert!(e.commit_edit(&m, EditorInput::Null).is_err());
        // A boolean toggle on a non-boolean column.
        e.start_edit(&m, at(0, 0)).unwrap();
        assert!(e.commit_edit(&m, EditorInput::Bool(true)).is_err());
    }

    #[test]
    fn set_null_respects_not_null_and_is_atomic() {
        let m = model();
        let mut e = editor();
        assert_eq!(e.set_null(&m, &[at(0, 1), at(1, 1)]), Ok(2));
        assert_eq!(e.pending(), 2);
        assert_eq!(shown(&e, &m, 0, 1), ("NULL".to_owned(), CellMark::Changed));
        assert!(e.display_cell(&m, 0, 1).null);
        // `id` is NOT NULL: nothing at all is staged.
        let mut e = editor();
        let err = e.set_null(&m, &[at(0, 1), at(1, 0)]).unwrap_err();
        assert_eq!(err, "Column id is NOT NULL.");
        assert_eq!(e.pending(), 0);
        e.start_edit(&m, at(0, 0)).unwrap();
        assert_eq!(
            e.commit_edit(&m, EditorInput::Null),
            Err("Column id is NOT NULL.".to_owned())
        );
        assert!(e.editing().is_some());
    }

    #[test]
    fn binary_cells_are_read_only() {
        let table = keyed(vec![ColumnInfo::new("blob", DataType::Binary)]);
        let columns = vec![
            Column::new("id", DataType::Integer),
            Column::new("name", DataType::Text),
            Column::new("blob", DataType::Binary),
        ];
        let mut e = GridEditor::new();
        e.set_context(ctx_for("SELECT * FROM t", Some(table)), &columns);
        assert_eq!(e.state(), &EditState::Editable);
        let m = model_of(
            vec![row(1, Some("a"))],
            IngestState::Done {
                rows_affected: None,
            },
        );
        // The model has two columns; the third is only checked by type.
        assert_eq!(
            e.start_edit(&m, at(0, 2)).unwrap_err(),
            "Binary cells are read-only."
        );
        assert!(e.set_null(&m, &[at(0, 2)]).is_err());
    }

    #[test]
    fn a_row_with_a_null_key_cannot_be_edited_or_deleted() {
        let m = model_of(
            vec![
                Row(vec![Value::Null, Value::Text("x".into())]),
                row(2, Some("b")),
            ],
            IngestState::Done {
                rows_affected: None,
            },
        );
        let mut e = editor();
        let err = e.start_edit(&m, at(0, 1)).unwrap_err();
        assert!(err.contains("key column id"), "{err}");
        assert!(e.delete_rows(&m, &[0]).is_err());
        assert_eq!(e.pending(), 0);
        assert!(e.start_edit(&m, at(1, 1)).is_ok());
    }

    #[test]
    fn delete_toggles_and_blocks_edits() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "X");
        assert_eq!(e.delete_rows(&m, &[0, 1]), Ok(2));
        assert_eq!(e.pending(), 2);
        // The cell edit of a deleted row is gone and the row is marked.
        assert_eq!(shown(&e, &m, 0, 1), ("one".to_owned(), CellMark::Deleted));
        assert_eq!(shown(&e, &m, 1, 0), ("2".to_owned(), CellMark::Deleted));
        assert_eq!(shown(&e, &m, 2, 0), ("3".to_owned(), CellMark::None));
        assert!(e.start_edit(&m, at(0, 1)).unwrap_err().contains("deletion"));
        // A selection with an undeleted row deletes that row.
        assert_eq!(e.delete_rows(&m, &[0, 1, 2]), Ok(1));
        assert_eq!(e.pending(), 3);
        // All deleted: it undeletes.
        assert_eq!(e.delete_rows(&m, &[0, 1, 2]), Ok(3));
        assert_eq!(e.pending(), 0);
        assert!(e.delete_rows(&m, &[]).is_err());
        assert!(e.delete_rows(&m, &[99]).is_err());
        assert_eq!(e.status_text(), None);
    }

    #[test]
    fn new_rows_are_staged_marked_edited_and_removed() {
        let m = model();
        let mut e = editor();
        let r = e.add_row(&m).unwrap();
        assert_eq!(r, 3);
        assert_eq!(e.extra_rows(), 1);
        assert_eq!(shown(&e, &m, 3, 0), (String::new(), CellMark::Unset));
        edit_text_cell(&mut e, &m, 3, 0, "10");
        edit_text_cell(&mut e, &m, 3, 1, "ten");
        assert_eq!(shown(&e, &m, 3, 0), ("10".to_owned(), CellMark::New));
        assert_eq!(shown(&e, &m, 3, 1), ("ten".to_owned(), CellMark::New));
        // NULL is a value, not "unset".
        e.set_null(&m, &[at(3, 1)]).unwrap();
        assert_eq!(shown(&e, &m, 3, 1), ("NULL".to_owned(), CellMark::New));
        assert_eq!(e.add_row(&m), Ok(4));
        edit_text_cell(&mut e, &m, 1, 1, "TWO");
        assert_eq!(
            e.status_text().as_deref(),
            Some("3 pending: 1 edited, 2 new")
        );
        // Delete one model row too.
        e.delete_rows(&m, &[2]).unwrap();
        assert_eq!(
            e.status_text().as_deref(),
            Some("4 pending: 1 edited, 1 deleted, 2 new")
        );
        // Deleting a new row removes it; the next one moves up.
        assert_eq!(e.delete_rows(&m, &[3]), Ok(1));
        assert_eq!(e.extra_rows(), 1);
        assert_eq!(shown(&e, &m, 3, 0), (String::new(), CellMark::Unset));
        assert_eq!(shown(&e, &m, 4, 0), (String::new(), CellMark::None));
        // Past the last row there is nothing.
        assert!(e.start_edit(&m, at(4, 0)).is_err());
    }

    #[test]
    fn status_text_example_of_the_spec() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "X");
        e.delete_rows(&m, &[1]).unwrap();
        e.add_row(&m).unwrap();
        assert_eq!(
            e.status_text().as_deref(),
            Some("3 pending: 1 edited, 1 deleted, 1 new")
        );
    }

    #[test]
    fn revert_and_discard() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "X");
        edit_text_cell(&mut e, &m, 1, 1, "Y");
        e.delete_rows(&m, &[2]).unwrap();
        let n = e.add_row(&m).unwrap();
        edit_text_cell(&mut e, &m, n, 1, "new");
        e.revert(&m, &[at(0, 1), at(2, 0), at(n, 1)]);
        assert_eq!(shown(&e, &m, 0, 1), ("one".to_owned(), CellMark::None));
        assert_eq!(shown(&e, &m, 1, 1), ("Y".to_owned(), CellMark::Changed));
        assert_eq!(shown(&e, &m, 2, 1), ("three".to_owned(), CellMark::None));
        assert_eq!(shown(&e, &m, n, 1), (String::new(), CellMark::Unset));
        assert_eq!(e.pending(), 2);
        e.discard();
        assert_eq!(e.pending(), 0);
        assert_eq!(e.extra_rows(), 0);
        assert_eq!(shown(&e, &m, 1, 1), ("two".to_owned(), CellMark::None));
        assert!(matches!(e.plan(), Err(EditError::NothingStaged)));
    }

    #[test]
    fn the_target_is_frozen_while_edits_are_staged() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "X");
        // The catalog entry vanishes while the edit is pending: the decision is kept.
        e.set_context(ctx_for("SELECT * FROM t", None), &cols());
        assert_eq!(e.state(), &EditState::Editable);
        assert_eq!(e.pending(), 1);
        assert!(e.plan().is_ok());
        // After a discard it is decided again.
        e.discard();
        e.set_context(ctx_for("SELECT * FROM t", None), &cols());
        assert_eq!(
            e.state(),
            &EditState::ReadOnly(ReadOnlyReason::TableNotLoaded)
        );
        assert_eq!(e.pending(), 0);
    }

    #[test]
    fn the_apply_block_is_the_contexts_until_a_commit_is_unknown() {
        let mut e = editor();
        assert_eq!(e.apply_blocked(), None);
        let mut c = ctx_for("SELECT * FROM t", Some(keyed(vec![])));
        c.apply_block = Some("A query is running.".to_owned());
        e.set_context(c, &cols());
        assert_eq!(e.apply_blocked(), Some("A query is running."));
    }

    fn applying(e: &mut GridEditor) -> Vec<RowRef> {
        let rows: Vec<RowRef> = e
            .plan()
            .unwrap()
            .statements()
            .iter()
            .map(|s| s.row)
            .collect();
        e.begin_apply(rows.clone());
        rows
    }

    #[test]
    fn nothing_is_edited_while_applying() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "X");
        applying(&mut e);
        assert!(e.is_applying());
        assert_eq!(
            e.start_edit(&m, at(1, 1)).unwrap_err(),
            "Changes are being applied."
        );
        assert!(e.add_row(&m).is_err());
        assert!(e.delete_rows(&m, &[1]).is_err());
        assert!(e.set_null(&m, &[at(1, 1)]).is_err());
        e.finish_apply(&ApplyOutcome::NotStarted {
            message: "x".to_owned(),
        });
        assert!(!e.is_applying());
        assert!(e.start_edit(&m, at(1, 1)).is_ok());
    }

    #[test]
    fn committed_moves_the_edits_into_the_overlay() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "ONE");
        e.delete_rows(&m, &[1]).unwrap();
        let n = e.add_row(&m).unwrap();
        edit_text_cell(&mut e, &m, n, 0, "9");
        applying(&mut e);
        let msg = e.finish_apply(&ApplyOutcome::Committed { applied: 3 });
        assert_eq!(msg, "Applied 3 change(s).");
        assert_eq!(e.pending(), 0);
        assert_eq!(e.status_text(), None);
        assert!(!e.is_applying());
        // The updated cell shows the new value, unmarked.
        assert_eq!(shown(&e, &m, 0, 1), ("ONE".to_owned(), CellMark::None));
        assert_eq!(
            shown(&e, &m, 1, 1),
            ("two".to_owned(), CellMark::DeletedApplied)
        );
        assert_eq!(shown(&e, &m, 2, 1), ("three".to_owned(), CellMark::None));
        // The inserted row sits below the model's rows; its unset cell stays unset.
        assert_eq!(e.extra_rows(), 1);
        assert_eq!(shown(&e, &m, 3, 0), ("9".to_owned(), CellMark::None));
        assert_eq!(shown(&e, &m, 3, 1), (String::new(), CellMark::Unset));
        // Applied rows are read-only until a reload.
        assert!(
            e.start_edit(&m, at(0, 1))
                .unwrap_err()
                .contains("last apply")
        );
        assert!(
            e.start_edit(&m, at(1, 1))
                .unwrap_err()
                .contains("last apply")
        );
        assert!(
            e.start_edit(&m, at(3, 0))
                .unwrap_err()
                .contains("last apply")
        );
        assert!(e.delete_rows(&m, &[0, 3]).is_err());
        // Other rows are still editable, and new rows go below the applied ones.
        assert!(e.start_edit(&m, at(2, 1)).is_ok());
        assert_eq!(e.add_row(&m), Ok(4));
        assert_eq!(shown(&e, &m, 4, 0), (String::new(), CellMark::Unset));
        // A mixed selection skips the applied rows.
        assert_eq!(e.delete_rows(&m, &[0, 2]), Ok(1));
    }

    #[test]
    fn rolled_back_marks_the_failing_row_and_keeps_the_edits() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "A");
        edit_text_cell(&mut e, &m, 2, 1, "C");
        let rows = applying(&mut e);
        assert_eq!(rows.len(), 2);
        let msg = e.finish_apply(&ApplyOutcome::RolledBack {
            at: Some(1),
            failure: ApplyFailure::RowCount {
                expected: 1,
                actual: Some(0),
            },
            rollback_error: None,
        });
        assert!(msg.ends_with("Your changes are kept."), "{msg}");
        assert!(msg.contains("statement 2"), "{msg}");
        assert_eq!(e.pending(), 2);
        assert_eq!(shown(&e, &m, 2, 1), ("C".to_owned(), CellMark::Failed));
        assert_eq!(shown(&e, &m, 0, 1), ("A".to_owned(), CellMark::Changed));
        assert_eq!(e.apply_blocked(), None);
        // Editing the row again clears the mark.
        edit_text_cell(&mut e, &m, 2, 1, "D");
        assert_eq!(shown(&e, &m, 2, 1), ("D".to_owned(), CellMark::Changed));
        // A cancel before COMMIT has no row.
        applying(&mut e);
        let msg = e.finish_apply(&ApplyOutcome::RolledBack {
            at: None,
            failure: ApplyFailure::Cancelled,
            rollback_error: Some("boom".to_owned()),
        });
        assert!(msg.ends_with("Your changes are kept."), "{msg}");
        assert!(
            msg.contains("cancelled") && msg.contains("rollback also failed"),
            "{msg}"
        );
        assert_eq!(shown(&e, &m, 2, 1), ("D".to_owned(), CellMark::Changed));
        assert_eq!(e.pending(), 2);
    }

    #[test]
    fn rolled_back_on_a_new_row_marks_it() {
        let m = model();
        let mut e = editor();
        let n = e.add_row(&m).unwrap();
        edit_text_cell(&mut e, &m, n, 0, "7");
        applying(&mut e);
        e.finish_apply(&ApplyOutcome::RolledBack {
            at: Some(0),
            failure: ApplyFailure::Driver("UNIQUE constraint failed".to_owned()),
            rollback_error: None,
        });
        assert_eq!(shown(&e, &m, n, 0), ("7".to_owned(), CellMark::Failed));
    }

    #[test]
    fn not_started_keeps_the_edits() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "A");
        applying(&mut e);
        let msg = e.finish_apply(&ApplyOutcome::NotStarted {
            message: "Could not connect: nope.".to_owned(),
        });
        assert!(msg.ends_with("Your changes are kept."), "{msg}");
        assert!(msg.contains("Could not connect: nope"), "{msg}");
        assert_eq!(e.pending(), 1);
        assert_eq!(e.apply_blocked(), None);
    }

    #[test]
    fn commit_unknown_blocks_apply_until_discard() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 0, 1, "A");
        applying(&mut e);
        let msg = e.finish_apply(&ApplyOutcome::CommitUnknown {
            failure: ApplyFailure::Driver("connection lost".to_owned()),
            rollback_error: None,
        });
        assert!(msg.starts_with(COMMIT_UNKNOWN), "{msg}");
        assert!(msg.ends_with("Your changes are kept."), "{msg}");
        assert_eq!(
            COMMIT_UNKNOWN,
            "The outcome is uncertain: re-read before retrying."
        );
        assert_eq!(e.apply_blocked(), Some(COMMIT_UNKNOWN));
        assert_eq!(e.pending(), 1);
        // Editing stays possible; a new context does not lift the block.
        edit_text_cell(&mut e, &m, 1, 1, "B");
        e.set_context(ctx_for("SELECT * FROM t", Some(keyed(vec![]))), &cols());
        assert_eq!(e.apply_blocked(), Some(COMMIT_UNKNOWN));
        e.discard();
        assert_eq!(e.apply_blocked(), None);
        assert_eq!(e.pending(), 0);
    }

    #[test]
    fn plan_has_the_statements_of_the_edits() {
        let m = model();
        let mut e = editor();
        edit_text_cell(&mut e, &m, 1, 1, "TWO");
        let plan = e.plan().unwrap();
        assert_eq!(plan.statements().len(), 1);
        assert_eq!(
            plan.statements()[0].query.sql,
            "UPDATE \"main\".\"t\" SET \"name\" = ?1 WHERE \"id\" = ?2"
        );
        assert_eq!(
            plan.statements()[0].query.params,
            vec![Value::Text("TWO".into()), Value::Int(2)]
        );
        let text = preview_text(&plan);
        assert!(text.contains(LAST_WRITER_WINS));
        assert!(text.contains("UPDATE"));
        // A new row without a value cannot be planned.
        let mut e = editor();
        e.add_row(&m).unwrap();
        assert!(matches!(e.plan(), Err(EditError::Dml { .. })));
    }

    #[test]
    fn debug_never_shows_values_or_sql() {
        let m = model_of(
            vec![row(424_242, Some("sekrit-cell"))],
            IngestState::Done {
                rows_affected: None,
            },
        );
        let mut e = GridEditor::new();
        let ctx = EditContext {
            origin: EditOrigin::QueryResult("SELECT * FROM t WHERE x = 'sekrit-sql'".to_owned()),
            ..ctx_for("", Some(keyed(vec![])))
        };
        let shown_ctx = format!("{ctx:?} {:?}", ctx.origin);
        e.set_context(ctx, &cols());
        e.start_edit(&m, at(0, 1)).unwrap();
        let editing = format!("{:?}", e.editing().unwrap());
        e.commit_edit(&m, EditorInput::Text("sekrit-new".to_owned()))
            .unwrap();
        let n = e.add_row(&m).unwrap();
        edit_text_cell(&mut e, &m, n, 1, "sekrit-ins");
        let cell = format!("{:?}", e.display_cell(&m, 0, 1));
        let input = format!("{:?}", EditorInput::Text("sekrit-in".to_owned()));
        let all = format!("{e:?} {shown_ctx} {editing} {cell} {input}");
        for secret in ["sekrit", "424242", "SELECT"] {
            assert!(!all.contains(secret), "{secret} in {all}");
        }
        assert!(all.contains("pending: 2"), "{all}");
        assert_eq!(format!("{:?}", EditorInput::Null), "Null");
    }
}
