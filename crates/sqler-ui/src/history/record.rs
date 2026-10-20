// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! From a run event to a history entry.

use std::time::{Duration, SystemTime};

use sqler_config::{ConnectionId, HistoryOutcome, NewHistoryEntry};

use crate::query::RunEvent;

/// The history entry for a statement that reached its end, `None` for [`RunEvent::Started`].
///
/// `sql` is the statement text before binding, so bound values are not in it. The error message of
/// a failed statement is dropped. `started_at` is when the statement started, if the caller saw
/// it; otherwise the start is `now - duration`. A cancelled statement has no duration of its own:
/// it is the time since `started_at` (zero when that is unknown).
#[must_use]
pub fn history_entry(
    connection: &ConnectionId,
    sql: &str,
    event: &RunEvent,
    started_at: Option<SystemTime>,
    now: SystemTime,
) -> Option<NewHistoryEntry> {
    let (outcome, duration, rows) = match event {
        RunEvent::Finished {
            rows,
            rows_affected,
            duration,
            ..
        } => {
            let count = match rows_affected {
                Some(affected) if *rows == 0 => *affected,
                _ => *rows as u64,
            };
            (HistoryOutcome::Ok, *duration, Some(count))
        }
        RunEvent::Failed { duration, .. } => (HistoryOutcome::Error, *duration, None),
        RunEvent::Cancelled { .. } => {
            let elapsed = started_at
                .and_then(|start| now.duration_since(start).ok())
                .unwrap_or(Duration::ZERO);
            (HistoryOutcome::Cancelled, elapsed, None)
        }
        _ => return None,
    };
    Some(NewHistoryEntry {
        sql: sql.to_owned(),
        connection: connection.clone(),
        started_at: started_at.unwrap_or_else(|| now.checked_sub(duration).unwrap_or(now)),
        duration,
        outcome,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use sqler_driver_api::sqler_core::{ConnectionConfig, DriverId};

    use super::*;
    use crate::sidebar::SidebarModel;

    fn conn() -> ConnectionId {
        let driver = DriverId::new("sqlite").unwrap();
        SidebarModel::from_connections(vec![ConnectionConfig::new("dev", driver, ":memory:")])
            .entries()[0]
            .id()
            .clone()
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn started_records_nothing() {
        let event = RunEvent::Started { index: 0 };
        assert!(history_entry(&conn(), "select 1", &event, None, at(100)).is_none());
    }

    #[test]
    fn finished_with_rows_counts_the_rows() {
        let event = RunEvent::Finished {
            index: 0,
            rows: 7,
            rows_affected: Some(99),
            duration: Duration::from_millis(12),
        };
        let e = history_entry(&conn(), "select 1", &event, None, at(100)).unwrap();
        assert_eq!(e.outcome, HistoryOutcome::Ok);
        assert_eq!(e.rows, Some(7));
        assert_eq!(e.duration, Duration::from_millis(12));
        assert_eq!(e.sql, "select 1");
        assert_eq!(
            e.started_at,
            at(100).checked_sub(Duration::from_millis(12)).unwrap()
        );
    }

    #[test]
    fn finished_without_rows_counts_the_affected_rows() {
        let affected = RunEvent::Finished {
            index: 0,
            rows: 0,
            rows_affected: Some(3),
            duration: Duration::ZERO,
        };
        let e = history_entry(&conn(), "delete from t", &affected, None, at(5)).unwrap();
        assert_eq!(e.rows, Some(3));
        let none = RunEvent::Finished {
            index: 0,
            rows: 0,
            rows_affected: None,
            duration: Duration::ZERO,
        };
        let e = history_entry(&conn(), "select 1 where false", &none, None, at(5)).unwrap();
        assert_eq!(e.rows, Some(0));
    }

    #[test]
    fn failed_drops_the_message() {
        let event = RunEvent::Failed {
            index: 0,
            message: "secret detail".to_owned(),
            duration: Duration::from_secs(2),
        };
        let e =
            history_entry(&conn(), "select * from nope", &event, Some(at(90)), at(100)).unwrap();
        assert_eq!(e.outcome, HistoryOutcome::Error);
        assert_eq!(e.rows, None);
        assert_eq!(e.started_at, at(90));
        assert!(!format!("{e:?}").contains("secret detail"));
    }

    #[test]
    fn cancelled_measures_from_the_start() {
        let event = RunEvent::Cancelled { index: 0 };
        let e = history_entry(&conn(), "select 1", &event, Some(at(90)), at(100)).unwrap();
        assert_eq!(e.outcome, HistoryOutcome::Cancelled);
        assert_eq!(e.duration, Duration::from_secs(10));
        assert_eq!(e.rows, None);
        let e = history_entry(&conn(), "select 1", &event, None, at(100)).unwrap();
        assert_eq!(e.duration, Duration::ZERO);
        assert_eq!(e.started_at, at(100));
        // A start in the future (clock jump) gives zero, not a panic.
        let e = history_entry(&conn(), "select 1", &event, Some(at(110)), at(100)).unwrap();
        assert_eq!(e.duration, Duration::ZERO);
    }
}
