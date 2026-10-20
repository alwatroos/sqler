// SPDX-License-Identifier: Apache-2.0
//! Smoke report: fixed `SPIKE key=value` lines, verdict and watchdog.

use std::io::Write as _;
use std::time::Duration;

/// Seconds after which the watchdog kills the process.
pub const WATCHDOG_SECS: u64 = 120;

/// Everything the smoke run measures.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Display server actually used.
    pub display: String,
    /// Renderer / adapter name, or `unknown`.
    pub renderer: String,
    /// Milliseconds from `main()` start to the first presented frame.
    pub first_frame_ms: f64,
    /// Rows in the grid source.
    pub grid_rows: usize,
    /// Largest per-frame delegate row count.
    pub grid_max_rows_rendered_per_frame: u32,
    /// Whether the last row was rendered after `scroll_to_row`.
    pub grid_rendered_last_row: bool,
    /// Distinct highlight names on the sample SQL.
    pub editor_highlight_kinds: u32,
    /// Physical memory at idle, in MB.
    pub rss_mb: f64,
}

impl Report {
    /// Pass conditions of the contract; the error is a short slug.
    ///
    /// # Errors
    /// Returns the slug of the first failed check.
    pub fn verdict(&self) -> Result<(), &'static str> {
        if self.grid_max_rows_rendered_per_frame > 500 {
            return Err("grid-not-virtualised");
        }
        if !self.grid_rendered_last_row {
            return Err("grid-last-row-not-rendered");
        }
        if self.editor_highlight_kinds < 3 {
            return Err("editor-highlight-kinds");
        }
        Ok(())
    }

    /// The report lines, in contract order.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let result = match self.verdict() {
            Ok(()) => "pass".to_owned(),
            Err(reason) => format!("fail reason={reason}"),
        };
        vec![
            "SPIKE backend=gpui".to_owned(),
            format!("SPIKE display={}", self.display),
            format!("SPIKE renderer={}", self.renderer),
            format!("SPIKE first_frame_ms={:.1}", self.first_frame_ms),
            format!("SPIKE grid_rows={}", self.grid_rows),
            format!(
                "SPIKE grid_max_rows_rendered_per_frame={}",
                self.grid_max_rows_rendered_per_frame
            ),
            format!(
                "SPIKE grid_rendered_last_row={}",
                self.grid_rendered_last_row
            ),
            format!(
                "SPIKE editor_highlight_kinds={}",
                self.editor_highlight_kinds
            ),
            format!("SPIKE rss_mb={:.1}", self.rss_mb),
            format!("SPIKE result={result}"),
        ]
    }
}

/// Prints the failure line for a run that could not produce a report.
pub fn fail(reason: &str) {
    println!("SPIKE result=fail reason={reason}");
    let _ = std::io::stdout().flush();
}

/// Prints the report and exits with `0` (pass) or `1` (a check failed).
pub fn finish(report: &Report) -> ! {
    let mut out = std::io::stdout().lock();
    for line in report.lines() {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
    std::process::exit(i32::from(report.verdict().is_err()));
}

/// Kills the process with exit code 2 if it is still alive after [`WATCHDOG_SECS`].
pub fn spawn_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(WATCHDOG_SECS));
        fail("watchdog");
        std::process::exit(2);
    });
}

/// Idle physical memory in MB, or 0 when the OS does not report it.
#[must_use]
pub fn rss_mb() -> f64 {
    #[allow(clippy::cast_precision_loss)]
    memory_stats::memory_stats().map_or(0.0, |s| s.physical_mem as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            display: "x11".into(),
            renderer: "unknown".into(),
            first_frame_ms: 123.456,
            grid_rows: 1_000_000,
            grid_max_rows_rendered_per_frame: 40,
            grid_rendered_last_row: true,
            editor_highlight_kinds: 7,
            rss_mb: 210.04,
        }
    }

    #[test]
    fn report_lines_follow_the_contract() {
        let lines = sample().lines();
        let keys: Vec<&str> = lines
            .iter()
            .map(|l| l.split('=').next().unwrap_or_default())
            .collect();
        assert_eq!(
            keys,
            [
                "SPIKE backend",
                "SPIKE display",
                "SPIKE renderer",
                "SPIKE first_frame_ms",
                "SPIKE grid_rows",
                "SPIKE grid_max_rows_rendered_per_frame",
                "SPIKE grid_rendered_last_row",
                "SPIKE editor_highlight_kinds",
                "SPIKE rss_mb",
                "SPIKE result",
            ]
        );
        assert_eq!(lines[3], "SPIKE first_frame_ms=123.5");
        assert_eq!(lines[9], "SPIKE result=pass");
    }

    #[test]
    fn failing_checks_give_a_slug() {
        let mut r = sample();
        r.grid_max_rows_rendered_per_frame = 501;
        assert_eq!(r.verdict(), Err("grid-not-virtualised"));
        assert_eq!(
            r.lines()[9],
            "SPIKE result=fail reason=grid-not-virtualised"
        );
        let mut r = sample();
        r.grid_rendered_last_row = false;
        assert_eq!(r.verdict(), Err("grid-last-row-not-rendered"));
        let mut r = sample();
        r.editor_highlight_kinds = 2;
        assert_eq!(r.verdict(), Err("editor-highlight-kinds"));
    }
}
