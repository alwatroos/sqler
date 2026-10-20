// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridTab`: the headless state of one results-grid tab.

use std::ops::Range;

use super::action::GridAction;
use super::cell::{self, ColumnHeader, group_thousands};
use super::stats::RenderStats;
use super::{
    GridCancel, GridLimits, GridModel, IngestState, SyntheticSource, grid_channel_with,
    spawn_ingest,
};

/// Rows of the synthetic result opened by `dev:open-synthetic-grid`.
pub const SYNTHETIC_ROWS: usize = 1_000_000;
/// Columns of the synthetic result.
pub const SYNTHETIC_COLUMNS: usize = 6;
/// Rows per batch of the synthetic source.
pub const SYNTHETIC_BATCH: usize = 1024;
/// Messages applied per poll, so one poll stays short.
pub const POLL_BUDGET: usize = 64;

/// What one [`GridTab::poll`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollOutcome {
    /// Messages applied.
    pub applied: usize,
    /// The column set arrived or changed.
    pub columns_changed: bool,
    /// The row count grew.
    pub rows_changed: bool,
    /// Ingestion has ended (done, cancelled or failed); polling can stop.
    pub terminal: bool,
}

/// A results-grid tab: the model, header labels, the visible row range and render statistics.
#[derive(Debug)]
pub struct GridTab {
    model: GridModel,
    headers: Vec<ColumnHeader>,
    visible: Range<usize>,
    stats: RenderStats,
}

impl GridTab {
    /// A tab over a model whose sink is fed elsewhere.
    #[must_use]
    pub fn new(model: GridModel) -> Self {
        Self {
            model,
            headers: Vec::new(),
            visible: 0..0,
            stats: RenderStats::default(),
        }
    }

    /// A tab fed by a [`SyntheticSource`] on its own ingestion thread, plus the flag that cancels
    /// it. The synthetic result is unbounded (the dev probe waits for `Done`). If the thread cannot
    /// start, the sink is dropped and the tab ends up `Failed`.
    #[must_use]
    pub fn spawn_synthetic(rows: usize, columns: usize, batch: usize) -> (Self, GridCancel) {
        let (model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        let cancel = sink.canceller();
        // On error the closure's sink is already dropped, so the model reports `Failed`.
        drop(spawn_ingest(
            SyntheticSource::new(rows, columns, batch),
            sink,
        ));
        (Self::new(model), cancel)
    }

    /// Applies up to `budget` pending messages without waiting.
    pub fn poll(&mut self, budget: usize) -> PollOutcome {
        let rows_before = self.model.row_count();
        let applied = self.model.poll(budget);
        let columns_changed = self.model.columns().len() != self.headers.len();
        if columns_changed {
            self.headers = self
                .model
                .columns()
                .iter()
                .map(ColumnHeader::from)
                .collect();
        }
        PollOutcome {
            applied,
            columns_changed,
            rows_changed: self.model.row_count() != rows_before,
            terminal: !matches!(
                self.model.state(),
                IngestState::Waiting | IngestState::Streaming | IngestState::Paused
            ),
        }
    }

    /// The ingestion state.
    #[must_use]
    pub fn state(&self) -> &IngestState {
        self.model.state()
    }

    /// Rows received so far.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.model.row_count()
    }

    /// Columns known so far (as of the last [`GridTab::poll`]).
    #[must_use]
    pub fn column_count(&self) -> usize {
        self.headers.len()
    }

    /// The header of a column.
    #[must_use]
    pub fn header(&self, col: usize) -> Option<&ColumnHeader> {
        self.headers.get(col)
    }

    /// The display text of a cell; empty when the row or column does not exist.
    #[must_use]
    pub fn cell_text(&self, row: usize, col: usize) -> String {
        self.model
            .row(row)
            .and_then(|r| r.get(col))
            .map(|c| cell::cell_text(&c))
            .unwrap_or_default()
    }

    /// Records the range of rows the view currently draws.
    pub fn set_visible(&mut self, r: Range<usize>) {
        self.visible = r;
    }

    /// The range of rows the view currently draws.
    #[must_use]
    pub fn visible(&self) -> Range<usize> {
        self.visible.clone()
    }

    /// The status line: loading progress or the final row count.
    #[must_use]
    pub fn status_text(&self) -> String {
        let n = group_thousands(self.row_count());
        match self.model.state() {
            IngestState::Waiting => "Waiting for columns…".to_owned(),
            IngestState::Streaming => format!("Loading… {n} rows"),
            IngestState::Paused => format!("{n} rows loaded; more rows available"),
            IngestState::Cancelled => format!("{n} rows (loading cancelled)"),
            IngestState::Failed(m) => format!("{n} rows (failed: {m})"),
            _ => format!("{n} rows"),
        }
    }

    /// `Rows 1–27 of 1,000,000` for the visible range; `None` when nothing is visible.
    #[must_use]
    pub fn range_text(&self) -> Option<String> {
        let end = self.visible.end.min(self.row_count());
        if self.visible.start >= end {
            return None;
        }
        Some(format!(
            "Rows {}–{} of {}",
            group_thousands(self.visible.start + 1),
            group_thousands(end),
            group_thousands(self.row_count())
        ))
    }

    /// The row an action scrolls to; `None` when there are no rows or the action does not scroll.
    #[must_use]
    pub fn jump_target(&self, a: GridAction) -> Option<usize> {
        let rows = self.row_count();
        if rows == 0 {
            return None;
        }
        Some(match a {
            GridAction::ScrollToTop => 0,
            GridAction::ScrollToEnd => rows - 1,
            _ => return None,
        })
    }

    /// An estimate of the heap bytes held by the rows. Walks every row: not for per-frame use.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.model.heap_bytes()
    }

    /// Loads the next page of a paused result; `false` when nothing is paused.
    pub fn fetch_more(&mut self) -> bool {
        self.model.fetch_more()
    }

    /// Cancels ingestion.
    pub fn cancel(&mut self) {
        self.model.cancel();
    }

    /// The render statistics.
    #[must_use]
    pub fn stats(&self) -> &RenderStats {
        &self.stats
    }

    /// The render statistics, for the view to record into.
    pub fn stats_mut(&mut self) -> &mut RenderStats {
        &mut self.stats
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::grid_channel;
    use super::super::viewport::{ROW_HEIGHT_PX, ScrollViewport};
    use super::*;

    fn wait_done(tab: &mut GridTab) {
        let start = Instant::now();
        while !matches!(tab.state(), IngestState::Done { .. }) {
            assert!(start.elapsed() < Duration::from_secs(20), "ingest stalled");
            assert!(
                !matches!(tab.state(), IngestState::Failed(_) | IngestState::Cancelled),
                "{:?}",
                tab.state()
            );
            if tab.poll(POLL_BUDGET).applied == 0 {
                std::thread::sleep(Duration::from_micros(100));
            }
        }
    }

    fn loaded(rows: usize) -> GridTab {
        let (mut tab, _) = GridTab::spawn_synthetic(rows, SYNTHETIC_COLUMNS, 128);
        wait_done(&mut tab);
        tab
    }

    #[test]
    fn waiting_before_anything_arrives() {
        let (model, _sink) = grid_channel();
        let mut tab = GridTab::new(model);
        let out = tab.poll(POLL_BUDGET);
        assert_eq!(
            out,
            PollOutcome {
                applied: 0,
                columns_changed: false,
                rows_changed: false,
                terminal: false
            }
        );
        assert_eq!(tab.status_text(), "Waiting for columns…");
        assert_eq!(tab.column_count(), 0);
        assert_eq!(tab.range_text(), None);
        assert_eq!(tab.jump_target(GridAction::ScrollToEnd), None);
        assert_eq!(tab.cell_text(0, 0), "");
    }

    #[test]
    fn poll_outcome_flags_follow_the_stream() {
        let (model, sink) = grid_channel();
        let mut tab = GridTab::new(model);
        let handle = spawn_ingest(SyntheticSource::new(10, 3, 4), sink).unwrap();
        handle.join().unwrap().unwrap();
        // Columns first.
        let out = tab.poll(1);
        assert_eq!(out.applied, 1);
        assert!(out.columns_changed && !out.rows_changed && !out.terminal);
        assert_eq!(tab.status_text(), "Loading… 0 rows");
        // One batch.
        let out = tab.poll(1);
        assert!(!out.columns_changed && out.rows_changed && !out.terminal);
        assert_eq!(tab.status_text(), "Loading… 4 rows");
        // The rest, up to Done.
        let out = tab.poll(POLL_BUDGET);
        assert!(!out.columns_changed && out.rows_changed && out.terminal);
        assert_eq!(out.applied, 3);
        assert_eq!(tab.status_text(), "10 rows");
        // Nothing left.
        let out = tab.poll(POLL_BUDGET);
        assert_eq!(out.applied, 0);
        assert!(!out.rows_changed && out.terminal);
    }

    #[test]
    fn synthetic_tab_exposes_headers_and_cells() {
        let tab = loaded(10_000);
        assert_eq!(tab.row_count(), 10_000);
        assert_eq!(tab.column_count(), SYNTHETIC_COLUMNS);
        let h = tab.header(0).unwrap();
        assert_eq!((h.name.as_str(), h.type_label), ("c0", "integer"));
        let labels: Vec<_> = (0..6).map(|c| tab.header(c).unwrap().type_label).collect();
        assert_eq!(
            labels,
            ["integer", "text", "float", "boolean", "integer", "text"]
        );
        assert!(tab.header(6).is_none());
        assert_eq!(tab.cell_text(1234, 0), "1234");
        assert_eq!(tab.cell_text(1234, 1), "r1234c1");
        assert_eq!(tab.cell_text(1234, 2), "1234.5");
        assert_eq!(tab.cell_text(1234, 3), "true");
        assert_eq!(tab.cell_text(1235, 3), "false");
        assert_eq!(tab.cell_text(1234, 4), "4936");
        assert_eq!(tab.cell_text(10_000, 0), "");
        assert_eq!(tab.cell_text(0, 99), "");
    }

    #[test]
    fn range_text_and_jump_targets() {
        let mut tab = loaded(2_500);
        assert_eq!(tab.range_text(), None);
        tab.set_visible(0..27);
        assert_eq!(tab.visible(), 0..27);
        assert_eq!(tab.range_text().as_deref(), Some("Rows 1–27 of 2,500"));
        tab.set_visible(2_400..2_600);
        assert_eq!(
            tab.range_text().as_deref(),
            Some("Rows 2,401–2,500 of 2,500")
        );
        tab.set_visible(5..5);
        assert_eq!(tab.range_text(), None);
        assert_eq!(tab.jump_target(GridAction::ScrollToTop), Some(0));
        assert_eq!(tab.jump_target(GridAction::ScrollToEnd), Some(2_499));
        assert_eq!(tab.jump_target(GridAction::FetchMore), None);
    }

    #[test]
    fn failed_when_the_sink_goes_away() {
        let (model, sink) = grid_channel();
        let mut tab = GridTab::new(model);
        drop(sink);
        let out = tab.poll(POLL_BUDGET);
        assert!(out.terminal);
        assert_eq!(
            tab.status_text(),
            "0 rows (failed: ingestion ended unexpectedly)"
        );
    }

    #[test]
    fn paused_status_text_and_fetch_more() {
        let (model, sink) = grid_channel_with(GridLimits::rows(1_000));
        let mut tab = GridTab::new(model);
        let handle = spawn_ingest(SyntheticSource::new(2_500, 3, 256), sink).unwrap();
        let start = Instant::now();
        while !matches!(tab.state(), IngestState::Paused) {
            assert!(start.elapsed() < Duration::from_secs(20), "never paused");
            tab.poll(POLL_BUDGET);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(tab.row_count(), 1_000);
        assert_eq!(tab.status_text(), "1,000 rows loaded; more rows available");
        assert!(!tab.poll(POLL_BUDGET).terminal);
        assert!(tab.fetch_more());
        assert!(!tab.fetch_more());
        assert_eq!(tab.status_text(), "Loading… 1,000 rows");
        while !matches!(tab.state(), IngestState::Paused) {
            assert!(start.elapsed() < Duration::from_secs(20), "never paused");
            tab.poll(POLL_BUDGET);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(tab.row_count(), 2_000);
        assert!(tab.fetch_more());
        wait_done(&mut tab);
        assert_eq!(tab.row_count(), 2_500);
        assert_eq!(tab.status_text(), "2,500 rows");
        assert!(!tab.fetch_more());
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn synthetic_tab_never_pauses() {
        // Larger than the default page.
        let (mut tab, _) = GridTab::spawn_synthetic(150_000, 2, 1024);
        let start = Instant::now();
        while !matches!(tab.state(), IngestState::Done { .. }) {
            assert!(start.elapsed() < Duration::from_secs(60), "ingest stalled");
            assert!(!matches!(tab.state(), IngestState::Paused));
            tab.poll(POLL_BUDGET);
            std::thread::sleep(Duration::from_micros(100));
        }
        assert_eq!(tab.row_count(), 150_000);
    }

    #[test]
    fn cancel_stops_loading_and_sets_the_flag() {
        let (mut tab, flag) = GridTab::spawn_synthetic(10_000_000, 3, 64);
        tab.poll(2);
        tab.cancel();
        assert!(flag.is_cancelled());
        assert!(matches!(tab.state(), IngestState::Cancelled));
        assert!(tab.status_text().ends_with("rows (loading cancelled)"));
        let n = tab.row_count();
        assert!(tab.poll(POLL_BUDGET).terminal);
        assert_eq!(tab.row_count(), n);
    }

    #[test]
    fn external_cancel_flag_reaches_the_ingest_thread() {
        let (mut tab, flag) = GridTab::spawn_synthetic(100_000_000, 2, 64);
        flag.cancel();
        let start = Instant::now();
        while !tab.poll(usize::MAX).terminal {
            assert!(start.elapsed() < Duration::from_secs(20), "never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(tab.state(), IngestState::Cancelled));
    }

    #[test]
    fn simulated_scroll_over_a_million_rows_draws_bounded_rows() {
        let rows = 1_000_000;
        let mut vp = ScrollViewport::new(ROW_HEIGHT_PX, 760.0, 2);
        vp.set_rows(rows);
        let cap = vp.max_visible_rows();
        // The model content is irrelevant to the windowing, so a small tab stands in for it.
        let mut tab = loaded(1_000);
        let frame = |tab: &mut GridTab, vp: &ScrollViewport| {
            let r = vp.visible();
            tab.set_visible(r.clone());
            for row in r {
                tab.stats_mut().row_rendered(row, rows);
            }
            tab.stats_mut().end_frame(None);
        };
        while vp.offset() < vp.max_offset() {
            frame(&mut tab, &vp);
            vp.scroll_by(96.0);
        }
        vp.jump_to_end();
        frame(&mut tab, &vp);
        assert!(tab.stats().max_rows_per_frame() <= cap);
        assert!(tab.stats().max_rows_per_frame() > 20);
        assert!(tab.stats().saw_last());
        assert!(tab.stats().frames() > 300_000);
    }
}
