// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The guard: split and classify SQL, then collect every approval before anything runs.

use sqler_sql::{Dialect, StatementClass, Token, TokenKind, classify_each, tokenize};

use crate::approve::{ApprovalReason, ApprovalRequest, ApprovalStage, Approver, Decision};
use crate::error::ToolError;
use crate::source::ConnectionInfo;
use sqler_driver_api::sqler_core::Environment;

/// One statement of a call, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    /// The statement text as sent to the driver (no terminator).
    pub sql: String,
    /// The classifier's verdict.
    pub class: StatementClass,
    /// Why approval is needed; `None` for a plain read.
    pub reason: Option<ApprovalReason>,
}

fn reason_of(class: StatementClass) -> Option<ApprovalReason> {
    match class {
        StatementClass::Read => None,
        StatementClass::Other => Some(ApprovalReason::Other),
        StatementClass::Write => Some(ApprovalReason::Write),
        StatementClass::Ddl => Some(ApprovalReason::Ddl),
        _ => Some(ApprovalReason::Unknown),
    }
}

/// The tokens that carry meaning: whitespace and plain comments dropped.
fn significant(dialect: Dialect, text: &str) -> Vec<Token> {
    tokenize(dialect, text)
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment { .. }
            )
        })
        .collect()
}

/// Whether `text` starts with `EXPLAIN`, `ANALYZE`, `ANALYSE` or `(`, comments and whitespace
/// aside.
pub(crate) fn starts_with_explain_like(dialect: Dialect, text: &str) -> bool {
    significant(dialect, text).first().is_some_and(|t| {
        let word = t.text(text);
        word == "("
            || ["EXPLAIN", "DESCRIBE", "DESC", "ANALYZE", "ANALYSE"]
                .iter()
                .any(|w| t.kind == TokenKind::Word && word.eq_ignore_ascii_case(w))
    })
}

/// Whether the statement is an `EXPLAIN` that also runs the query: `ANALYZE` or `ANALYSE`
/// appears among its tokens, as a bare option or inside the parenthesised list. This is
/// deliberately wide: a column named `analyze` inside the explained query also matches, which
/// only costs one approval. An executable comment (`/*! ... */`) inside an `EXPLAIN` counts too.
fn explain_runs_query(dialect: Dialect, text: &str) -> bool {
    let tokens = significant(dialect, text);
    let leads = tokens.first().is_some_and(|t| {
        t.kind == TokenKind::Word
            && ["EXPLAIN", "DESCRIBE", "DESC"]
                .iter()
                .any(|w| t.text(text).eq_ignore_ascii_case(w))
    });
    leads
        && tokens.iter().skip(1).any(|t| {
            matches!(t.kind, TokenKind::ExecComment { .. })
                || (t.kind == TokenKind::Word
                    && ["ANALYZE", "ANALYSE"]
                        .iter()
                        .any(|w| t.text(text).eq_ignore_ascii_case(w)))
        })
}

/// Splits and classifies `sql` for the connection's driver.
///
/// With a dialect, each statement is classified on its own. Without one (mssql, duckdb) the
/// trimmed text is one `Unknown` statement. `force` makes every statement need approval for
/// that reason, whatever its class. An `EXPLAIN` with `ANALYZE` among its options always needs
/// [`ApprovalReason::ExplainAnalyze`] (unless a stricter reason applies), whichever tool sent it.
pub(crate) fn plan(
    info: &ConnectionInfo,
    sql: &str,
    force: Option<ApprovalReason>,
) -> Result<Vec<Planned>, ToolError> {
    let dialect = Dialect::from_driver_id(info.driver.as_str());
    let statements: Vec<(String, StatementClass)> = if let Some(dialect) = dialect {
        classify_each(dialect, sql)
            .into_iter()
            .map(|(segment, class)| (sql.get(segment.body).unwrap_or_default().to_owned(), class))
            .collect()
    } else {
        let text = sql.trim();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![(text.to_owned(), StatementClass::Unknown)]
        }
    };
    if statements.is_empty() {
        return Err(ToolError::InvalidArguments(
            "`sql` has no statement".to_owned(),
        ));
    }
    Ok(statements
        .into_iter()
        .map(|(sql, class)| {
            let analyze = dialect
                .is_some_and(|d| explain_runs_query(d, &sql))
                .then_some(ApprovalReason::ExplainAnalyze);
            Planned {
                reason: force.or_else(|| reason_of(class)).or(analyze),
                sql,
                class,
            }
        })
        .collect())
}

/// Asks for every approval the plan needs, in statement order, before anything runs.
///
/// Returns, per statement, whether it was explicitly approved. A statement that needs approval
/// on a read-only connection gives [`ToolError::ReadOnly`]; with no approver,
/// [`ToolError::NoApprover`]; any `Deny` gives [`ToolError::Denied`]. On `prod` each statement
/// is asked twice: [`ApprovalStage::Confirm`], then [`ApprovalStage::ProdSecond`].
pub(crate) async fn approve_all(
    tool: &'static str,
    approver: Option<&dyn Approver>,
    info: &ConnectionInfo,
    plan: &[Planned],
) -> Result<Vec<bool>, ToolError> {
    let mut granted = Vec::with_capacity(plan.len());
    for (index, statement) in plan.iter().enumerate() {
        let Some(reason) = statement.reason else {
            granted.push(false);
            continue;
        };
        if info.read_only {
            return Err(ToolError::ReadOnly);
        }
        let Some(approver) = approver else {
            return Err(ToolError::NoApprover);
        };
        let mut stages = vec![ApprovalStage::Confirm];
        if info.environment == Some(Environment::Prod) {
            stages.push(ApprovalStage::ProdSecond);
        }
        for stage in stages {
            let request = ApprovalRequest {
                tool,
                connection: info.clone(),
                sql: statement.sql.clone(),
                class: statement.class,
                reason,
                stage,
                index,
                count: plan.len(),
            };
            if approver.approve(&request).await != Decision::Approve {
                return Err(ToolError::Denied { index });
            }
        }
        granted.push(true);
    }
    Ok(granted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;

    fn info(driver: &str) -> ConnectionInfo {
        ConnectionInfo::new("c", "c", DriverId::new(driver).unwrap(), None)
    }

    #[test]
    fn classifies_each_statement() {
        let p = plan(
            &info("sqlite"),
            "select 1; insert into t values (1); drop table t;",
            None,
        )
        .unwrap();
        let classes: Vec<_> = p.iter().map(|s| s.class).collect();
        assert_eq!(
            classes,
            [
                StatementClass::Read,
                StatementClass::Write,
                StatementClass::Ddl
            ]
        );
        assert_eq!(p[0].sql, "select 1");
        assert_eq!(p[0].reason, None);
        assert_eq!(p[1].reason, Some(ApprovalReason::Write));
        assert_eq!(p[2].reason, Some(ApprovalReason::Ddl));
    }

    #[test]
    fn dialect_less_is_one_unknown_statement() {
        for driver in ["mssql", "duckdb"] {
            let p = plan(&info(driver), "  select 1; select 2  ", None).unwrap();
            assert_eq!(p.len(), 1);
            assert_eq!(p[0].sql, "select 1; select 2");
            assert_eq!(p[0].class, StatementClass::Unknown);
            assert_eq!(p[0].reason, Some(ApprovalReason::Unknown));
        }
    }

    #[test]
    fn empty_input_is_invalid() {
        for text in ["", "  \n", "-- only a comment", ";"] {
            let err = plan(&info("sqlite"), text, None).unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{text:?}");
        }
        for text in ["", "  \n"] {
            let err = plan(&info("mssql"), text, None).unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments(_)), "{text:?}");
        }
    }

    #[test]
    fn transaction_control_is_other() {
        let p = plan(&info("postgres"), "begin", None).unwrap();
        assert_eq!(p[0].reason, Some(ApprovalReason::Other));
    }

    #[test]
    fn explain_analyze_forces_approval_on_postgres() {
        let text = "EXPLAIN ANALYZE select 1";
        let plain = plan(&info("postgres"), text, None).unwrap();
        assert_eq!(plain[0].class, StatementClass::Read);
        assert_eq!(plain[0].reason, Some(ApprovalReason::ExplainAnalyze));
        let bare = plan(&info("postgres"), "EXPLAIN select 1", None).unwrap();
        assert_eq!(bare[0].reason, None);
        let forced = plan(
            &info("postgres"),
            text,
            Some(ApprovalReason::ExplainAnalyze),
        )
        .unwrap();
        assert_eq!(forced[0].reason, Some(ApprovalReason::ExplainAnalyze));
        assert_eq!(forced[0].class, StatementClass::Read);
    }

    #[test]
    fn explain_analyze_in_any_spelling_needs_approval() {
        let cases = [
            "EXPLAIN ANALYZE select 1",
            "explain analyse select 1",
            "EXPLAIN (ANALYZE) select 1",
            "explain ( analyze , buffers ) select 1",
            "EXPLAIN (FORMAT JSON, ANALYZE) select 1",
            "EXPLAIN (FORMAT JSON, ANALYZE true) select 1",
            "EXPLAIN/**/ANALYZE select 1",
            "/* x */ EXPLAIN -- c\n ANALYZE VERBOSE select 1",
            "EXPLAIN\n\tANALYZE\nselect 1",
            "EXPLAIN ANALYZE FORMAT=TREE select 1",
            "DESCRIBE ANALYZE select 1",
        ];
        for driver in ["postgres", "mysql"] {
            for text in cases {
                let p = plan(&info(driver), text, None).unwrap();
                assert_eq!(
                    p[0].reason,
                    Some(ApprovalReason::ExplainAnalyze),
                    "[{driver}] {text:?}"
                );
            }
        }
    }

    #[test]
    fn mysql_executable_comment_in_explain_needs_approval() {
        let p = plan(&info("mysql"), "EXPLAIN /*! ANALYZE */ select 1", None).unwrap();
        assert!(p[0].reason.is_some());
    }

    #[test]
    fn explain_without_analyze_stays_a_read() {
        for driver in ["postgres", "mysql"] {
            for text in ["EXPLAIN select 1", "EXPLAIN (FORMAT JSON) select 1"] {
                let p = plan(&info(driver), text, None).unwrap();
                assert_eq!(p[0].reason, None, "[{driver}] {text:?}");
            }
        }
    }

    #[test]
    fn explain_like_prefix_detection() {
        for text in [
            "ANALYZE select 1",
            "(ANALYZE) select 1",
            "  /* c */ explain select 1",
            "analyse t",
        ] {
            assert!(
                starts_with_explain_like(Dialect::Postgres, text),
                "{text:?}"
            );
        }
        assert!(!starts_with_explain_like(Dialect::Postgres, "select 1"));
    }
}
