// SPDX-License-Identifier: Apache-2.0
//! gpui-component table over `SyntheticRows`, with per-frame row render counters.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    Styled as _, Window, div, px,
};
use gpui_component::table::{Column, TableDelegate, TableState};

use crate::data::{COL_COUNT, HEADERS, ROW_COUNT, SyntheticRows};

/// Frame and row counters shared between the grid delegate and the smoke driver.
#[derive(Default)]
pub struct FrameStats {
    /// Rows the delegate rendered since the last frame boundary.
    pending_rows: Cell<u32>,
    /// Largest per-frame row count seen so far.
    max_rows: Cell<u32>,
    /// True once the last row (`ROW_COUNT - 1`) was rendered.
    saw_last: Cell<bool>,
    /// Frames presented so far.
    frames: Cell<u64>,
    /// Milliseconds from process start to the first presented frame.
    first_frame_ms: Cell<Option<f64>>,
}

impl FrameStats {
    /// Creates zeroed counters.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    fn row_rendered(&self, row_ix: usize) {
        self.pending_rows.set(self.pending_rows.get() + 1);
        if row_ix + 1 == ROW_COUNT {
            self.saw_last.set(true);
        }
    }

    /// Closes the current frame: folds the pending row count into the maximum.
    pub fn end_frame(&self, ms_since_start: f64) {
        self.max_rows
            .set(self.max_rows.get().max(self.pending_rows.get()));
        self.pending_rows.set(0);
        self.frames.set(self.frames.get() + 1);
        if self.first_frame_ms.get().is_none() {
            self.first_frame_ms.set(Some(ms_since_start));
        }
    }

    /// Largest number of rows rendered in a single frame.
    #[must_use]
    pub fn max_rows(&self) -> u32 {
        self.max_rows.get()
    }

    /// Whether the last row was rendered at any point.
    #[must_use]
    pub fn saw_last(&self) -> bool {
        self.saw_last.get()
    }

    /// Frames presented so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames.get()
    }

    /// Time to the first presented frame, if there was one.
    #[must_use]
    pub fn first_frame_ms(&self) -> Option<f64> {
        self.first_frame_ms.get()
    }
}

/// Table delegate: fixed column widths, rows formatted on demand.
pub struct GridDelegate {
    rows: SyntheticRows,
    columns: Vec<Column>,
    stats: Rc<FrameStats>,
}

impl GridDelegate {
    /// Builds the 1M-row source and the fixed-width columns.
    #[must_use]
    pub fn new(stats: Rc<FrameStats>) -> Self {
        let widths = [90., 150., 60., 80., 80., 110., 420., 90.];
        let columns = (0..COL_COUNT)
            .map(|i| {
                Column::new(HEADERS[i], HEADERS[i])
                    .width(px(widths[i]))
                    .resizable(false)
                    .movable(false)
            })
            .collect();
        Self {
            rows: SyntheticRows::new(ROW_COUNT),
            columns,
            stats,
        }
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        COL_COUNT
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        self.stats.row_rendered(row_ix);
        div().id(("row", row_ix))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(self.rows.cell(row_ix, col_ix))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows.cell(row_ix, col_ix)
    }
}
