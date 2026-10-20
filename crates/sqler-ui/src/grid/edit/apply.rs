// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Runs an [`ApplyPlan`] in one transaction.

use sqler_driver_api::sqler_core::Query;
use sqler_driver_api::{BoxFuture, CancelFlag, DriverError, Session, collect};

#[cfg(doc)]
use super::plan::ApplyPlan;
use super::plan::ConfirmedPlan;

/// Why an apply was rolled back, or why its `COMMIT` is in doubt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplyFailure {
    /// A statement did not affect the expected number of rows.
    RowCount {
        /// What the plan requires.
        expected: u64,
        /// What the driver reported; `None` if it reported nothing.
        actual: Option<u64>,
    },
    /// The driver reported an error.
    Driver(String),
    /// The cancel flag was set, or the driver reported a cancel.
    Cancelled,
}

/// The result of [`apply()`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplyOutcome {
    /// Everything was committed; `applied` data statements ran. The caller clears the staged edits.
    Committed {
        /// The number of data statements.
        applied: usize,
    },
    /// `BEGIN` returned an error; no data statement ran. A `ROLLBACK` was sent (its result
    /// ignored) only if the outcome of `BEGIN` was ambiguous (a cancel, a lost connection or any
    /// other error that is not a statement error), because the server may have begun the
    /// transaction. After a definite statement error ([`DriverError::Query`], for example
    /// `SQLite`'s "cannot start a transaction within a transaction") nothing was sent: a
    /// `ROLLBACK` there would discard the user's own open transaction.
    NotStarted {
        /// The driver's message.
        message: String,
    },
    /// Nothing was committed: a body statement failed or the cancel flag was seen before
    /// `COMMIT` was sent, and `ROLLBACK` ran. The staged edits are untouched and may be retried.
    RolledBack {
        /// The index of the failing data statement; `None` if the cancel flag was seen after the
        /// last data statement, before `COMMIT` was sent.
        at: Option<usize>,
        /// Why.
        failure: ApplyFailure,
        /// The error of the `ROLLBACK` statement itself, if it failed.
        rollback_error: Option<String>,
    },
    /// `COMMIT` was sent and returned an error or a cancel. It is **not known** whether the
    /// changes were committed: the connection may have dropped after the server committed, and
    /// the `MySQL` driver reports a cancel for a statement that completed if the cancel flag was
    /// set while it ran. A `ROLLBACK` was sent as well (harmless after a commit). The staged edits
    /// are untouched, but the caller must **not offer a plain retry**: it must tell the user to
    /// re-read the data first, because retrying could repeat `INSERT`s (duplicate rows on tables
    /// with generated keys).
    CommitUnknown {
        /// What `COMMIT` returned.
        failure: ApplyFailure,
        /// The error of the `ROLLBACK` statement that followed, if it failed.
        rollback_error: Option<String>,
    },
}

/// Runs one statement and reads its stream to the end; returns the rows affected.
async fn run(session: &mut (dyn Session + '_), query: &Query) -> Result<Option<u64>, DriverError> {
    let mut stream = session.execute(query).await?;
    let result = collect(&mut *stream).await?;
    Ok(result.rows_affected())
}

/// Whether the outcome of a failed `BEGIN` is unknown. A statement error ([`DriverError::Query`]:
/// the server answered "no", including "already in a transaction") and errors raised before
/// anything was sent ([`DriverError::Unsupported`]) are definite: this `BEGIN` opened nothing.
/// Everything else (a cancel, which the `MySQL` driver reports for a statement that completed,
/// a lost connection, a protocol error, any passed-through backend error) may have followed a
/// `BEGIN` that took effect.
fn begin_outcome_is_ambiguous(error: &DriverError) -> bool {
    !matches!(error, DriverError::Query(_) | DriverError::Unsupported(_))
}

fn failure_of(error: &DriverError) -> ApplyFailure {
    match error {
        DriverError::Cancelled => ApplyFailure::Cancelled,
        other => ApplyFailure::Driver(other.to_string()),
    }
}

/// Runs `BEGIN`, the plan's statements and `COMMIT` on `session`.
///
/// Each data statement must report exactly its expected number of rows affected. On a different
/// count, a driver error or a cancel (checked before every data statement and before `COMMIT`)
/// it runs `ROLLBACK` (which ignores `stop`) and reports [`ApplyOutcome::RolledBack`] with where
/// and why. A failing `BEGIN` reports [`ApplyOutcome::NotStarted`]; a `ROLLBACK` (result ignored)
/// is sent first only when the outcome of `BEGIN` is ambiguous (a cancel, a connection-level or
/// unknown error), never after a definite statement error ([`DriverError::Query`]), because that
/// `ROLLBACK` would undo the user's own open transaction. An error or cancel returned
/// by `COMMIT` itself reports [`ApplyOutcome::CommitUnknown`], never `RolledBack`: whether the
/// server committed cannot be told from the client. Only the plan's own queries are executed, and
/// the staged edits are never touched, so after any failure the caller still has them.
///
/// # Preconditions
///
/// * The plan went through [`ApplyPlan::confirm`] (which refuses a `prod` plan the caller does
///   not assert the user confirmed) or [`ApplyPlan::confirm_non_production`]. The types make
///   skipping the step impossible; they cannot check the caller's flags.
/// * **The session must not be inside a user transaction. This is a REQUIRED guard for the
///   caller (T11).** The driver API has no transaction state, so this function cannot check it:
///   the caller must not apply while the query tab or session has an open user transaction as
///   far as the app knows (a state flag in the workspace). What happens otherwise depends on
///   the engine:
///   * `SQLite`: `BEGIN` fails with "cannot start a transaction within a transaction", a
///     definite error; nothing is sent after it and the user's transaction is untouched
///     ([`ApplyOutcome::NotStarted`]).
///   * `PostgreSQL`: `BEGIN` only warns, so the plan's data statements join the user's
///     transaction and its `COMMIT` commits the user's pending work too; after a failure the
///     plan's `ROLLBACK` discards it.
///   * `MySQL`/`MariaDB`: `START TRANSACTION` implicitly commits the user's pending work before
///     the apply starts.
///
/// It never panics.
pub fn apply<'a>(
    session: &'a mut (dyn Session + '_),
    plan: ConfirmedPlan,
    stop: &'a CancelFlag,
) -> BoxFuture<'a, ApplyOutcome> {
    let plan = plan.into_plan();
    Box::pin(async move {
        if let Err(e) = run(&mut *session, plan.begin()).await {
            // A definite statement error means this BEGIN opened nothing, and a ROLLBACK would
            // undo the user's own open transaction. Only when the outcome is unknown (a cancel is
            // reported for a completed statement, a connection error can follow a BEGIN that took
            // effect) close any transaction it may have opened; ignore the result.
            if begin_outcome_is_ambiguous(&e) {
                let _ = run(&mut *session, plan.rollback()).await;
            }
            return ApplyOutcome::NotStarted {
                message: e.to_string(),
            };
        }
        let mut failed: Option<(Option<usize>, ApplyFailure)> = None;
        for (i, statement) in plan.statements().iter().enumerate() {
            if stop.is_cancelled() {
                failed = Some((Some(i), ApplyFailure::Cancelled));
                break;
            }
            match run(&mut *session, &statement.query).await {
                Ok(actual) if actual == Some(statement.expected_rows) => {}
                Ok(actual) => {
                    failed = Some((
                        Some(i),
                        ApplyFailure::RowCount {
                            expected: statement.expected_rows,
                            actual,
                        },
                    ));
                    break;
                }
                Err(e) => {
                    failed = Some((Some(i), failure_of(&e)));
                    break;
                }
            }
        }
        let mut commit_failure: Option<ApplyFailure> = None;
        if failed.is_none() {
            if stop.is_cancelled() {
                failed = Some((None, ApplyFailure::Cancelled));
            } else if let Err(e) = run(&mut *session, plan.commit()).await {
                commit_failure = Some(failure_of(&e));
            }
        }
        if failed.is_none() && commit_failure.is_none() {
            return ApplyOutcome::Committed {
                applied: plan.statements().len(),
            };
        }
        let rollback_error = run(&mut *session, plan.rollback())
            .await
            .err()
            .map(|e| e.to_string());
        match (failed, commit_failure) {
            (Some((at, failure)), _) => ApplyOutcome::RolledBack {
                at,
                failure,
                rollback_error,
            },
            (None, Some(failure)) => ApplyOutcome::CommitUnknown {
                failure,
                rollback_error,
            },
            (None, None) => ApplyOutcome::Committed {
                applied: plan.statements().len(),
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use sqler_driver_api::sqler_core::{Column, DataType, Value};
    use sqler_driver_api::{CancelHandle, QueryEvent, QueryStream};

    use super::*;
    use crate::grid::edit::testing::{row, target};
    use crate::grid::edit::{ApplyPlan, ModelRow, StagedEdits};

    /// What the scripted session answers for the n-th executed statement.
    #[derive(Clone)]
    enum Answer {
        Affected(Option<u64>),
        Fail(&'static str),
        Cancelled,
    }

    struct Scripted {
        log: Arc<Mutex<Vec<String>>>,
        answers: VecDeque<Answer>,
        stop: Option<(usize, CancelFlag)>,
    }

    struct Events(VecDeque<Result<QueryEvent, DriverError>>);

    impl QueryStream for Events {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            Box::pin(std::future::ready(self.0.pop_front()))
        }
    }

    impl Session for Scripted {
        fn cancel_handle(&self) -> CancelHandle {
            CancelFlag::new().handle()
        }

        fn execute<'s>(
            &'s mut self,
            query: &'s Query,
        ) -> BoxFuture<'s, Result<Box<dyn QueryStream + 's>, DriverError>> {
            let n = {
                let mut log = self.log.lock().unwrap();
                log.push(query.sql.clone());
                log.len()
            };
            if let Some((at, flag)) = &self.stop
                && *at == n
            {
                flag.cancel();
            }
            let answer = self
                .answers
                .pop_front()
                .unwrap_or(Answer::Affected(Some(1)));
            let result: Result<Box<dyn QueryStream + 's>, DriverError> = match answer {
                Answer::Fail(m) => Err(DriverError::Query(m.to_owned())),
                Answer::Cancelled => Err(DriverError::Cancelled),
                Answer::Affected(rows_affected) => {
                    let events = VecDeque::from([
                        Ok(QueryEvent::Columns(vec![Column::new(
                            "a",
                            DataType::Integer,
                        )])),
                        Ok(QueryEvent::Done { rows_affected }),
                    ]);
                    Ok(Box::new(Events(events)))
                }
            };
            Box::pin(std::future::ready(result))
        }

        fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), DriverError>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn two_row_plan() -> ApplyPlan {
        let mut s = StagedEdits::new(target(false));
        s.set_cell(ModelRow(0), &row(1, Some("a")), 1, Value::Text("x".into()))
            .unwrap();
        s.delete_row(ModelRow(1), &row(2, Some("b"))).unwrap();
        s.plan().unwrap()
    }

    fn run_with(
        plan: &ApplyPlan,
        answers: Vec<Answer>,
        stop: &CancelFlag,
        stop_at: Option<usize>,
    ) -> (ApplyOutcome, Vec<String>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut session = Scripted {
            log: Arc::clone(&log),
            answers: answers.into(),
            stop: stop_at.map(|n| (n, stop.clone())),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let outcome = rt.block_on(apply(
            &mut session,
            plan.clone().confirm_non_production(),
            stop,
        ));
        let sql = log.lock().unwrap().clone();
        (outcome, sql)
    }

    fn listing(plan: &ApplyPlan, with_rollback: bool) -> Vec<String> {
        let mut v = vec![plan.begin().sql.clone()];
        v.extend(plan.statements().iter().map(|s| s.query.sql.clone()));
        v.push(plan.commit().sql.clone());
        if with_rollback {
            v.push(plan.rollback().sql.clone());
        }
        v
    }

    #[test]
    fn commit_path_runs_exactly_the_listing() {
        let plan = two_row_plan();
        let (outcome, sql) = run_with(&plan, vec![], &CancelFlag::new(), None);
        assert_eq!(outcome, ApplyOutcome::Committed { applied: 2 });
        assert_eq!(sql, listing(&plan, false));
    }

    #[test]
    fn zero_rows_rolls_back() {
        let plan = two_row_plan();
        let answers = vec![Answer::Affected(None), Answer::Affected(Some(0))];
        let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(0),
                failure: ApplyFailure::RowCount {
                    expected: 1,
                    actual: Some(0)
                },
                rollback_error: None
            }
        );
        // BEGIN, first statement, ROLLBACK: the second statement and COMMIT never ran.
        assert_eq!(sql.len(), 3);
        assert_eq!(sql[2], "ROLLBACK");
    }

    #[test]
    fn two_rows_and_unreported_count_roll_back() {
        let plan = two_row_plan();
        let answers = vec![
            Answer::Affected(None),
            Answer::Affected(Some(1)),
            Answer::Affected(Some(2)),
        ];
        let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(1),
                failure: ApplyFailure::RowCount {
                    expected: 1,
                    actual: Some(2)
                },
                rollback_error: None
            }
        );
        let mut expected = listing(&plan, true);
        expected.remove(3); // COMMIT never ran
        assert_eq!(sql, expected);
        let answers = vec![Answer::Affected(None), Answer::Affected(None)];
        let (outcome, _) = run_with(&plan, answers, &CancelFlag::new(), None);
        assert!(matches!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(0),
                failure: ApplyFailure::RowCount { actual: None, .. },
                ..
            }
        ));
    }

    #[test]
    fn driver_error_rolls_back_and_rollback_error_is_reported() {
        let plan = two_row_plan();
        let answers = vec![
            Answer::Affected(None),
            Answer::Fail("boom"),
            Answer::Fail("gone"),
        ];
        let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
        match outcome {
            ApplyOutcome::RolledBack {
                at,
                failure: ApplyFailure::Driver(m),
                rollback_error: Some(r),
            } => {
                assert_eq!(at, Some(0));
                assert!(m.contains("boom"));
                assert!(r.contains("gone"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sql.last().map(String::as_str), Some("ROLLBACK"));
    }

    /// Runs the two-row plan with `begin` as the answer to BEGIN and `rollback` for any
    /// ROLLBACK that follows.
    fn begin_fails(begin: Answer, rollback: Answer) -> (ApplyOutcome, Vec<String>) {
        let plan = two_row_plan();
        run_with(&plan, vec![begin, rollback], &CancelFlag::new(), None)
    }

    #[test]
    fn definite_begin_error_is_not_started_without_rollback() {
        // `already in a transaction`: a ROLLBACK would undo the user's transaction.
        let (outcome, sql) = begin_fails(
            Answer::Fail("cannot start a transaction within a transaction"),
            Answer::Affected(None),
        );
        match outcome {
            ApplyOutcome::NotStarted { message } => {
                assert!(message.contains("within a transaction"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(sql, ["BEGIN"]);
    }

    #[test]
    fn unsupported_begin_is_definite() {
        assert!(!begin_outcome_is_ambiguous(&DriverError::Unsupported(
            "x".into()
        )));
    }

    #[test]
    fn ambiguous_begin_outcomes_roll_back() {
        // A cancel (also reported for a completed statement) and connection-level errors.
        assert!(begin_outcome_is_ambiguous(&DriverError::Connection(
            "reset".into()
        )));
        assert!(begin_outcome_is_ambiguous(&DriverError::Protocol(
            "x".into()
        )));
        assert!(begin_outcome_is_ambiguous(&DriverError::Backend(
            "io".into()
        )));
        for rollback in [Answer::Affected(None), Answer::Fail("no transaction")] {
            let (outcome, sql) = begin_fails(Answer::Cancelled, rollback);
            assert!(matches!(outcome, ApplyOutcome::NotStarted { .. }));
            assert_eq!(sql, ["BEGIN", "ROLLBACK"]);
        }
    }

    #[test]
    fn commit_error_is_in_doubt_not_rolled_back() {
        // The connection may have dropped after the server committed: the outcome must not say
        // "rolled back", and the rollback that follows is still sent.
        let plan = two_row_plan();
        let answers = vec![
            Answer::Affected(None),
            Answer::Affected(Some(1)),
            Answer::Affected(Some(1)),
            Answer::Fail("connection reset"),
        ];
        let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
        match outcome {
            ApplyOutcome::CommitUnknown {
                failure: ApplyFailure::Driver(m),
                rollback_error: None,
            } => assert!(m.contains("connection reset")),
            other => panic!("{other:?}"),
        }
        assert_eq!(sql, listing(&plan, true));
    }

    #[test]
    fn cancel_seen_during_commit_is_in_doubt() {
        // The MySQL driver turns a COMMIT that completed into Cancelled when the flag was set.
        let plan = two_row_plan();
        let answers = vec![
            Answer::Affected(None),
            Answer::Affected(Some(1)),
            Answer::Affected(Some(1)),
            Answer::Cancelled,
            Answer::Fail("gone"),
        ];
        let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
        match outcome {
            ApplyOutcome::CommitUnknown {
                failure: ApplyFailure::Cancelled,
                rollback_error: Some(r),
            } => assert!(r.contains("gone")),
            other => panic!("{other:?}"),
        }
        assert_eq!(sql, listing(&plan, true));
    }

    #[test]
    fn cancel_before_a_statement_rolls_back_and_rollback_ignores_the_flag() {
        let plan = two_row_plan();
        // The flag is set while BEGIN runs (statement 1): nothing after BEGIN executes but ROLLBACK.
        let stop = CancelFlag::new();
        let (outcome, sql) = run_with(&plan, vec![], &stop, Some(1));
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: Some(0),
                failure: ApplyFailure::Cancelled,
                rollback_error: None
            }
        );
        assert_eq!(sql, ["BEGIN", "ROLLBACK"]);
        // Set while the last statement runs: COMMIT is not sent.
        let stop = CancelFlag::new();
        let (outcome, sql) = run_with(&plan, vec![], &stop, Some(3));
        assert_eq!(
            outcome,
            ApplyOutcome::RolledBack {
                at: None,
                failure: ApplyFailure::Cancelled,
                rollback_error: None
            }
        );
        assert_eq!(sql.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!sql.iter().any(|s| s == "COMMIT"));
    }

    #[test]
    fn driver_cancel_is_reported_as_cancelled() {
        let plan = two_row_plan();
        let answers = vec![Answer::Affected(None), Answer::Cancelled];
        let (outcome, _) = run_with(&plan, answers, &CancelFlag::new(), None);
        assert!(matches!(
            outcome,
            ApplyOutcome::RolledBack {
                failure: ApplyFailure::Cancelled,
                ..
            }
        ));
    }
}
