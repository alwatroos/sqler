// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The pending changes of one result.

use std::collections::BTreeMap;

use sqler_driver_api::sqler_core::{Row, Value};
use sqler_sql::DmlError;

use super::editability::{EditTarget, RowIdentity};
use super::plan::ApplyPlan;

/// A row's position in the `GridModel`; sort and filter only permute the view over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelRow(pub usize);

/// The id of a row that is staged for insertion; ids grow in creation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NewRowId(pub u32);

/// A row that has a staged change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RowRef {
    /// A row of the result.
    Model(ModelRow),
    /// A row staged for insertion.
    New(NewRowId),
}

/// What a planned statement does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangeKind {
    /// `DELETE`.
    Delete,
    /// `UPDATE`.
    Update,
    /// `INSERT`.
    Insert,
}

/// Why a change could not be staged or planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EditError {
    /// The row snapshot does not have the width of the result.
    #[error("row has {actual} values, the result has {expected} columns")]
    RowWidth {
        /// The result width.
        expected: usize,
        /// The snapshot width.
        actual: usize,
    },
    /// The column index is past the last result column.
    #[error("column {0} is out of range")]
    ColumnOutOfRange(usize),
    /// The row is staged for deletion.
    #[error("row {} is staged for deletion", .0.0)]
    RowDeleted(ModelRow),
    /// No such staged new row.
    #[error("unknown new row {}", .0.0)]
    UnknownNewRow(NewRowId),
    /// The row's original key value is `NULL`, so it cannot be found again.
    #[error("row {} has a NULL key value in {column:?}", .row.0)]
    NullKey {
        /// The row.
        row: ModelRow,
        /// The key column.
        column: String,
    },
    /// Nothing is staged.
    #[error("nothing is staged")]
    NothingStaged,
    /// A statement could not be generated.
    #[error("cannot generate the statement: {error}")]
    Dml {
        /// The row it was for.
        row: RowRef,
        /// Why.
        error: DmlError,
    },
}

#[derive(Clone)]
pub(super) struct RowUpdate {
    pub(super) original: Row,
    pub(super) cells: BTreeMap<usize, Value>,
}

impl std::fmt::Debug for RowUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RowUpdate")
            .field("columns", &self.original.len())
            .field("changed", &self.cells.len())
            .finish()
    }
}

/// The staged edits of one editable result.
///
/// It can only be built from an [`EditTarget`], so a result that did not pass
/// `check_editable` cannot have staged changes. It never writes anything; [`Self::plan`] lists
/// the statements and `apply` runs them.
///
/// `Debug` prints counts only: the staged values and the snapshots of the rows are user data.
#[derive(Clone)]
pub struct StagedEdits {
    target: EditTarget,
    pub(super) updates: BTreeMap<ModelRow, RowUpdate>,
    pub(super) deletes: BTreeMap<ModelRow, Row>,
    pub(super) inserts: Vec<(NewRowId, Vec<Option<Value>>)>,
    next_id: u32,
}

impl std::fmt::Debug for StagedEdits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedEdits")
            .field("dialect", &self.target.dialect())
            .field("updated_rows", &self.updates.len())
            .field("deleted_rows", &self.deletes.len())
            .field("new_rows", &self.inserts.len())
            .finish_non_exhaustive()
    }
}

impl StagedEdits {
    /// No changes yet, for `target`.
    #[must_use]
    pub fn new(target: EditTarget) -> Self {
        Self {
            target,
            updates: BTreeMap::new(),
            deletes: BTreeMap::new(),
            inserts: Vec::new(),
            next_id: 0,
        }
    }

    /// The target these edits are for.
    #[must_use]
    pub fn target(&self) -> &EditTarget {
        &self.target
    }

    fn check_original(&self, row: ModelRow, original: &Row) -> Result<(), EditError> {
        let expected = self.target.width();
        if original.len() != expected {
            return Err(EditError::RowWidth {
                expected,
                actual: original.len(),
            });
        }
        if let RowIdentity::Key { columns } = self.target.identity() {
            for &c in columns {
                if matches!(original.get(c), Some(Value::Null) | None) {
                    return Err(EditError::NullKey {
                        row,
                        column: self.target.table_column(c).to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Stages `value` for column `col` of `row`, whose current values are `original`.
    ///
    /// The first change of a row keeps `original` as the snapshot the row is later found by.
    /// A value equal to the original removes the change.
    ///
    /// # Errors
    ///
    /// [`EditError::RowWidth`], [`EditError::ColumnOutOfRange`], [`EditError::RowDeleted`] and
    /// [`EditError::NullKey`].
    pub fn set_cell(
        &mut self,
        row: ModelRow,
        original: &Row,
        col: usize,
        value: Value,
    ) -> Result<(), EditError> {
        if self.deletes.contains_key(&row) {
            return Err(EditError::RowDeleted(row));
        }
        if col >= self.target.width() {
            return Err(EditError::ColumnOutOfRange(col));
        }
        self.check_original(row, original)?;
        let entry = self.updates.entry(row).or_insert_with(|| RowUpdate {
            original: original.clone(),
            cells: BTreeMap::new(),
        });
        if entry.original.get(col) == Some(&value) {
            entry.cells.remove(&col);
        } else {
            entry.cells.insert(col, value);
        }
        if entry.cells.is_empty() {
            self.updates.remove(&row);
        }
        Ok(())
    }

    /// Drops the staged change of one cell.
    pub fn revert_cell(&mut self, row: ModelRow, col: usize) {
        if let Some(entry) = self.updates.get_mut(&row) {
            entry.cells.remove(&col);
            if entry.cells.is_empty() {
                self.updates.remove(&row);
            }
        }
    }

    /// Stages `row` for deletion and drops its cell changes.
    ///
    /// # Errors
    ///
    /// [`EditError::RowWidth`] and [`EditError::NullKey`].
    pub fn delete_row(&mut self, row: ModelRow, original: &Row) -> Result<(), EditError> {
        self.check_original(row, original)?;
        self.updates.remove(&row);
        self.deletes.insert(row, original.clone());
        Ok(())
    }

    /// Takes the deletion of `row` back. Its cell changes are gone.
    pub fn undelete_row(&mut self, row: ModelRow) {
        self.deletes.remove(&row);
    }

    /// Stages an empty new row and returns its id.
    pub fn add_row(&mut self) -> NewRowId {
        let id = NewRowId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.inserts.push((id, vec![None; self.target.width()]));
        id
    }

    /// Sets (`Some`) or clears (`None`) the value of a new row's cell. Only set cells are inserted.
    ///
    /// # Errors
    ///
    /// [`EditError::UnknownNewRow`] and [`EditError::ColumnOutOfRange`].
    pub fn set_new_cell(
        &mut self,
        id: NewRowId,
        col: usize,
        value: Option<Value>,
    ) -> Result<(), EditError> {
        let Some((_, cells)) = self.inserts.iter_mut().find(|(i, _)| *i == id) else {
            return Err(EditError::UnknownNewRow(id));
        };
        let Some(slot) = cells.get_mut(col) else {
            return Err(EditError::ColumnOutOfRange(col));
        };
        *slot = value;
        Ok(())
    }

    /// Drops a staged new row.
    pub fn remove_new_row(&mut self, id: NewRowId) {
        self.inserts.retain(|(i, _)| *i != id);
    }

    /// The staged value of a cell, if it has one.
    #[must_use]
    pub fn staged_value(&self, row: ModelRow, col: usize) -> Option<&Value> {
        self.updates.get(&row)?.cells.get(&col)
    }

    /// Whether `row` is staged for deletion.
    #[must_use]
    pub fn is_deleted(&self, row: ModelRow) -> bool {
        self.deletes.contains_key(&row)
    }

    /// The staged new rows in creation order; `None` is a cell the user did not fill.
    pub fn new_rows(&self) -> impl Iterator<Item = (NewRowId, &[Option<Value>])> {
        self.inserts
            .iter()
            .map(|(id, cells)| (*id, cells.as_slice()))
    }

    /// The number of statements the changes produce: changed rows, deleted rows and new rows.
    #[must_use]
    pub fn change_count(&self) -> usize {
        self.updates.len() + self.deletes.len() + self.inserts.len()
    }

    /// Whether nothing is staged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.change_count() == 0
    }

    /// Drops every staged change. Call it after a commit and when the result is re-run.
    pub fn clear(&mut self) {
        self.updates.clear();
        self.deletes.clear();
        self.inserts.clear();
    }

    /// The complete list of statements that applies the changes.
    ///
    /// # Errors
    ///
    /// [`EditError::NothingStaged`], or [`EditError::Dml`] when a statement cannot be generated
    /// (such as a new row without a value).
    pub fn plan(&self) -> Result<ApplyPlan, EditError> {
        ApplyPlan::build(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::edit::testing::{row, target};

    #[test]
    fn set_cell_stages_and_equal_value_reverts() {
        let mut s = StagedEdits::new(target(false));
        let r = row(1, Some("a"));
        s.set_cell(ModelRow(0), &r, 1, Value::Text("b".into()))
            .unwrap();
        assert_eq!(
            s.staged_value(ModelRow(0), 1),
            Some(&Value::Text("b".into()))
        );
        assert_eq!(s.change_count(), 1);
        s.set_cell(ModelRow(0), &r, 1, Value::Text("a".into()))
            .unwrap();
        assert!(s.is_empty());
        assert_eq!(s.staged_value(ModelRow(0), 1), None);
    }

    #[test]
    fn revert_cell_drops_row_entry_when_empty() {
        let mut s = StagedEdits::new(target(false));
        let r = row(1, Some("a"));
        s.set_cell(ModelRow(3), &r, 1, Value::Null).unwrap();
        s.set_cell(ModelRow(3), &r, 0, Value::Int(9)).unwrap();
        s.revert_cell(ModelRow(3), 1);
        assert_eq!(s.change_count(), 1);
        s.revert_cell(ModelRow(3), 0);
        assert!(s.is_empty());
        s.revert_cell(ModelRow(3), 0);
    }

    #[test]
    fn delete_wins_over_cell_changes_and_blocks_edits() {
        let mut s = StagedEdits::new(target(false));
        let r = row(1, Some("a"));
        s.set_cell(ModelRow(0), &r, 1, Value::Null).unwrap();
        s.delete_row(ModelRow(0), &r).unwrap();
        assert!(s.is_deleted(ModelRow(0)));
        assert_eq!(s.staged_value(ModelRow(0), 1), None);
        assert_eq!(s.change_count(), 1);
        assert_eq!(
            s.set_cell(ModelRow(0), &r, 1, Value::Null),
            Err(EditError::RowDeleted(ModelRow(0)))
        );
        s.undelete_row(ModelRow(0));
        assert!(s.is_empty());
        s.set_cell(ModelRow(0), &r, 1, Value::Null).unwrap();
    }

    #[test]
    fn width_range_and_null_key_errors() {
        let mut s = StagedEdits::new(target(false));
        let short = Row(vec![Value::Int(1)]);
        assert_eq!(
            s.set_cell(ModelRow(0), &short, 0, Value::Int(2)),
            Err(EditError::RowWidth {
                expected: 2,
                actual: 1
            })
        );
        let r = row(1, None);
        assert_eq!(
            s.set_cell(ModelRow(0), &r, 5, Value::Null),
            Err(EditError::ColumnOutOfRange(5))
        );
        let null_key = Row(vec![Value::Null, Value::Null]);
        assert_eq!(
            s.set_cell(ModelRow(0), &null_key, 1, Value::Int(1)),
            Err(EditError::NullKey {
                row: ModelRow(0),
                column: "id".to_owned()
            })
        );
        assert!(matches!(
            s.delete_row(ModelRow(0), &null_key),
            Err(EditError::NullKey { .. })
        ));
        assert!(s.is_empty());
        // Keyless matching can handle NULLs (IS NULL).
        let mut k = StagedEdits::new(target(true));
        k.set_cell(ModelRow(0), &null_key, 1, Value::Int(1))
            .unwrap();
    }

    #[test]
    fn new_rows() {
        let mut s = StagedEdits::new(target(false));
        let a = s.add_row();
        let b = s.add_row();
        assert!(a < b);
        s.set_new_cell(b, 1, Some(Value::Text("x".into()))).unwrap();
        s.set_new_cell(a, 0, Some(Value::Int(1))).unwrap();
        s.set_new_cell(a, 0, None).unwrap();
        assert_eq!(
            s.set_new_cell(a, 7, None),
            Err(EditError::ColumnOutOfRange(7))
        );
        assert_eq!(
            s.set_new_cell(NewRowId(99), 0, None),
            Err(EditError::UnknownNewRow(NewRowId(99)))
        );
        let rows: Vec<_> = s.new_rows().collect();
        assert_eq!(rows[0].0, a);
        assert_eq!(rows[1].1, &[None, Some(Value::Text("x".into()))]);
        s.remove_new_row(a);
        assert_eq!(s.change_count(), 1);
        let c = s.add_row();
        assert!(c > b, "ids are not reused");
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn debug_never_shows_values() {
        let mut s = StagedEdits::new(target(false));
        let r = row(424_242, Some("sekrit-old"));
        s.set_cell(ModelRow(0), &r, 1, Value::Text("sekrit-new".into()))
            .unwrap();
        s.delete_row(ModelRow(1), &row(515_151, Some("sekrit-del")))
            .unwrap();
        let id = s.add_row();
        s.set_new_cell(id, 1, Some(Value::Text("sekrit-ins".into())))
            .unwrap();
        let shown = format!("{s:?} {:?}", s.updates.values().collect::<Vec<_>>());
        for secret in ["sekrit", "424242", "515151"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(shown.contains("updated_rows: 1"), "{shown}");
        assert!(shown.contains("new_rows: 1"), "{shown}");
    }

    #[test]
    fn plan_of_nothing_is_refused() {
        let s = StagedEdits::new(target(false));
        assert_eq!(s.plan().unwrap_err(), EditError::NothingStaged);
    }
}
