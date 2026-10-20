// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `Selection`: the active cell and the selected range of one results grid.
//!
//! The selection stores *model* rows and columns, so a later sort or column move does not change
//! what is selected. [`DisplayMap`] translates to on-screen positions, and [`Selection::rect`]
//! gives the selected block in display space.

use std::ops::Range;

use sqler_config::Platform;

use super::layout::ColumnLayout;
use crate::keymap::KeyInput;

/// A cell by model row and model column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPos {
    /// The model row.
    pub row: usize,
    /// The model column.
    pub col: usize,
}

/// What the selected range covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionKind {
    /// The block between the anchor and the active cell.
    #[default]
    Cells,
    /// Whole rows, from the anchor row to the active row.
    Rows,
    /// Whole columns, from the anchor column to the active column.
    Columns,
    /// Every cell, including rows that arrive later.
    All,
}

/// The order in which model rows are shown. T5 shows them as they are; a sort provides another.
pub trait RowOrder {
    /// The number of displayed rows.
    fn len(&self) -> usize;
    /// Whether no row is displayed.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The model row shown at this display position.
    fn model_row(&self, display: usize) -> Option<usize>;
    /// The display position of this model row.
    fn display_row(&self, model: usize) -> Option<usize>;
}

/// Display row = model row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityRows(pub usize);

impl RowOrder for IdentityRows {
    fn len(&self) -> usize {
        self.0
    }

    fn model_row(&self, display: usize) -> Option<usize> {
        (display < self.0).then_some(display)
    }

    fn display_row(&self, model: usize) -> Option<usize> {
        (model < self.0).then_some(model)
    }
}

/// How rows and columns are laid out on screen.
#[derive(Clone, Copy)]
pub struct DisplayMap<'a> {
    /// The row order.
    pub rows: &'a dyn RowOrder,
    /// The column layout.
    pub columns: &'a ColumnLayout,
}

impl DisplayMap<'_> {
    /// The cell at a display position.
    #[must_use]
    pub fn cell_at(&self, display_row: usize, display_col: usize) -> Option<CellPos> {
        Some(CellPos {
            row: self.rows.model_row(display_row)?,
            col: self.columns.model_column(display_col)?,
        })
    }

    /// The display position of a cell.
    fn position(&self, cell: CellPos) -> Option<(usize, usize)> {
        Some((
            self.rows.display_row(cell.row)?,
            self.columns.display_column(cell.col)?,
        ))
    }
}

/// A block of cells in display space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayRect {
    /// The display rows.
    pub rows: Range<usize>,
    /// The display columns.
    pub cols: Range<usize>,
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One row up.
    Up,
    /// One row down.
    Down,
    /// One column left.
    Left,
    /// One column right.
    Right,
    /// The first column of the row.
    RowStart,
    /// The last column of the row.
    RowEnd,
    /// One page up.
    PageUp,
    /// One page down.
    PageDown,
    /// The top-left cell.
    First,
    /// The bottom-right cell.
    Last,
}

/// The active cell, the anchor of the range and what the range covers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    anchor: Option<CellPos>,
    active: Option<CellPos>,
    kind: SelectionKind,
}

impl Selection {
    /// The active cell (the one the keyboard moves).
    #[must_use]
    pub fn active(&self) -> Option<CellPos> {
        self.active
    }

    /// The fixed end of the range.
    #[must_use]
    pub fn anchor(&self) -> Option<CellPos> {
        self.anchor
    }

    /// What the range covers.
    #[must_use]
    pub fn kind(&self) -> SelectionKind {
        self.kind
    }

    /// Selects one cell, or with `extend` and an anchor extends the range to it.
    pub fn click(&mut self, cell: CellPos, extend: bool) {
        if extend && self.anchor.is_some() {
            self.active = Some(cell);
        } else {
            self.anchor = Some(cell);
            self.active = Some(cell);
            self.kind = SelectionKind::Cells;
        }
    }

    /// Selects a whole column (a header click), or with `extend` extends to it. The active row
    /// stays, or is the first displayed row. Does nothing without rows.
    pub fn click_column(&mut self, model_col: usize, map: &DisplayMap, extend: bool) {
        if extend
            && self.anchor.is_some()
            && let Some(c) = self.active
        {
            self.active = Some(CellPos {
                row: c.row,
                col: model_col,
            });
            self.kind = SelectionKind::Columns;
            return;
        }
        let row = match self.active {
            Some(c) if map.rows.display_row(c.row).is_some() => c.row,
            _ => match map.rows.model_row(0) {
                Some(r) => r,
                None => return,
            },
        };
        let cell = CellPos {
            row,
            col: model_col,
        };
        self.anchor = Some(cell);
        self.active = Some(cell);
        self.kind = SelectionKind::Columns;
    }

    /// Selects the whole row of the active cell. Does nothing without an active cell.
    pub fn select_row(&mut self) {
        if self.active.is_some() {
            self.anchor = self.active;
            self.kind = SelectionKind::Rows;
        }
    }

    /// Selects the whole column of the active cell. Does nothing without an active cell.
    pub fn select_column(&mut self) {
        if self.active.is_some() {
            self.anchor = self.active;
            self.kind = SelectionKind::Columns;
        }
    }

    /// Selects every cell.
    pub fn select_all(&mut self) {
        self.kind = SelectionKind::All;
    }

    /// Moves the active cell. With `extend` the anchor and kind stay. `page` is the page size in
    /// rows. Returns whether the selection changed. A motion without an active cell starts at
    /// display (0, 0).
    pub fn apply_motion(&mut self, m: Motion, extend: bool, map: &DisplayMap, page: usize) -> bool {
        let (rows, cols) = (map.rows.len(), map.columns.len());
        if rows == 0 || cols == 0 {
            return false;
        }
        let (r, c) = self.active.and_then(|a| map.position(a)).unwrap_or((0, 0));
        let (last_r, last_c) = (rows - 1, cols - 1);
        let page = page.max(1);
        let (nr, nc) = match m {
            Motion::Up => (r.saturating_sub(1), c),
            Motion::Down => ((r + 1).min(last_r), c),
            Motion::Left => (r, c.saturating_sub(1)),
            Motion::Right => (r, (c + 1).min(last_c)),
            Motion::RowStart => (r, 0),
            Motion::RowEnd => (r, last_c),
            Motion::PageUp => (r.saturating_sub(page), c),
            Motion::PageDown => ((r + page).min(last_r), c),
            Motion::First => (0, 0),
            Motion::Last => (last_r, last_c),
        };
        let Some(cell) = map.cell_at(nr, nc) else {
            return false;
        };
        let before = self.clone();
        if extend {
            if self.anchor.is_none() {
                self.anchor = map.cell_at(r, c);
            }
        } else {
            self.anchor = Some(cell);
            self.kind = SelectionKind::Cells;
        }
        self.active = Some(cell);
        *self != before
    }

    /// The selected block in display space; `None` without a selection or without cells.
    #[must_use]
    pub fn rect(&self, map: &DisplayMap) -> Option<DisplayRect> {
        let (rows, cols) = (map.rows.len(), map.columns.len());
        if rows == 0 || cols == 0 {
            return None;
        }
        if self.kind == SelectionKind::All {
            return Some(DisplayRect {
                rows: 0..rows,
                cols: 0..cols,
            });
        }
        let (ar, ac) = map.position(self.anchor?)?;
        let (br, bc) = map.position(self.active?)?;
        let span = |a: usize, b: usize| a.min(b)..a.max(b) + 1;
        Some(match self.kind {
            SelectionKind::Cells | SelectionKind::All => DisplayRect {
                rows: span(ar, br),
                cols: span(ac, bc),
            },
            SelectionKind::Rows => DisplayRect {
                rows: span(ar, br),
                cols: 0..cols,
            },
            SelectionKind::Columns => DisplayRect {
                rows: 0..rows,
                cols: span(ac, bc),
            },
        })
    }

    /// Whether the cell at a display position is selected.
    #[must_use]
    pub fn contains(&self, display_row: usize, display_col: usize, map: &DisplayMap) -> bool {
        self.rect(map)
            .is_some_and(|r| r.rows.contains(&display_row) && r.cols.contains(&display_col))
    }

    /// Clears the selection.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The grid command a key runs while the grid has the focus, if any. `mod` is Cmd on macOS and
/// Ctrl elsewhere; Ctrl+Space selects a column on every platform. Alt gives `None`.
#[must_use]
pub fn key_command(input: &KeyInput, platform: Platform) -> Option<&'static str> {
    if input.alt {
        return None;
    }
    let (modifier, other) = match platform {
        Platform::MacOs => (input.cmd, input.ctrl),
        Platform::Other => (input.ctrl, input.cmd),
    };
    let shift = input.shift;
    if input.key == "space" {
        return match (input.ctrl, input.cmd, shift) {
            (false, false, true) => Some("grid:select-row"),
            (true, false, false) => Some("grid:select-column"),
            _ => None,
        };
    }
    if other {
        return None;
    }
    let pick = |plain: &'static str, extend: &'static str| Some(if shift { extend } else { plain });
    match (input.key.as_str(), modifier) {
        ("up", false) => pick("grid:move-up", "grid:extend-up"),
        ("down", false) => pick("grid:move-down", "grid:extend-down"),
        ("left", false) => pick("grid:move-left", "grid:extend-left"),
        ("right", false) => pick("grid:move-right", "grid:extend-right"),
        ("home", false) => pick("grid:move-row-start", "grid:extend-row-start"),
        ("end", false) => pick("grid:move-row-end", "grid:extend-row-end"),
        ("home", true) => pick("grid:move-first-cell", "grid:extend-first-cell"),
        ("end", true) => pick("grid:move-last-cell", "grid:extend-last-cell"),
        ("pageup", false) => pick("grid:move-page-up", "grid:extend-page-up"),
        ("pagedown", false) => pick("grid:move-page-down", "grid:extend-page-down"),
        ("tab", false) => pick("grid:move-right", "grid:move-left"),
        ("a", true) if !shift => Some("grid:select-all"),
        _ => None,
    }
}

/// The first row to scroll to so that `row` becomes visible; `None` when it already is. Scrolls
/// with the row at the top (up) or at the bottom (down).
#[must_use]
pub fn scroll_row(row: usize, visible: Range<usize>) -> Option<usize> {
    if visible.is_empty() || row < visible.start {
        Some(row)
    } else if row >= visible.end {
        Some(row + 1 - visible.len())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display row `d` shows model row `len - 1 - d`: proves the selection follows model rows.
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

    fn cell(row: usize, col: usize) -> CellPos {
        CellPos { row, col }
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

    fn at(rows: &dyn RowOrder, cols: &ColumnLayout, f: impl FnOnce(&DisplayMap)) {
        f(&DisplayMap {
            rows,
            columns: cols,
        });
    }

    #[allow(clippy::unnecessary_wraps)] // matches the type of `Selection::rect`
    fn rect(rows: Range<usize>, cols: Range<usize>) -> Option<DisplayRect> {
        Some(DisplayRect { rows, cols })
    }

    #[test]
    fn motions_clamp_at_the_edges() {
        let cols = ColumnLayout::new(vec![100.0; 4]);
        let rows = IdentityRows(10);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click(cell(0, 0), false);
            assert!(!s.apply_motion(Motion::Up, false, m, 5));
            assert!(!s.apply_motion(Motion::Left, false, m, 5));
            assert!(!s.apply_motion(Motion::PageUp, false, m, 5));
            assert!(!s.apply_motion(Motion::RowStart, false, m, 5));
            assert!(!s.apply_motion(Motion::First, false, m, 5));
            assert!(s.apply_motion(Motion::Last, false, m, 5));
            assert_eq!(s.active(), Some(cell(9, 3)));
            assert!(!s.apply_motion(Motion::Down, false, m, 5));
            assert!(!s.apply_motion(Motion::Right, false, m, 5));
            assert!(!s.apply_motion(Motion::PageDown, false, m, 5));
            assert!(!s.apply_motion(Motion::RowEnd, false, m, 5));
            assert!(s.apply_motion(Motion::RowStart, false, m, 5));
            assert_eq!(s.active(), Some(cell(9, 0)));
            assert!(s.apply_motion(Motion::Up, false, m, 5));
            assert!(s.apply_motion(Motion::Right, false, m, 5));
            assert_eq!(s.active(), Some(cell(8, 1)));
            assert!(s.apply_motion(Motion::RowEnd, false, m, 5));
            assert_eq!(s.active(), Some(cell(8, 3)));
            assert!(s.apply_motion(Motion::Left, false, m, 5));
            assert!(s.apply_motion(Motion::Down, false, m, 5));
            assert_eq!(s.active(), Some(cell(9, 2)));
        });
    }

    #[test]
    fn page_motions_use_the_page_size_and_at_least_one_row() {
        let cols = ColumnLayout::new(vec![100.0]);
        let rows = IdentityRows(100);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click(cell(50, 0), false);
            s.apply_motion(Motion::PageDown, false, m, 20);
            assert_eq!(s.active(), Some(cell(70, 0)));
            s.apply_motion(Motion::PageUp, false, m, 30);
            assert_eq!(s.active(), Some(cell(40, 0)));
            s.apply_motion(Motion::PageDown, false, m, 0);
            assert_eq!(s.active(), Some(cell(41, 0)));
            s.apply_motion(Motion::PageDown, false, m, 1000);
            assert_eq!(s.active(), Some(cell(99, 0)));
        });
    }

    #[test]
    fn motion_without_an_active_cell_starts_at_the_origin() {
        let cols = ColumnLayout::new(vec![100.0; 3]);
        let rows = IdentityRows(5);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            assert!(s.apply_motion(Motion::Up, false, m, 3));
            assert_eq!(s.active(), Some(cell(0, 0)));
            let mut s = Selection::default();
            assert!(s.apply_motion(Motion::Down, false, m, 3));
            assert_eq!(s.active(), Some(cell(1, 0)));
            let mut s = Selection::default();
            assert!(s.apply_motion(Motion::Right, true, m, 3));
            assert_eq!(s.anchor(), Some(cell(0, 0)));
            assert_eq!(s.active(), Some(cell(0, 1)));
        });
    }

    #[test]
    fn motions_do_nothing_on_an_empty_grid() {
        let no_cols = ColumnLayout::new(vec![]);
        let cols = ColumnLayout::new(vec![100.0]);
        for (rows, layout) in [(0, &cols), (3, &no_cols)] {
            let rows = IdentityRows(rows);
            at(&rows, layout, |m| {
                let mut s = Selection::default();
                for motion in [Motion::Down, Motion::Right, Motion::First, Motion::Last] {
                    assert!(!s.apply_motion(motion, false, m, 5));
                    assert!(!s.apply_motion(motion, true, m, 5));
                }
                assert_eq!(s, Selection::default());
                assert_eq!(s.rect(m), None);
                assert!(!s.contains(0, 0, m));
            });
        }
    }

    #[test]
    fn extend_keeps_the_anchor_and_plain_motion_resets_it() {
        let cols = ColumnLayout::new(vec![100.0; 4]);
        let rows = IdentityRows(10);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click(cell(2, 1), false);
            assert!(s.apply_motion(Motion::Down, true, m, 5));
            assert!(s.apply_motion(Motion::Right, true, m, 5));
            assert_eq!(s.anchor(), Some(cell(2, 1)));
            assert_eq!(s.active(), Some(cell(3, 2)));
            assert_eq!(s.rect(m), rect(2..4, 1..3));
            assert!(s.apply_motion(Motion::Up, true, m, 5));
            assert!(s.apply_motion(Motion::Up, true, m, 5));
            assert_eq!(s.rect(m), rect(1..3, 1..3));
            assert!(s.apply_motion(Motion::Left, false, m, 5));
            assert_eq!(s.anchor(), s.active());
            assert_eq!(s.kind(), SelectionKind::Cells);
            assert_eq!(s.rect(m), rect(1..2, 1..2));
        });
    }

    #[test]
    fn extend_at_an_edge_reports_no_change() {
        let cols = ColumnLayout::new(vec![100.0]);
        let rows = IdentityRows(2);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click(cell(1, 0), false);
            assert!(!s.apply_motion(Motion::Down, true, m, 5));
            assert!(!s.apply_motion(Motion::Right, true, m, 5));
        });
    }

    #[test]
    fn shift_click_extends_the_range() {
        let cols = ColumnLayout::new(vec![100.0; 5]);
        let rows = IdentityRows(10);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click(cell(4, 3), true); // no anchor yet: a plain click
            assert_eq!(s.anchor(), Some(cell(4, 3)));
            s.click(cell(1, 1), true);
            assert_eq!(s.anchor(), Some(cell(4, 3)));
            assert_eq!(s.active(), Some(cell(1, 1)));
            assert_eq!(s.rect(m), rect(1..5, 1..4));
            s.click(cell(7, 0), false);
            assert_eq!(s.rect(m), rect(7..8, 0..1));
        });
    }

    #[test]
    fn row_column_and_all_kinds() {
        let cols = ColumnLayout::new(vec![100.0; 4]);
        let rows = IdentityRows(6);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.select_row();
            s.select_column();
            assert_eq!(s, Selection::default(), "no active cell: nothing happens");
            s.click(cell(2, 1), false);
            s.select_row();
            assert_eq!(s.kind(), SelectionKind::Rows);
            assert_eq!(s.rect(m), rect(2..3, 0..4));
            assert!(s.apply_motion(Motion::Down, true, m, 5));
            assert_eq!(s.rect(m), rect(2..4, 0..4));
            s.select_column();
            assert_eq!(s.kind(), SelectionKind::Columns);
            assert_eq!(s.rect(m), rect(0..6, 1..2));
            assert!(s.apply_motion(Motion::Right, true, m, 5));
            assert_eq!(s.rect(m), rect(0..6, 1..3));
            s.select_all();
            assert_eq!(s.kind(), SelectionKind::All);
            assert_eq!(s.rect(m), rect(0..6, 0..4));
            assert!(s.contains(5, 3, m));
            assert!(s.apply_motion(Motion::Up, false, m, 5));
            assert_eq!(s.kind(), SelectionKind::Cells);
        });
    }

    #[test]
    fn select_all_covers_rows_that_arrive_later() {
        let cols = ColumnLayout::new(vec![100.0; 2]);
        let mut s = Selection::default();
        s.select_all();
        at(&IdentityRows(3), &cols, |m| {
            assert_eq!(s.rect(m), rect(0..3, 0..2));
        });
        at(&IdentityRows(8), &cols, |m| {
            assert_eq!(s.rect(m), rect(0..8, 0..2));
            assert!(s.contains(7, 1, m));
            assert!(!s.contains(8, 1, m));
        });
    }

    #[test]
    fn header_click_selects_a_column_and_extends() {
        let cols = ColumnLayout::new(vec![100.0; 5]);
        let rows = IdentityRows(4);
        at(&rows, &cols, |m| {
            let mut s = Selection::default();
            s.click_column(2, m, false);
            assert_eq!(s.kind(), SelectionKind::Columns);
            assert_eq!(s.active(), Some(cell(0, 2)));
            assert_eq!(s.rect(m), rect(0..4, 2..3));
            s.click_column(4, m, true);
            assert_eq!(s.rect(m), rect(0..4, 2..5));
            s.click_column(0, m, true);
            assert_eq!(s.rect(m), rect(0..4, 0..3));
            s.click(cell(3, 1), false);
            s.click_column(1, m, false);
            assert_eq!(s.active(), Some(cell(3, 1)), "keeps the active row");
            s.click(cell(2, 1), false);
            s.click_column(3, m, true);
            assert_eq!(s.kind(), SelectionKind::Columns);
            assert_eq!(s.rect(m), rect(0..4, 1..4));
        });
        at(&IdentityRows(0), &cols, |m| {
            let mut s = Selection::default();
            s.click_column(1, m, false);
            assert_eq!(s, Selection::default());
        });
    }

    #[test]
    fn rect_and_contains_follow_a_reordered_layout() {
        let mut cols = ColumnLayout::new(vec![100.0; 4]);
        let rows = IdentityRows(5);
        let mut s = Selection::default();
        s.click(cell(1, 0), false);
        s.click(cell(2, 1), true);
        at(&rows, &cols, |m| assert_eq!(s.rect(m), rect(1..3, 0..2)));
        assert!(cols.move_display(3, 0)); // order: 3 0 1 2
        at(&rows, &cols, |m| {
            assert_eq!(s.rect(m), rect(1..3, 1..3));
            assert!(s.contains(1, 1, m) && s.contains(2, 2, m));
            assert!(!s.contains(1, 0, m) && !s.contains(1, 3, m) && !s.contains(3, 1, m));
        });
        // Split by a move: model 0 and 1 are no longer adjacent, the block is still between them.
        assert!(cols.move_display(1, 3)); // order: 3 1 2 0
        at(&rows, &cols, |m| assert_eq!(s.rect(m), rect(1..3, 1..4)));
    }

    #[test]
    fn selection_follows_model_rows_through_a_row_order() {
        let cols = ColumnLayout::new(vec![100.0; 2]);
        let mut s = Selection::default();
        s.click(cell(1, 0), false);
        s.click(cell(3, 1), true);
        at(&IdentityRows(5), &cols, |m| {
            assert_eq!(s.rect(m), rect(1..4, 0..2));
        });
        // Reversed: model 3..=1 sit at display 1..=3, active model row 3 at display 1.
        at(&Reversed(5), &cols, |m| {
            assert_eq!(s.rect(m), rect(1..4, 0..2));
            assert!(s.contains(2, 0, m));
            assert_eq!(m.cell_at(0, 1), Some(cell(4, 1)));
            assert_eq!(m.cell_at(5, 1), None);
        });
        s.click(cell(0, 0), false);
        at(&Reversed(5), &cols, |m| {
            assert_eq!(s.rect(m), rect(4..5, 0..1));
            // Model row 0 is the last display row: Down is clamped there.
            assert!(!s.apply_motion(Motion::Down, false, m, 3));
            assert_eq!(s.active(), Some(cell(0, 0)));
        });
        at(&Reversed(5), &cols, |m| {
            assert!(s.apply_motion(Motion::Up, false, m, 3));
            assert_eq!(s.active(), Some(cell(1, 0)));
        });
    }

    #[test]
    fn a_selection_that_left_the_grid_has_no_rect() {
        let cols = ColumnLayout::new(vec![100.0; 2]);
        let mut s = Selection::default();
        s.click(cell(7, 1), false);
        at(&IdentityRows(5), &cols, |m| {
            assert_eq!(s.rect(m), None);
            assert!(
                s.apply_motion(Motion::Down, false, m, 3),
                "restarts at the origin"
            );
            assert_eq!(s.active(), Some(cell(1, 0)));
        });
        s.clear();
        assert_eq!(s, Selection::default());
    }

    #[test]
    fn key_table_on_both_platforms() {
        for platform in [Platform::MacOs, Platform::Other] {
            let m = if platform == Platform::MacOs {
                "m"
            } else {
                "c"
            };
            let k = |mods: &str, name: &str| key_command(&key(mods, name), platform);
            for (name, plain, ext) in [
                ("up", "move-up", "extend-up"),
                ("down", "move-down", "extend-down"),
                ("left", "move-left", "extend-left"),
                ("right", "move-right", "extend-right"),
                ("home", "move-row-start", "extend-row-start"),
                ("end", "move-row-end", "extend-row-end"),
                ("pageup", "move-page-up", "extend-page-up"),
                ("pagedown", "move-page-down", "extend-page-down"),
                ("tab", "move-right", "move-left"),
            ] {
                assert_eq!(
                    k("", name),
                    Some(format!("grid:{plain}")).as_deref(),
                    "{name}"
                );
                assert_eq!(
                    k("s", name),
                    Some(format!("grid:{ext}")).as_deref(),
                    "s+{name}"
                );
            }
            assert_eq!(k(m, "home"), Some("grid:move-first-cell"));
            assert_eq!(k(m, "end"), Some("grid:move-last-cell"));
            assert_eq!(k(&format!("{m}s"), "home"), Some("grid:extend-first-cell"));
            assert_eq!(k(&format!("{m}s"), "end"), Some("grid:extend-last-cell"));
            for name in ["up", "down", "left", "right", "pageup", "pagedown", "tab"] {
                assert_eq!(k(m, name), None, "mod+{name}");
            }
            assert_eq!(k(m, "a"), Some("grid:select-all"));
            assert_eq!(k("", "a"), None);
            assert_eq!(k(&format!("{m}s"), "a"), None);
            assert_eq!(k("s", "space"), Some("grid:select-row"));
            assert_eq!(k("c", "space"), Some("grid:select-column"));
            assert_eq!(k("", "space"), None);
            assert_eq!(k("cs", "space"), None);
            assert_eq!(k("a", "up"), None);
            assert_eq!(k("as", "space"), None);
            assert_eq!(k("ac", "space"), None);
            assert_eq!(k("", "x"), None);
            assert_eq!(k("", "escape"), None);
        }
    }

    #[test]
    fn the_other_modifier_blocks_a_key() {
        assert_eq!(key_command(&key("c", "home"), Platform::MacOs), None);
        assert_eq!(key_command(&key("c", "a"), Platform::MacOs), None);
        assert_eq!(key_command(&key("m", "home"), Platform::Other), None);
        assert_eq!(key_command(&key("m", "up"), Platform::Other), None);
        // Literal ctrl+space on macOS; cmd+space is not ours.
        assert_eq!(
            key_command(&key("c", "space"), Platform::MacOs),
            Some("grid:select-column")
        );
        assert_eq!(key_command(&key("m", "space"), Platform::MacOs), None);
    }

    #[test]
    fn scroll_row_targets() {
        assert_eq!(scroll_row(5, 10..20), Some(5));
        assert_eq!(scroll_row(9, 10..20), Some(9));
        assert_eq!(scroll_row(10, 10..20), None);
        assert_eq!(scroll_row(19, 10..20), None);
        assert_eq!(scroll_row(20, 10..20), Some(11));
        assert_eq!(scroll_row(100, 10..20), Some(91));
        assert_eq!(scroll_row(3, 0..0), Some(3));
        assert_eq!(scroll_row(0, 7..7), Some(0));
    }
}
