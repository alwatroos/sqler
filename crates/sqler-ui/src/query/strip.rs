// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The tone of a result strip segment, derived from the statement status.

use super::StatementStatus;

/// How a segment of the result strip looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    /// Waiting for its turn.
    Pending,
    /// Executing.
    Running,
    /// Finished.
    Done,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
    /// Never started.
    NotRun,
}

/// The tone of a statement status.
#[must_use]
pub fn status_tone(s: &StatementStatus) -> StatusTone {
    match s {
        StatementStatus::Pending => StatusTone::Pending,
        StatementStatus::Running => StatusTone::Running,
        StatementStatus::Done { .. } => StatusTone::Done,
        StatementStatus::Failed { .. } => StatusTone::Failed,
        StatementStatus::Cancelled => StatusTone::Cancelled,
        StatementStatus::NotRun => StatusTone::NotRun,
    }
}

/// The word for the accessibility label.
#[must_use]
pub fn status_word(t: StatusTone) -> &'static str {
    match t {
        StatusTone::Pending => "pending",
        StatusTone::Running => "running",
        StatusTone::Done => "done",
        StatusTone::Failed => "failed",
        StatusTone::Cancelled => "cancelled",
        StatusTone::NotRun => "not run",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn every_status_maps() {
        let d = Duration::from_millis(1);
        let cases = [
            (StatementStatus::Pending, StatusTone::Pending, "pending"),
            (StatementStatus::Running, StatusTone::Running, "running"),
            (
                StatementStatus::Done {
                    rows: 1,
                    rows_affected: None,
                    duration: d,
                },
                StatusTone::Done,
                "done",
            ),
            (
                StatementStatus::Failed {
                    message: "x".to_owned(),
                    duration: d,
                },
                StatusTone::Failed,
                "failed",
            ),
            (
                StatementStatus::Cancelled,
                StatusTone::Cancelled,
                "cancelled",
            ),
            (StatementStatus::NotRun, StatusTone::NotRun, "not run"),
        ];
        for (status, tone, word) in cases {
            assert_eq!(status_tone(&status), tone);
            assert_eq!(status_word(tone), word);
        }
    }
}
