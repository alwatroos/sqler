// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The lines of the Messages view: one per statement of the last run.

use super::status::format_duration;
use crate::query::{QueryTab, StatementStatus};

/// One line of the Messages view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLine {
    /// The statement label, such as `#2 DELETE`.
    pub title: String,
    /// The outcome text without the `Error: ` prefix.
    pub status: String,
    /// The time the statement took, when it finished or failed.
    pub duration: Option<String>,
    /// Whether the statement failed.
    pub failed: bool,
}

/// One line per statement of the tab's last run.
#[must_use]
pub fn message_lines(q: &QueryTab) -> Vec<MessageLine> {
    q.results()
        .iter()
        .map(|r| {
            let text = r.status_text();
            let status = text
                .strip_prefix("Error: ")
                .map_or(text.clone(), str::to_owned);
            let (duration, failed) = match &r.status {
                StatementStatus::Done { duration, .. } => (Some(format_duration(*duration)), false),
                StatementStatus::Failed { duration, .. } => {
                    (Some(format_duration(*duration)), true)
                }
                _ => (None, false),
            };
            MessageLine {
                title: r.title.clone(),
                status,
                duration,
                failed,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqler_sql::Dialect;

    use super::*;
    use crate::query::{BufferSnapshot, RunEvent, RunId, RunMode, Step, plan};

    const D: Option<Dialect> = Some(Dialect::Postgres);

    fn started(sql: &str) -> (QueryTab, RunId) {
        let planned = plan(D, &BufferSnapshot::at_cursor(sql, 0), RunMode::All).unwrap();
        let mut tab = QueryTab::new();
        let Step::Start(bound) = tab.request(D, planned, false).unwrap() else {
            panic!("expected a start")
        };
        let run = RunId::new(1);
        tab.started(run, &bound);
        (tab, run)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn no_run_has_no_lines() {
        assert_eq!(message_lines(&QueryTab::new()), []);
    }

    #[test]
    fn done_failed_and_not_run() {
        let (mut tab, run) = started("select 1; select x; select 3");
        tab.event(
            run,
            RunEvent::Finished {
                index: 0,
                rows: 1,
                rows_affected: None,
                duration: ms(12),
            },
        );
        tab.event(
            run,
            RunEvent::Failed {
                index: 1,
                message: "no such column: x".into(),
                duration: ms(1500),
            },
        );
        let lines = message_lines(&tab);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].title, "#1 SELECT");
        assert_eq!(lines[0].status, "1 row · 12 ms");
        assert_eq!(lines[0].duration.as_deref(), Some("12 ms"));
        assert!(!lines[0].failed);
        assert_eq!(lines[1].status, "no such column: x");
        assert_eq!(lines[1].duration.as_deref(), Some("1.5 s"));
        assert!(lines[1].failed);
        assert_eq!(lines[2].status, "Not run");
        assert_eq!(lines[2].duration, None);
        assert!(!lines[2].failed);
    }

    #[test]
    fn cancelled_has_no_duration() {
        let (mut tab, run) = started("select 1; select 2");
        tab.event(run, RunEvent::Cancelled { index: 0 });
        let lines = message_lines(&tab);
        assert_eq!(lines[0].status, "Cancelled");
        assert_eq!(lines[0].duration, None);
        assert!(!lines[0].failed);
        assert_eq!(lines[1].status, "Not run");
    }
}
