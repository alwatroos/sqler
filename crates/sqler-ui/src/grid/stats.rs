// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Render statistics and the report printed by the in-app grid probe.

/// The nearest-rank percentile of an ascending slice; `None` when it is empty.
#[must_use]
pub fn nearest_rank<T: Copy + PartialOrd>(sorted: &[T], pct: u32) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let pct = usize::try_from(pct.min(100)).unwrap_or(100);
    let rank = (sorted.len() * pct).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

/// What the grid drew: rows per frame, whether the last row was ever drawn, frame intervals.
#[derive(Debug, Clone, Default)]
pub struct RenderStats {
    frame_rows: usize,
    max_rows: usize,
    saw_last: bool,
    frames: u64,
    intervals_ms: Vec<f64>,
}

impl RenderStats {
    /// Records that row `row_ix` of `row_count` was drawn in the current frame.
    pub fn row_rendered(&mut self, row_ix: usize, row_count: usize) {
        self.frame_rows += 1;
        if row_count > 0 && row_ix + 1 == row_count {
            self.saw_last = true;
        }
    }

    /// Closes the current frame; `interval_ms` is the time since the previous frame, if known.
    pub fn end_frame(&mut self, interval_ms: Option<f64>) {
        self.frames += 1;
        self.max_rows = self.max_rows.max(self.frame_rows);
        self.frame_rows = 0;
        if let Some(ms) = interval_ms {
            self.intervals_ms.push(ms);
        }
    }

    /// The most rows drawn in one frame so far.
    #[must_use]
    pub fn max_rows_per_frame(&self) -> usize {
        self.max_rows
    }

    /// Whether the last row of the result was drawn at least once.
    #[must_use]
    pub fn saw_last(&self) -> bool {
        self.saw_last
    }

    /// Frames closed so far.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The `pct`-th percentile of the recorded frame intervals in milliseconds.
    #[must_use]
    pub fn frame_ms(&self, pct: u32) -> Option<f64> {
        let mut sorted = self.intervals_ms.clone();
        sorted.sort_by(f64::total_cmp);
        nearest_rank(&sorted, pct)
    }

    /// The longest recorded frame interval in milliseconds.
    #[must_use]
    pub fn max_frame_ms(&self) -> Option<f64> {
        self.intervals_ms.iter().copied().max_by(f64::total_cmp)
    }
}

/// The result of one probe run, printed as `SQLER key=value` lines.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    /// The operating system name.
    pub os: String,
    /// Rows loaded.
    pub rows: usize,
    /// Milliseconds from the first poll to the `Done` state.
    pub ingest_ms: f64,
    /// Frames measured while scrolling.
    pub frames: u64,
    /// Median frame interval in milliseconds.
    pub frame_ms_p50: f64,
    /// 95th percentile frame interval in milliseconds.
    pub frame_ms_p95: f64,
    /// Longest frame interval in milliseconds.
    pub frame_ms_max: f64,
    /// Most rows drawn in a single frame.
    pub max_rows_rendered_per_frame: usize,
    /// Whether the last row was drawn.
    pub rendered_last_row: bool,
    /// Resident set size in MB, if the platform offers it.
    pub rss_mb: Option<f64>,
    /// Why the run failed before it could measure, as a slug.
    pub failure: Option<&'static str>,
}

/// More rows than this in one frame means the grid is not virtualised.
const MAX_ROWS_PER_FRAME: usize = 500;

impl ProbeReport {
    /// `None` when the run passes, else the failure slug.
    #[must_use]
    pub fn verdict(&self) -> Option<&'static str> {
        if let Some(f) = self.failure {
            Some(f)
        } else if self.max_rows_rendered_per_frame > MAX_ROWS_PER_FRAME {
            Some("grid-not-virtualised")
        } else if !self.rendered_last_row {
            Some("grid-last-row-not-rendered")
        } else {
            None
        }
    }

    /// The report lines, each starting with `SQLER `.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let rss = self
            .rss_mb
            .map_or_else(|| "n/a".to_owned(), |v| format!("{v:.1}"));
        let result = match self.verdict() {
            None => "pass".to_owned(),
            Some(slug) => format!("fail reason={slug}"),
        };
        vec![
            format!("SQLER grid_probe os={}", self.os),
            format!("SQLER grid_rows={}", self.rows),
            format!("SQLER grid_ingest_ms={:.1}", self.ingest_ms),
            format!("SQLER grid_frames={}", self.frames),
            format!("SQLER grid_frame_ms_p50={:.1}", self.frame_ms_p50),
            format!("SQLER grid_frame_ms_p95={:.1}", self.frame_ms_p95),
            format!("SQLER grid_frame_ms_max={:.1}", self.frame_ms_max),
            format!(
                "SQLER grid_max_rows_rendered_per_frame={}",
                self.max_rows_rendered_per_frame
            ),
            format!("SQLER grid_rendered_last_row={}", self.rendered_last_row),
            format!("SQLER grid_rss_mb={rss}"),
            format!("SQLER grid_result={result}"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> ProbeReport {
        ProbeReport {
            os: "macos".into(),
            rows: 1_000_000,
            ingest_ms: 812.34,
            frames: 601,
            frame_ms_p50: 8.3,
            frame_ms_p95: 9.04,
            frame_ms_max: 20.0,
            max_rows_rendered_per_frame: 27,
            rendered_last_row: true,
            rss_mb: Some(312.25),
            failure: None,
        }
    }

    #[test]
    fn nearest_rank_percentiles() {
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!(nearest_rank(&v, 50), Some(50));
        assert_eq!(nearest_rank(&v, 95), Some(95));
        assert_eq!(nearest_rank(&v, 100), Some(100));
        assert_eq!(nearest_rank(&v, 0), Some(1));
        assert_eq!(nearest_rank(&v, 900), Some(100));
        assert_eq!(nearest_rank(&[7], 95), Some(7));
        assert_eq!(nearest_rank::<u32>(&[], 50), None);
    }

    #[test]
    fn frames_fold_into_stats() {
        let mut s = RenderStats::default();
        assert_eq!(
            (s.frames(), s.max_rows_per_frame(), s.saw_last()),
            (0, 0, false)
        );
        assert_eq!(s.frame_ms(50), None);
        assert_eq!(s.max_frame_ms(), None);
        for i in 0..5 {
            s.row_rendered(i, 100);
        }
        s.end_frame(None);
        for i in 95..100 {
            s.row_rendered(i, 100);
        }
        s.row_rendered(0, 100);
        s.end_frame(Some(10.0));
        s.end_frame(Some(30.0));
        s.end_frame(Some(20.0));
        assert_eq!(s.frames(), 4);
        assert_eq!(s.max_rows_per_frame(), 6);
        assert!(s.saw_last());
        assert_eq!(s.frame_ms(50), Some(20.0));
        assert_eq!(s.frame_ms(95), Some(30.0));
        assert_eq!(s.max_frame_ms(), Some(30.0));
    }

    #[test]
    fn empty_result_never_sees_last() {
        let mut s = RenderStats::default();
        s.row_rendered(0, 0);
        assert!(!s.saw_last());
    }

    #[test]
    fn lines_have_pinned_order_and_format() {
        let lines = report().lines();
        assert_eq!(
            lines,
            [
                "SQLER grid_probe os=macos",
                "SQLER grid_rows=1000000",
                "SQLER grid_ingest_ms=812.3",
                "SQLER grid_frames=601",
                "SQLER grid_frame_ms_p50=8.3",
                "SQLER grid_frame_ms_p95=9.0",
                "SQLER grid_frame_ms_max=20.0",
                "SQLER grid_max_rows_rendered_per_frame=27",
                "SQLER grid_rendered_last_row=true",
                "SQLER grid_rss_mb=312.2",
                "SQLER grid_result=pass",
            ]
        );
        let mut r = report();
        r.rss_mb = None;
        assert_eq!(r.lines()[9], "SQLER grid_rss_mb=n/a");
    }

    #[test]
    fn verdicts() {
        assert_eq!(report().verdict(), None);
        let mut r = report();
        r.max_rows_rendered_per_frame = 501;
        assert_eq!(r.verdict(), Some("grid-not-virtualised"));
        r.rendered_last_row = false;
        assert_eq!(r.verdict(), Some("grid-not-virtualised"));
        r.max_rows_rendered_per_frame = 500;
        assert_eq!(r.verdict(), Some("grid-last-row-not-rendered"));
        r.failure = Some("ingest-failed");
        assert_eq!(r.verdict(), Some("ingest-failed"));
        assert_eq!(
            r.lines().last().map(String::as_str),
            Some("SQLER grid_result=fail reason=ingest-failed")
        );
    }
}
