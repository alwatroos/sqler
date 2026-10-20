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
    /// `BEGIN` returned an error; no data statement ran. A `ROLLBACK` was sent anyway (and its
    /// result ignored), because the server may have begun the transaction before the error
    /// reached the client; without it the session could be left inside one.
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
/// and why. A failing `BEGIN` reports [`ApplyOutcome::NotStarted`] after a `ROLLBACK` whose result
/// is ignored. An error or cancel returned
/// by `COMMIT` itself reports [`ApplyOutcome::CommitUnknown`], never `RolledBack`: whether the
/// server committed cannot be told from the client. Only the plan's own queries are executed, and
/// the staged edits are never touched, so after any failure the caller still has them.
///
/// # Preconditions
///
/// * The plan was confirmed: [`ConfirmedPlan`] is built only by [`ApplyPlan::confirm`] (which
///   refuses a `prod` plan the user did not confirm) or by
///   [`ApplyPlan::confirm_non_production`].
/// * **The session must not be inside a user transaction.** The driver API has no transaction
///   state, so this function cannot check it, and the caller guarantees it. Inside an open
///   transaction `PostgreSQL` only warns on `BEGIN`, so the plan's `COMMIT` would commit, and a
///   failed apply's `ROLLBACK` would discard, the user's pending work; `MySQL`'s
///   `START TRANSACTION` implicitly commits it. Apply on a session with no open transaction
///   (for example a fresh one, or one on which the query editor has not opened a transaction).
///
/// It never panics.
pub fn apply<'a>(
    session: &'a mut (dyn Session + '_),
    plan: ConfirmedPlan<'a>,
    stop: &'a CancelFlag,
) -> BoxFuture<'a, ApplyOutcome> {
    let plan = plan.into_plan();
    Box::pin(async move {
        if let Err(e) = run(&mut *session, plan.begin()).await {
            // `BEGIN` may have run (a cancel is reported for a completed statement, and an error
            // can arrive after the server began): close any open transaction, ignore the result.
            let _ = run(&mut *session, plan.rollback()).await;
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
        let outcome = rt.block_on(apply(&mut session, plan.confirm_non_production(), stop));
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

    #[test]
    fn begin_failure_is_not_started_and_rolls_back() {
        // The server may have begun the transaction before the error reached the client.
        let plan = two_row_plan();
        for rollback in [Answer::Affected(None), Answer::Fail("no transaction")] {
            let answers = vec![Answer::Fail("no tx"), rollback];
            let (outcome, sql) = run_with(&plan, answers, &CancelFlag::new(), None);
            match outcome {
                ApplyOutcome::NotStarted { message } => assert!(message.contains("no tx")),
                other => panic!("{other:?}"),
            }
            assert_eq!(sql, ["BEGIN", "ROLLBACK"]);
        }
        // A cancel reported for a BEGIN that completed is handled the same way.
        let (outcome, sql) = run_with(
            &plan,
            vec![Answer::Cancelled, Answer::Affected(None)],
            &CancelFlag::new(),
            None,
        );
        assert!(matches!(outcome, ApplyOutcome::NotStarted { .. }));
        assert_eq!(sql, ["BEGIN", "ROLLBACK"]);
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
