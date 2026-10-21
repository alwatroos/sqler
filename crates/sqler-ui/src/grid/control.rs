// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridControls`: the column layout and the selection of one grid, driven by [`GridAction`]s.
//!
//! The view applies an action and then does what the returned [`Applied`] says: rebuild the
//! columns, scroll, fit a column or repaint. No GPUI types appear here.

use std::ops::Range;

use super::action::GridAction;
use super::cell::ColumnHeader;
use super::layout::{ColumnLayout, default_width};
use super::selection::{CellPos, DisplayMap, Motion, RowOrder, Selection};

/// What the view has to do after an action. Columns are display indices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// The column set, order, widths or pinning changed: rebuild the columns.
    pub relayout: bool,
    /// The selection changed: repaint.
    pub repaint: bool,
    /// Bring this display row fully into view: scroll the least that does it, and not at all
    /// when it is already fully visible (`ScrollStrategy::Nearest`). A row that is partly
    /// clipped at an edge is scrolled in.
    pub scroll_row: Option<usize>,
    /// Scroll this column into view.
    pub scroll_col: Option<usize>,
    /// Fit this column to its visible cells.
    pub autofit: Option<usize>,
}

/// The layout and the selection of one grid.
#[derive(Debug, Default)]
pub struct GridControls {
    layout: ColumnLayout,
    selection: Selection,
}

impl GridControls {
    /// Adapts to the result's columns. When their count changed the layout is rebuilt from the
    /// default widths and the selection is cleared; returns whether that happened.
    pub fn sync_columns(&mut self, headers: &[ColumnHeader]) -> bool {
        if self.layout.len() == headers.len() {
            return false;
        }
        self.layout = ColumnLayout::new(
            headers
                .iter()
                .map(|h| default_width(h.type_label))
                .collect(),
        );
        self.selection.clear();
        true
    }

    /// The column layout.
    #[must_use]
    pub fn layout(&self) -> &ColumnLayout {
        &self.layout
    }

    /// The selection.
    #[must_use]
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// The display column of the active cell.
    fn active_display_col(&self) -> Option<usize> {
        self.selection
            .active()
            .and_then(|c| self.layout.display_column(c.col))
    }

    /// Whether the motion `to` would change the active cell of a grid showing `rows`. Nothing
    /// is changed. Without an active cell a motion starts at the first cell, so it moves.
    #[must_use]
    pub fn motion_moves(&self, to: Motion, rows: &dyn RowOrder, visible: Range<usize>) -> bool {
        let map = DisplayMap {
            rows,
            columns: &self.layout,
        };
        let mut probe = self.selection.clone();
        probe.apply_motion(to, false, &map, visible.len().saturating_sub(1));
        probe.active() != self.selection.active()
    }

    /// Applies an action to a grid showing `rows`, of which the display rows `visible` are on
    /// screen. Rows in the result (`Applied::scroll_row`) are display rows.
    pub fn apply(&mut self, a: GridAction, rows: &dyn RowOrder, visible: Range<usize>) -> Applied {
        let mut out = Applied::default();
        match a {
            GridAction::ScrollToTop => out.scroll_row = (!rows.is_empty()).then_some(0),
            GridAction::ScrollToEnd => out.scroll_row = rows.len().checked_sub(1),
            GridAction::Move { to, extend } => {
                let before = self.active_display_col();
                let map = DisplayMap {
                    rows,
                    columns: &self.layout,
                };
                let page = visible.len().saturating_sub(1);
                out.repaint = self.selection.apply_motion(to, extend, &map, page);
                if out.repaint {
                    // The row the cell is shown at (a sort may have moved it).
                    out.scroll_row = self
                        .selection
                        .active()
                        .and_then(|active| rows.display_row(active.row));
                }
                let after = self.active_display_col();
                if after != before && after.is_some_and(|c| c >= self.layout.pinned_count()) {
                    out.scroll_col = after;
                }
            }
            GridAction::SelectAll => {
                self.selection.select_all();
                out.repaint = true;
            }
            GridAction::SelectRow => {
                self.selection.select_row();
                out.repaint = self.selection.active().is_some();
            }
            GridAction::SelectColumn => {
                self.selection.select_column();
                out.repaint = self.selection.active().is_some();
            }
            GridAction::PinColumn
            | GridAction::UnpinColumn
            | GridAction::MoveColumnLeft
            | GridAction::MoveColumnRight => {
                if let Some(CellPos { col, .. }) = self.selection.active() {
                    let changed = match a {
                        GridAction::PinColumn => self.layout.pin(col),
                        GridAction::UnpinColumn => self.layout.unpin(col),
                        GridAction::MoveColumnLeft => self.layout.move_left(col),
                        _ => self.layout.move_right(col),
                    };
                    out.relayout = changed;
                    out.repaint = changed;
                }
            }
            GridAction::AutofitColumn => out.autofit = self.active_display_col(),
            GridAction::ResetLayout => {
                self.layout.reset();
                out.relayout = true;
                out.repaint = true;
            }
            // Handled by the tab: it owns the model, the fetch and the sort and filter; the
            // view handles the detail pane.
            GridAction::FetchMore
            | GridAction::ToggleCellDetail
            | GridAction::SortAscending
            | GridAction::SortDescending
            | GridAction::ClearSort
            | GridAction::ClearFilter
            | GridAction::ToggleMatchCase
            // Editing acts on the editor and the workspace, not on the selection or layout.
            | GridAction::EditCell
            | GridAction::SetNull
            | GridAction::RevertSelection
            | GridAction::AddRow
            | GridAction::DeleteRows
            | GridAction::PreviewChanges
            | GridAction::ApplyChanges
            | GridAction::DiscardChanges
            | GridAction::EditWithoutKey => {}
        }
        out
    }

    /// A click on the cell at a display position. Returns whether it hit a cell.
    pub fn click_cell(
        &mut self,
        display_row: usize,
        display_col: usize,
        rows: &dyn RowOrder,
        extend: bool,
    ) -> bool {
        let map = DisplayMap {
            rows,
            columns: &self.layout,
        };
        match map.cell_at(display_row, display_col) {
            Some(cell) => {
                self.selection.click(cell, extend);
                true
            }
            None => false,
        }
    }

    /// A click on the header of the column at a display position. Returns whether it hit a column.
    pub fn click_header(&mut self, display_col: usize, rows: &dyn RowOrder, extend: bool) -> bool {
        let Some(model) = self.layout.model_column(display_col) else {
            return false;
        };
        let map = DisplayMap {
            rows,
            columns: &self.layout,
        };
        self.selection.click_column(model, &map, extend);
        true
    }

    /// Records the widths the table reports, in display order.
    pub fn set_display_widths(&mut self, widths: &[f32]) {
        self.layout.set_display_widths(widths);
    }

    /// Moves a column by drag; `false` if it would cross the pinned boundary or nothing moves.
    pub fn move_display(&mut self, from: usize, to: usize) -> bool {
        self.layout.move_display(from, to)
    }

    /// Whether the cell at a display position is selected.
    #[must_use]
    pub fn is_selected(&self, display_row: usize, display_col: usize, rows: &dyn RowOrder) -> bool {
        let map = DisplayMap {
            rows,
            columns: &self.layout,
        };
        self.selection.contains(display_row, display_col, &map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::selection::{IdentityRows, Motion, RowOrder};

    fn r(n: usize) -> IdentityRows {
        IdentityRows(n)
    }

    fn headers(n: usize) -> Vec<ColumnHeader> {
        (0..n)
            .map(|i| ColumnHeader {
                name: format!("c{i}"),
                type_label: if i % 2 == 0 { "integer" } else { "text" },
            })
            .collect()
    }

    fn controls(cols: usize) -> GridControls {
        let mut c = GridControls::default();
        assert!(c.sync_columns(&headers(cols)));
        c
    }

    fn mv(to: Motion) -> GridAction {
        GridAction::Move { to, extend: false }
    }

    #[test]
    fn motion_moves_tells_edges_without_changing_anything() {
        let mut c = controls(3);
        // No active cell: a motion starts at the first cell.
        assert!(c.motion_moves(Motion::Right, &r(5), 0..5));
        assert_eq!(c.selection().active(), None);
        assert!(c.click_cell(0, 2, &r(5), false));
        assert!(!c.motion_moves(Motion::Right, &r(5), 0..5));
        assert!(c.motion_moves(Motion::Left, &r(5), 0..5));
        assert!(!c.motion_moves(Motion::Right, &r(0), 0..0));
    }

    #[test]
    fn sync_columns_builds_defaults_and_resets_on_count_change() {
        let mut c = GridControls::default();
        assert!(!c.sync_columns(&[]), "nothing to do for no columns");
        assert!(c.sync_columns(&headers(3)));
        assert_eq!(c.layout().len(), 3);
        assert_eq!(c.layout().width(0), 110.0);
        assert_eq!(c.layout().width(1), 220.0);
        assert!(c.click_cell(1, 1, &r(5), false));
        c.apply(GridAction::PinColumn, &r(5), 0..5);
        assert!(!c.sync_columns(&headers(3)), "same count: kept");
        assert_eq!(c.layout().pinned_count(), 1);
        assert!(c.selection().active().is_some());
        assert!(c.sync_columns(&headers(4)));
        assert_eq!(c.layout().len(), 4);
        assert_eq!(c.layout().pinned_count(), 0);
        assert_eq!(c.selection().active(), None);
    }

    #[test]
    fn scroll_actions_target_the_ends() {
        let mut c = controls(2);
        let a = c.apply(GridAction::ScrollToTop, &r(100), 40..60);
        assert_eq!(
            a,
            Applied {
                scroll_row: Some(0),
                ..Applied::default()
            }
        );
        let a = c.apply(GridAction::ScrollToEnd, &r(100), 0..20);
        assert_eq!(
            a,
            Applied {
                scroll_row: Some(99),
                ..Applied::default()
            }
        );
        assert_eq!(
            c.apply(GridAction::ScrollToTop, &r(0), 0..0),
            Applied::default()
        );
        assert_eq!(
            c.apply(GridAction::ScrollToEnd, &r(0), 0..0),
            Applied::default()
        );
    }

    #[test]
    fn a_move_asks_for_the_active_row_to_be_brought_into_view() {
        let mut c = controls(3);
        c.click_cell(0, 0, &r(100), false);
        // The view scrolls the least that makes the row fully visible; the active row is
        // always named, so a partly clipped last row is scrolled in too.
        let a = c.apply(mv(Motion::Down), &r(100), 0..10);
        assert_eq!(
            a,
            Applied {
                repaint: true,
                scroll_row: Some(1),
                ..Applied::default()
            }
        );
        let a = c.apply(mv(Motion::PageDown), &r(100), 0..10);
        assert_eq!(c.selection().active(), Some(CellPos { row: 10, col: 0 }));
        assert_eq!(a.scroll_row, Some(10));
        let a = c.apply(mv(Motion::Up), &r(100), 20..30);
        assert_eq!(a.scroll_row, Some(9));
        // At the edge nothing changes and nothing is requested.
        c.apply(mv(Motion::First), &r(100), 0..10);
        let a = c.apply(mv(Motion::Up), &r(100), 0..10);
        assert_eq!(a, Applied::default());
    }

    #[test]
    fn extending_down_names_the_active_row() {
        let mut c = controls(2);
        c.click_cell(8, 0, &r(100), false);
        let a = c.apply(
            GridAction::Move {
                to: Motion::Down,
                extend: true,
            },
            &r(100),
            0..10,
        );
        assert_eq!(c.selection().active(), Some(CellPos { row: 9, col: 0 }));
        assert_eq!(a.scroll_row, Some(9));
    }

    /// Display row `d` shows model row `len - 1 - d`.
    struct Reversed(usize);

    impl RowOrder for Reversed {
        fn len(&self) -> usize {
            self.0
        }
        fn model_row(&self, d: usize) -> Option<usize> {
            (d < self.0).then(|| self.0 - 1 - d)
        }
        fn display_row(&self, m: usize) -> Option<usize> {
            (m < self.0).then(|| self.0 - 1 - m)
        }
    }

    #[test]
    fn scroll_rows_are_display_rows_under_another_order() {
        let mut c = controls(2);
        let rows = Reversed(100);
        // Display row 3 is model row 96.
        assert!(c.click_cell(3, 1, &rows, false));
        assert_eq!(c.selection().active(), Some(CellPos { row: 96, col: 1 }));
        let a = c.apply(mv(Motion::Down), &rows, 0..10);
        assert_eq!(c.selection().active(), Some(CellPos { row: 95, col: 1 }));
        assert_eq!(a.scroll_row, Some(4), "the display row, not the model row");
        assert_eq!(
            c.apply(GridAction::ScrollToEnd, &rows, 0..10).scroll_row,
            Some(99)
        );
        assert_eq!(
            c.apply(GridAction::ScrollToTop, &rows, 0..10).scroll_row,
            Some(0)
        );
        assert!(c.is_selected(4, 1, &rows) && !c.is_selected(95, 1, &rows));
        assert!(c.motion_moves(Motion::Down, &rows, 0..10));
        assert!(c.click_header(1, &rows, false));
        assert!(c.is_selected(0, 1, &rows) && c.is_selected(99, 1, &rows));
    }

    #[test]
    fn selection_follows_model_rows_through_a_real_sort_and_filter() {
        use crate::grid::order::testing;
        use crate::grid::{GridOrder, OrderProgress, SortDirection, SortKey, SortSpec};
        use sqler_driver_api::sqler_core::Value;

        let (mut model, sink) = testing::channel(2);
        let rows = [5, 3, 9, 1, 7]
            .into_iter()
            .map(|n| vec![Value::Int(n), Value::Text(format!("t{n}"))])
            .collect();
        testing::push(&mut model, &sink, rows);
        let mut order = GridOrder::default();
        let mut c = controls(2);
        // Model order: select the row holding 9 (model row 2).
        assert!(c.click_cell(2, 0, &order.rows(5), false));
        assert_eq!(c.selection().active(), Some(CellPos { row: 2, col: 0 }));
        // Sort descending: 9 moves to the top, and it is still the selected cell.
        order.set_sort(SortSpec {
            keys: vec![SortKey {
                column: 0,
                direction: SortDirection::Descending,
            }],
        });
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        let rows = order.rows(5);
        assert!(c.is_selected(0, 0, &rows));
        assert!(!c.is_selected(2, 0, &rows));
        // Moving down goes to 7 (model row 4), and the view is asked to show display row 1.
        let a = c.apply(mv(Motion::Down), &rows, 0..5);
        assert_eq!(c.selection().active(), Some(CellPos { row: 4, col: 0 }));
        assert_eq!(a.scroll_row, Some(1));
        // Filtering 7 out hides the selected row without losing it.
        order.set_filter(crate::grid::FilterSpec {
            quick: Some(crate::grid::QuickFilter {
                text: "t9".into(),
                column: Some(1),
            }),
            conditions: vec![],
        });
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        let rows = order.rows(5);
        assert_eq!(rows.len(), 1);
        assert!(!c.is_selected(0, 0, &rows));
        assert_eq!(c.selection().active(), Some(CellPos { row: 4, col: 0 }));
        assert!(c.click_cell(0, 1, &rows, false));
        assert_eq!(c.selection().active(), Some(CellPos { row: 2, col: 1 }));
    }

    #[test]
    fn move_scrolls_to_a_new_unpinned_column_only() {
        let mut c = controls(4);
        c.click_cell(0, 0, &r(10), false);
        c.apply(GridAction::PinColumn, &r(10), 0..10);
        let a = c.apply(mv(Motion::Right), &r(10), 0..10);
        assert_eq!(a.scroll_col, Some(1));
        let a = c.apply(mv(Motion::Left), &r(10), 0..10);
        assert_eq!(a.scroll_col, None, "the pinned column is always in view");
        let a = c.apply(mv(Motion::Up), &r(10), 0..10);
        assert_eq!(a.scroll_col, None, "same column");
        let a = c.apply(mv(Motion::RowEnd), &r(10), 0..10);
        assert_eq!(a.scroll_col, Some(3));
    }

    #[test]
    fn move_without_an_active_cell_starts_at_the_origin() {
        let mut c = controls(3);
        let a = c.apply(mv(Motion::Down), &r(10), 0..5);
        assert_eq!(c.selection().active(), Some(CellPos { row: 1, col: 0 }));
        assert!(a.repaint);
        assert_eq!(a.scroll_col, Some(0));
        let mut empty = controls(3);
        assert_eq!(
            empty.apply(mv(Motion::Down), &r(0), 0..0),
            Applied::default()
        );
    }

    #[test]
    fn extend_and_selection_actions_repaint() {
        let mut c = controls(3);
        assert_eq!(
            c.apply(GridAction::SelectRow, &r(5), 0..5),
            Applied::default()
        );
        assert_eq!(
            c.apply(GridAction::SelectColumn, &r(5), 0..5),
            Applied::default()
        );
        let a = c.apply(GridAction::SelectAll, &r(5), 0..5);
        assert_eq!(
            a,
            Applied {
                repaint: true,
                ..Applied::default()
            }
        );
        assert!(c.is_selected(4, 2, &r(5)));
        c.click_cell(1, 1, &r(5), false);
        assert!(!c.is_selected(0, 0, &r(5)));
        let a = c.apply(
            GridAction::Move {
                to: Motion::Down,
                extend: true,
            },
            &r(5),
            0..5,
        );
        assert!(a.repaint);
        assert!(
            c.is_selected(1, 1, &r(5)) && c.is_selected(2, 1, &r(5)) && !c.is_selected(3, 1, &r(5))
        );
        assert!(c.apply(GridAction::SelectRow, &r(5), 0..5).repaint);
        assert!(c.is_selected(2, 0, &r(5)) && c.is_selected(2, 2, &r(5)));
        assert!(c.apply(GridAction::SelectColumn, &r(5), 0..5).repaint);
        assert!(c.is_selected(4, 1, &r(5)) && !c.is_selected(4, 0, &r(5)));
    }

    #[test]
    fn layout_actions_act_on_the_active_column() {
        let mut c = controls(4);
        for a in [
            GridAction::PinColumn,
            GridAction::UnpinColumn,
            GridAction::MoveColumnLeft,
            GridAction::MoveColumnRight,
            GridAction::AutofitColumn,
        ] {
            assert_eq!(
                c.apply(a, &r(5), 0..5),
                Applied::default(),
                "{a:?} without a cell"
            );
        }
        c.click_cell(0, 2, &r(5), false);
        let relayout = Applied {
            relayout: true,
            repaint: true,
            ..Applied::default()
        };
        assert_eq!(c.apply(GridAction::PinColumn, &r(5), 0..5), relayout);
        assert!(c.layout().is_pinned(2));
        assert_eq!(
            c.apply(GridAction::PinColumn, &r(5), 0..5),
            Applied::default(),
            "already pinned"
        );
        assert_eq!(
            c.apply(GridAction::MoveColumnLeft, &r(5), 0..5),
            Applied::default(),
            "region edge"
        );
        assert_eq!(c.apply(GridAction::UnpinColumn, &r(5), 0..5), relayout);
        assert_eq!(c.apply(GridAction::MoveColumnRight, &r(5), 0..5), relayout);
        assert_eq!(c.layout().display_column(2), Some(1));
        assert_eq!(c.apply(GridAction::MoveColumnLeft, &r(5), 0..5), relayout);
        assert_eq!(c.layout().display_column(2), Some(0));
        assert_eq!(
            c.selection().active(),
            Some(CellPos { row: 0, col: 2 }),
            "model cell kept"
        );
    }

    #[test]
    fn autofit_names_the_display_column() {
        let mut c = controls(3);
        c.click_cell(0, 2, &r(5), false);
        c.apply(GridAction::PinColumn, &r(5), 0..5);
        let a = c.apply(GridAction::AutofitColumn, &r(5), 0..5);
        assert_eq!(
            a,
            Applied {
                autofit: Some(0),
                ..Applied::default()
            }
        );
    }

    #[test]
    fn reset_restores_the_layout_but_keeps_the_selection() {
        let mut c = controls(3);
        c.click_cell(0, 1, &r(5), false);
        c.apply(GridAction::PinColumn, &r(5), 0..5);
        c.set_display_widths(&[500.0, 100.0, 100.0]);
        assert!(c.move_display(1, 2));
        let a = c.apply(GridAction::ResetLayout, &r(5), 0..5);
        assert_eq!(
            a,
            Applied {
                relayout: true,
                repaint: true,
                ..Applied::default()
            }
        );
        assert_eq!(c.layout().pinned_count(), 0);
        assert_eq!(c.layout().display_column(1), Some(1));
        assert_eq!(c.layout().width(1), 220.0);
        assert!(c.selection().active().is_some());
    }

    #[test]
    fn clicks_select_and_extend() {
        let mut c = controls(4);
        assert!(!c.click_cell(0, 9, &r(5), false), "outside the columns");
        assert!(!c.click_cell(9, 0, &r(5), false), "outside the rows");
        assert!(c.click_cell(1, 1, &r(5), false));
        assert!(c.click_cell(3, 2, &r(5), true));
        assert!(
            c.is_selected(2, 1, &r(5)) && c.is_selected(3, 2, &r(5)) && !c.is_selected(4, 2, &r(5))
        );
        assert!(c.click_header(3, &r(5), false));
        assert!(
            c.is_selected(0, 3, &r(5)) && c.is_selected(4, 3, &r(5)) && !c.is_selected(0, 2, &r(5))
        );
        assert!(c.click_header(1, &r(5), true));
        assert!(c.is_selected(2, 2, &r(5)));
        assert!(!c.click_header(9, &r(5), false));
    }

    #[test]
    fn clicks_follow_the_display_order() {
        let mut c = controls(3);
        assert!(c.move_display(2, 0)); // display: model 2, 0, 1
        assert!(c.click_cell(0, 0, &r(5), false));
        assert_eq!(c.selection().active(), Some(CellPos { row: 0, col: 2 }));
        assert!(c.click_header(1, &r(5), false));
        assert_eq!(c.selection().active().map(|a| a.col), Some(0));
        assert!(!c.move_display(1, 1));
    }

    #[test]
    fn widths_from_the_table_are_clamped() {
        let mut c = controls(2);
        c.set_display_widths(&[10.0, 300.0]);
        assert_eq!(c.layout().width(0), 40.0);
        assert_eq!(c.layout().width(1), 300.0);
    }
}
