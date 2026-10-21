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
use super::selection::{CellPos, DisplayMap, IdentityRows, Motion, Selection, scroll_row};

/// What the view has to do after an action. Columns are display indices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// The column set, order, widths or pinning changed: rebuild the columns.
    pub relayout: bool,
    /// The selection changed: repaint.
    pub repaint: bool,
    /// Scroll this row into view.
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

    /// Whether the motion `to` would change the active cell of a grid of `rows` rows. Nothing
    /// is changed. Without an active cell a motion starts at the first cell, so it moves.
    #[must_use]
    pub fn motion_moves(&self, to: Motion, rows: usize, visible: Range<usize>) -> bool {
        let order = IdentityRows(rows);
        let map = DisplayMap {
            rows: &order,
            columns: &self.layout,
        };
        let mut probe = self.selection.clone();
        probe.apply_motion(to, false, &map, visible.len().saturating_sub(1));
        probe.active() != self.selection.active()
    }

    /// Applies an action to a grid of `rows` rows of which `visible` are on screen.
    pub fn apply(&mut self, a: GridAction, rows: usize, visible: Range<usize>) -> Applied {
        let mut out = Applied::default();
        match a {
            GridAction::ScrollToTop => out.scroll_row = (rows > 0).then_some(0),
            GridAction::ScrollToEnd => out.scroll_row = rows.checked_sub(1),
            GridAction::Move { to, extend } => {
                let before = self.active_display_col();
                let order = IdentityRows(rows);
                let map = DisplayMap {
                    rows: &order,
                    columns: &self.layout,
                };
                let page = visible.len().saturating_sub(1);
                out.repaint = self.selection.apply_motion(to, extend, &map, page);
                if let Some(active) = self.selection.active() {
                    out.scroll_row = scroll_row(active.row, visible);
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
            // Handled by the tab (it owns the model and the fetch) and the view (the pane).
            GridAction::FetchMore | GridAction::ToggleCellDetail => {}
        }
        out
    }

    /// A click on the cell at a display position. Returns whether it hit a cell.
    pub fn click_cell(
        &mut self,
        display_row: usize,
        display_col: usize,
        rows: usize,
        extend: bool,
    ) -> bool {
        let order = IdentityRows(rows);
        let map = DisplayMap {
            rows: &order,
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
    pub fn click_header(&mut self, display_col: usize, rows: usize, extend: bool) -> bool {
        let Some(model) = self.layout.model_column(display_col) else {
            return false;
        };
        let order = IdentityRows(rows);
        let map = DisplayMap {
            rows: &order,
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
    pub fn is_selected(&self, display_row: usize, display_col: usize, rows: usize) -> bool {
        let order = IdentityRows(rows);
        let map = DisplayMap {
            rows: &order,
            columns: &self.layout,
        };
        self.selection.contains(display_row, display_col, &map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::selection::Motion;

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
        assert!(c.motion_moves(Motion::Right, 5, 0..5));
        assert_eq!(c.selection().active(), None);
        assert!(c.click_cell(0, 2, 5, false));
        assert!(!c.motion_moves(Motion::Right, 5, 0..5));
        assert!(c.motion_moves(Motion::Left, 5, 0..5));
        assert!(!c.motion_moves(Motion::Right, 0, 0..0));
    }

    #[test]
    fn sync_columns_builds_defaults_and_resets_on_count_change() {
        let mut c = GridControls::default();
        assert!(!c.sync_columns(&[]), "nothing to do for no columns");
        assert!(c.sync_columns(&headers(3)));
        assert_eq!(c.layout().len(), 3);
        assert_eq!(c.layout().width(0), 110.0);
        assert_eq!(c.layout().width(1), 220.0);
        assert!(c.click_cell(1, 1, 5, false));
        c.apply(GridAction::PinColumn, 5, 0..5);
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
        let a = c.apply(GridAction::ScrollToTop, 100, 40..60);
        assert_eq!(
            a,
            Applied {
                scroll_row: Some(0),
                ..Applied::default()
            }
        );
        let a = c.apply(GridAction::ScrollToEnd, 100, 0..20);
        assert_eq!(
            a,
            Applied {
                scroll_row: Some(99),
                ..Applied::default()
            }
        );
        assert_eq!(
            c.apply(GridAction::ScrollToTop, 0, 0..0),
            Applied::default()
        );
        assert_eq!(
            c.apply(GridAction::ScrollToEnd, 0, 0..0),
            Applied::default()
        );
    }

    #[test]
    fn move_repaints_and_scrolls_to_the_active_row() {
        let mut c = controls(3);
        c.click_cell(0, 0, 100, false);
        // Visible 0..10: moving down within view only repaints.
        let a = c.apply(mv(Motion::Down), 100, 0..10);
        assert_eq!(
            a,
            Applied {
                repaint: true,
                ..Applied::default()
            }
        );
        // Page down from row 1: page = 9, lands on row 10, just below the view.
        let a = c.apply(mv(Motion::PageDown), 100, 0..10);
        assert_eq!(c.selection().active(), Some(CellPos { row: 10, col: 0 }));
        assert_eq!(a.scroll_row, Some(1));
        assert!(a.repaint);
        // Up above the view scrolls to the row itself.
        let a = c.apply(mv(Motion::Up), 100, 20..30);
        assert_eq!(a.scroll_row, Some(9));
        // At the edge nothing changes and nothing is repainted.
        c.apply(mv(Motion::First), 100, 0..10);
        let a = c.apply(mv(Motion::Up), 100, 0..10);
        assert_eq!(a, Applied::default());
    }

    #[test]
    fn down_scrolls_by_one_only_past_the_last_visible_row() {
        let mut c = controls(2);
        c.click_cell(8, 0, 100, false);
        // Row 9 is the last visible one: no scroll.
        let a = c.apply(mv(Motion::Down), 100, 0..10);
        assert_eq!(c.selection().active(), Some(CellPos { row: 9, col: 0 }));
        assert_eq!(a.scroll_row, None);
        // Row 10 is just below the view: the first row moves by one.
        let a = c.apply(mv(Motion::Down), 100, 0..10);
        assert_eq!(c.selection().active(), Some(CellPos { row: 10, col: 0 }));
        assert_eq!(a.scroll_row, Some(1));
        // Extending down behaves the same.
        let a = c.apply(
            GridAction::Move {
                to: Motion::Down,
                extend: true,
            },
            100,
            1..11,
        );
        assert_eq!(a.scroll_row, Some(2));
    }

    #[test]
    fn move_scrolls_to_a_new_unpinned_column_only() {
        let mut c = controls(4);
        c.click_cell(0, 0, 10, false);
        c.apply(GridAction::PinColumn, 10, 0..10);
        let a = c.apply(mv(Motion::Right), 10, 0..10);
        assert_eq!(a.scroll_col, Some(1));
        let a = c.apply(mv(Motion::Left), 10, 0..10);
        assert_eq!(a.scroll_col, None, "the pinned column is always in view");
        let a = c.apply(mv(Motion::Up), 10, 0..10);
        assert_eq!(a.scroll_col, None, "same column");
        let a = c.apply(mv(Motion::RowEnd), 10, 0..10);
        assert_eq!(a.scroll_col, Some(3));
    }

    #[test]
    fn move_without_an_active_cell_starts_at_the_origin() {
        let mut c = controls(3);
        let a = c.apply(mv(Motion::Down), 10, 0..5);
        assert_eq!(c.selection().active(), Some(CellPos { row: 1, col: 0 }));
        assert!(a.repaint);
        assert_eq!(a.scroll_col, Some(0));
        let mut empty = controls(3);
        assert_eq!(empty.apply(mv(Motion::Down), 0, 0..0), Applied::default());
    }

    #[test]
    fn extend_and_selection_actions_repaint() {
        let mut c = controls(3);
        assert_eq!(c.apply(GridAction::SelectRow, 5, 0..5), Applied::default());
        assert_eq!(
            c.apply(GridAction::SelectColumn, 5, 0..5),
            Applied::default()
        );
        let a = c.apply(GridAction::SelectAll, 5, 0..5);
        assert_eq!(
            a,
            Applied {
                repaint: true,
                ..Applied::default()
            }
        );
        assert!(c.is_selected(4, 2, 5));
        c.click_cell(1, 1, 5, false);
        assert!(!c.is_selected(0, 0, 5));
        let a = c.apply(
            GridAction::Move {
                to: Motion::Down,
                extend: true,
            },
            5,
            0..5,
        );
        assert!(a.repaint);
        assert!(c.is_selected(1, 1, 5) && c.is_selected(2, 1, 5) && !c.is_selected(3, 1, 5));
        assert!(c.apply(GridAction::SelectRow, 5, 0..5).repaint);
        assert!(c.is_selected(2, 0, 5) && c.is_selected(2, 2, 5));
        assert!(c.apply(GridAction::SelectColumn, 5, 0..5).repaint);
        assert!(c.is_selected(4, 1, 5) && !c.is_selected(4, 0, 5));
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
                c.apply(a, 5, 0..5),
                Applied::default(),
                "{a:?} without a cell"
            );
        }
        c.click_cell(0, 2, 5, false);
        let relayout = Applied {
            relayout: true,
            repaint: true,
            ..Applied::default()
        };
        assert_eq!(c.apply(GridAction::PinColumn, 5, 0..5), relayout);
        assert!(c.layout().is_pinned(2));
        assert_eq!(
            c.apply(GridAction::PinColumn, 5, 0..5),
            Applied::default(),
            "already pinned"
        );
        assert_eq!(
            c.apply(GridAction::MoveColumnLeft, 5, 0..5),
            Applied::default(),
            "region edge"
        );
        assert_eq!(c.apply(GridAction::UnpinColumn, 5, 0..5), relayout);
        assert_eq!(c.apply(GridAction::MoveColumnRight, 5, 0..5), relayout);
        assert_eq!(c.layout().display_column(2), Some(1));
        assert_eq!(c.apply(GridAction::MoveColumnLeft, 5, 0..5), relayout);
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
        c.click_cell(0, 2, 5, false);
        c.apply(GridAction::PinColumn, 5, 0..5);
        let a = c.apply(GridAction::AutofitColumn, 5, 0..5);
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
        c.click_cell(0, 1, 5, false);
        c.apply(GridAction::PinColumn, 5, 0..5);
        c.set_display_widths(&[500.0, 100.0, 100.0]);
        assert!(c.move_display(1, 2));
        let a = c.apply(GridAction::ResetLayout, 5, 0..5);
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
        assert!(!c.click_cell(0, 9, 5, false), "outside the columns");
        assert!(!c.click_cell(9, 0, 5, false), "outside the rows");
        assert!(c.click_cell(1, 1, 5, false));
        assert!(c.click_cell(3, 2, 5, true));
        assert!(c.is_selected(2, 1, 5) && c.is_selected(3, 2, 5) && !c.is_selected(4, 2, 5));
        assert!(c.click_header(3, 5, false));
        assert!(c.is_selected(0, 3, 5) && c.is_selected(4, 3, 5) && !c.is_selected(0, 2, 5));
        assert!(c.click_header(1, 5, true));
        assert!(c.is_selected(2, 2, 5));
        assert!(!c.click_header(9, 5, false));
    }

    #[test]
    fn clicks_follow_the_display_order() {
        let mut c = controls(3);
        assert!(c.move_display(2, 0)); // display: model 2, 0, 1
        assert!(c.click_cell(0, 0, 5, false));
        assert_eq!(c.selection().active(), Some(CellPos { row: 0, col: 2 }));
        assert!(c.click_header(1, 5, false));
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
