// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Property-style checks of `GridControls`, `ColumnLayout` and `Selection`: random action
//! sequences over random grid sizes must keep their invariants. Seeded, so failures reproduce.

// Test helpers outside `#[test]` fns unwrap; the generator casts are bounded by small sizes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::range_plus_one
)]

use sqler_ui::grid::{
    CellPos, ColumnHeader, ColumnLayout, DisplayMap, GridAction, GridControls, IdentityRows,
    Motion, RowOrder, Selection, SelectionKind,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// A shuffled row order (a bijection between display and model rows).
struct Permuted {
    to_model: Vec<usize>,
    to_display: Vec<usize>,
}

impl Permuted {
    fn new(n: usize, rng: &mut Rng) -> Self {
        let mut to_model: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            to_model.swap(i, rng.below(i + 1));
        }
        let mut to_display = vec![0; n];
        for (d, &m) in to_model.iter().enumerate() {
            to_display[m] = d;
        }
        Self {
            to_model,
            to_display,
        }
    }
}

impl RowOrder for Permuted {
    fn len(&self) -> usize {
        self.to_model.len()
    }
    fn model_row(&self, d: usize) -> Option<usize> {
        self.to_model.get(d).copied()
    }
    fn display_row(&self, m: usize) -> Option<usize> {
        self.to_display.get(m).copied()
    }
}

const MOTIONS: [Motion; 10] = [
    Motion::Up,
    Motion::Down,
    Motion::Left,
    Motion::Right,
    Motion::RowStart,
    Motion::RowEnd,
    Motion::PageUp,
    Motion::PageDown,
    Motion::First,
    Motion::Last,
];

fn headers(n: usize) -> Vec<ColumnHeader> {
    (0..n)
        .map(|i| ColumnHeader {
            name: format!("c{i}"),
            type_label: "int",
        })
        .collect()
}

fn check_layout(l: &ColumnLayout, cols: usize) {
    assert_eq!(l.len(), cols);
    assert!(l.pinned_count() <= cols);
    let mut seen = vec![false; cols];
    for d in 0..cols {
        let m = l.model_column(d).expect("display in range");
        assert!(m < cols && !seen[m], "order is not a bijection");
        seen[m] = true;
        assert_eq!(l.display_column(m), Some(d));
        let w = l.width(m);
        assert!((sqler_ui::grid::MIN_COLUMN_PX..=sqler_ui::grid::MAX_COLUMN_PX).contains(&w));
        assert_eq!(l.is_pinned(m), d < l.pinned_count());
    }
    assert_eq!(l.model_column(cols), None);
}

fn check_selection(s: &Selection, map: &DisplayMap) {
    let (rows, cols) = (map.rows.len(), map.columns.len());
    if let Some(a) = s.active() {
        assert!(map.rows.display_row(a.row).is_some(), "active row oob");
        assert!(
            map.columns.display_column(a.col).is_some(),
            "active col oob"
        );
    }
    if let Some(r) = s.rect(map) {
        assert!(r.rows.start < r.rows.end && r.rows.end <= rows, "{r:?}");
        assert!(r.cols.start < r.cols.end && r.cols.end <= cols, "{r:?}");
        if let Some(a) = s.active() {
            let dr = map.rows.display_row(a.row).unwrap();
            let dc = map.columns.display_column(a.col).unwrap();
            assert!(s.contains(dr, dc, map), "active cell outside its own rect");
        }
    } else if rows > 0 && cols > 0 {
        assert!(s.active().is_none() && s.kind() != SelectionKind::All);
    }
    assert!(!s.contains(rows, 0, map) && !s.contains(0, cols, map));
}

fn random_action(rng: &mut Rng) -> GridAction {
    match rng.below(8) {
        0..=2 => GridAction::Move {
            to: MOTIONS[rng.below(MOTIONS.len())],
            extend: rng.below(2) == 0,
        },
        _ => [
            GridAction::SelectAll,
            GridAction::SelectRow,
            GridAction::SelectColumn,
            GridAction::PinColumn,
            GridAction::UnpinColumn,
            GridAction::MoveColumnLeft,
            GridAction::MoveColumnRight,
            GridAction::AutofitColumn,
            GridAction::ResetLayout,
            GridAction::ScrollToTop,
            GridAction::ScrollToEnd,
        ][rng.below(11)],
    }
}

#[test]
fn controls_keep_invariants_under_random_sequences() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0..400 {
        let cols = [0, 1, 2, 3, 8, 50][rng.below(6)];
        let mut rows = [0, 1, 2, 7, 100][rng.below(5)];
        let mut gc = GridControls::default();
        gc.sync_columns(&headers(cols));
        for step in 0..120 {
            let vis_start = rng.below(rows + 1);
            let vis = vis_start..(vis_start + rng.below(30)).min(rows.max(vis_start));
            let pinned_set = |g: &GridControls| {
                let l = g.layout();
                let mut v: Vec<usize> = (0..l.pinned_count())
                    .map(|d| l.model_column(d).unwrap())
                    .collect();
                v.sort_unstable();
                v
            };
            let before = pinned_set(&gc);
            let a = random_action(&mut rng);
            let out = gc.apply(a, &IdentityRows(rows), vis.clone());
            let ctx = format!("case {case} step {step} {a:?} rows {rows} cols {cols}");
            if let Some(r) = out.scroll_row {
                assert!(r < rows, "{ctx}: scroll row oob");
            }
            for c in [out.scroll_col, out.autofit].into_iter().flatten() {
                assert!(c < cols, "{ctx}: col oob");
            }
            // The pinned set only changes through pin/unpin/reset.
            if !matches!(
                a,
                GridAction::PinColumn | GridAction::UnpinColumn | GridAction::ResetLayout
            ) {
                assert_eq!(before, pinned_set(&gc), "{ctx}: pinned set drifted");
            }
            match rng.below(6) {
                0 => {
                    gc.click_cell(
                        rng.below(rows + 2),
                        rng.below(cols + 2),
                        &IdentityRows(rows),
                        rng.below(2) == 0,
                    );
                }
                1 => {
                    gc.click_header(rng.below(cols + 2), &IdentityRows(rows), rng.below(2) == 0);
                }
                2 => {
                    gc.move_display(rng.below(cols + 2), rng.below(cols + 2));
                }
                3 => {
                    let w: Vec<f32> = (0..cols + 1)
                        .map(|_| {
                            [f32::NAN, -5.0, 0.0, 3.0, 120.0, 1e9, f32::INFINITY][rng.below(7)]
                        })
                        .collect();
                    gc.set_display_widths(&w);
                }
                4 => rows += rng.below(50), // fetch-more appends rows
                _ => {}
            }
            check_layout(gc.layout(), cols);
            let order = IdentityRows(rows);
            let map = DisplayMap {
                rows: &order,
                columns: gc.layout(),
            };
            check_selection(gc.selection(), &map);
            for _ in 0..3 {
                let (r, c) = (rng.below(rows + 1), rng.below(cols + 1));
                let _ = gc.is_selected(r, c, &order);
            }
        }
    }
}

#[test]
fn selection_with_permuted_rows_is_a_valid_rect_over_model_rows() {
    let mut rng = Rng(42);
    for _ in 0..300 {
        let rows = [0, 1, 2, 9, 60][rng.below(5)];
        let cols = [0, 1, 4][rng.below(3)];
        let order = Permuted::new(rows, &mut rng);
        let mut layout = ColumnLayout::new(vec![100.0; cols]);
        let mut sel = Selection::default();
        for _ in 0..80 {
            if cols > 0 && rng.below(5) == 0 {
                let m = rng.below(cols);
                let _ = layout.pin(m);
                let _ = layout.move_display(rng.below(cols), rng.below(cols));
            }
            let map = DisplayMap {
                rows: &order,
                columns: &layout,
            };
            match rng.below(5) {
                0 => {
                    if let Some(c) = map.cell_at(rng.below(rows + 1), rng.below(cols + 1)) {
                        sel.click(c, rng.below(2) == 0);
                    }
                }
                1 if cols > 0 => sel.click_column(rng.below(cols), &map, rng.below(2) == 0),
                1 => {}
                2 => sel.select_row(),
                3 => sel.select_all(),
                _ => {
                    let m = MOTIONS[rng.below(10)];
                    sel.apply_motion(m, rng.below(2) == 0, &map, rng.below(12));
                }
            }
            let map = DisplayMap {
                rows: &order,
                columns: &layout,
            };
            check_selection(&sel, &map);
            check_layout(&layout, cols);
        }
    }
}

#[test]
fn range_over_reversed_rows_covers_the_display_span_between_anchor_and_active() {
    let order = Permuted {
        to_model: vec![4, 3, 2, 1, 0],
        to_display: vec![4, 3, 2, 1, 0],
    };
    let layout = ColumnLayout::new(vec![100.0; 3]);
    let map = DisplayMap {
        rows: &order,
        columns: &layout,
    };
    let mut sel = Selection::default();
    sel.click(CellPos { row: 4, col: 0 }, false); // display row 0
    sel.click(CellPos { row: 2, col: 2 }, true); // display row 2
    let r = sel.rect(&map).unwrap();
    assert_eq!((r.rows, r.cols), (0..3, 0..3));
    // Display rows 0..3 are model rows 4, 3, 2.
    assert!(sel.contains(1, 1, &map) && !sel.contains(3, 1, &map));
}

#[test]
fn appended_rows_keep_the_selection_and_all_covers_them() {
    let mut gc = GridControls::default();
    gc.sync_columns(&headers(3));
    gc.click_cell(1, 1, &IdentityRows(3), false);
    gc.apply(GridAction::SelectAll, &IdentityRows(3), 0..3);
    assert!(gc.is_selected(2, 2, &IdentityRows(3)) && !gc.is_selected(3, 2, &IdentityRows(3)));
    assert!(
        gc.is_selected(9, 2, &IdentityRows(10)),
        "select-all covers fetched rows"
    );
    gc.apply(GridAction::SelectRow, &IdentityRows(10), 0..10);
    assert_eq!(gc.selection().active(), Some(CellPos { row: 1, col: 1 }));
}

#[test]
fn edges_and_empty_grids_do_nothing_harmful() {
    for (rows, cols) in [(0, 0), (0, 3), (3, 0)] {
        let mut gc = GridControls::default();
        gc.sync_columns(&headers(cols));
        for a in GridAction::ALL {
            let out = gc.apply(a, &IdentityRows(rows), 0..rows);
            assert!(out.autofit.is_none() && out.scroll_col.is_none(), "{a:?}");
        }
        assert!(!gc.click_cell(0, 0, &IdentityRows(rows), false));
    }
    // Keyboard at the edges stays put.
    let mut gc = GridControls::default();
    gc.sync_columns(&headers(2));
    gc.apply(
        GridAction::Move {
            to: Motion::First,
            extend: false,
        },
        &IdentityRows(5),
        0..5,
    );
    for to in [Motion::Up, Motion::Left, Motion::PageUp, Motion::RowStart] {
        gc.apply(
            GridAction::Move { to, extend: false },
            &IdentityRows(5),
            0..5,
        );
        assert_eq!(gc.selection().active(), Some(CellPos { row: 0, col: 0 }));
    }
    gc.apply(
        GridAction::Move {
            to: Motion::Last,
            extend: false,
        },
        &IdentityRows(5),
        0..5,
    );
    for to in [
        Motion::Down,
        Motion::Right,
        Motion::PageDown,
        Motion::RowEnd,
    ] {
        gc.apply(
            GridAction::Move { to, extend: false },
            &IdentityRows(5),
            0..5,
        );
        assert_eq!(gc.selection().active(), Some(CellPos { row: 4, col: 1 }));
    }
}

#[test]
fn all_columns_pinned_cannot_cross_and_reset_restores() {
    let mut l = ColumnLayout::new(vec![100.0; 4]);
    for m in 0..4 {
        assert!(l.pin(m));
    }
    assert!(!l.move_display(0, 4) && !l.move_left(0) && l.move_display(0, 3));
    check_layout(&l, 4);
    assert!(l.unpin(0));
    assert!(!l.move_display(0, 3), "cannot cross the pinned boundary");
    l.reset();
    assert_eq!(l, ColumnLayout::new(vec![100.0; 4]));
}
