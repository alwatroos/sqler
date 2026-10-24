// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The guard: classify one statement, decide whether it needs an approval and collect every stage before it runs.

use sqler_driver_api::sqler_core::{Environment, Value};
use sqler_sql::{Dialect, StatementClass, Token, TokenKind, classify_each, tokenize};

use super::convert::clip;
use super::service::{
    ApprovalStage, Decision, PluginConnection, PluginData, WriteReason, WriteRequest,
};
use crate::discovery::DiscoveredPlugin;
use crate::error::ErrorCode;
use crate::runtime::plugin_error;
use crate::runtime::wit::types::PluginError;

/// The one statement of a call, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planned {
    /// The statement text as sent to the driver (no terminator).
    pub sql: String,
    /// The classifier's verdict.
    pub class: StatementClass,
    /// Why approval is needed; `None` for a plain read.
    pub reason: Option<WriteReason>,
}

impl Planned {
    /// Whether this statement may only run after an approval. The one rule: free only when it is a plain `Read` with
    /// no reason. Nothing may look at `class` or `reason` alone.
    pub(crate) fn needs_approval(&self) -> bool {
        self.class != StatementClass::Read || self.reason.is_some()
    }
}

fn reason_of(class: StatementClass) -> Option<WriteReason> {
    match class {
        StatementClass::Read => None,
        StatementClass::Other => Some(WriteReason::Other),
        StatementClass::Write => Some(WriteReason::Write),
        StatementClass::Ddl => Some(WriteReason::Ddl),
        _ => Some(WriteReason::Unknown),
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

/// Whether the statement is an `EXPLAIN` that also runs the query: `ANALYZE` or `ANALYSE` appears among its tokens, as
/// a bare option or inside the parenthesised list. Deliberately wide: a column named `analyze` inside the explained
/// query also matches, which only costs one approval. An executable comment (`/*! ... */`) inside an `EXPLAIN` counts
/// too.
///
/// This is a deliberate copy of `explain_runs_query` in `sqler-ai-tools/src/guard.rs`: that function is private and
/// neither `sqler-sql` nor `sqler-ai-tools` may change here. Keep the two in step.
pub(crate) fn explain_runs_query(dialect: Dialect, text: &str) -> bool {
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

/// Splits and classifies `sql` for the connection's driver. Exactly one statement is allowed.
///
/// With a dialect the statement is the segment body. Without one (mssql, duckdb) the trimmed text is one `Unknown`
/// statement. Anything but a plain read, and an `EXPLAIN` that runs its query, needs approval.
///
/// # Errors
/// `invalid-argument` for no statement or more than one.
pub(crate) fn plan(conn: &PluginConnection, sql: &str) -> Result<Planned, PluginError> {
    let dialect = conn.dialect;
    let mut statements: Vec<(String, StatementClass)> = if let Some(dialect) = dialect {
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
    if statements.len() != 1 {
        let message = if statements.is_empty() {
            "`sql` has no statement"
        } else {
            "`sql` must be exactly one statement"
        };
        return Err(plugin_error(ErrorCode::InvalidArgument, message));
    }
    let (sql, class) = statements.remove(0);
    let analyze = dialect
        .is_some_and(|d| explain_runs_query(d, &sql))
        .then_some(WriteReason::ExplainAnalyze);
    Ok(Planned {
        reason: reason_of(class).or(analyze),
        sql,
        class,
    })
}

/// Checks a statement that needs approval against the connection and asks for every stage. Nothing runs before this
/// returns `Ok`.
///
/// # Errors
/// `declined` for a read-only connection, no approver, or any answer but `Approve`.
pub(crate) async fn approve(
    data: &PluginData,
    plugin: &DiscoveredPlugin,
    conn: &PluginConnection,
    planned: &Planned,
    params: &[Value],
) -> Result<(), PluginError> {
    let declined = |why: String| plugin_error(ErrorCode::Declined, why);
    if conn.read_only {
        return Err(declined(format!(
            "connection `{}` is read-only",
            clip(&conn.id)
        )));
    }
    let Some(approver) = data.approver() else {
        return Err(declined(
            "this statement needs the user's approval and the host has no approver".to_owned(),
        ));
    };
    let reason = planned.reason.unwrap_or(WriteReason::Unknown);
    let mut stages = vec![ApprovalStage::Confirm];
    if conn.environment == Some(Environment::Prod) {
        stages.push(ApprovalStage::ProdSecond);
    }
    for stage in stages {
        let request = WriteRequest {
            plugin: plugin.manifest.id().clone(),
            plugin_name: plugin.manifest.name().to_owned(),
            connection: conn.clone(),
            sql: planned.sql.clone(),
            params: params.to_vec(),
            class: planned.class,
            reason,
            stage,
        };
        if approver.approve(&request).await != Decision::Approve {
            return Err(declined("the user declined the statement".to_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_driver_api::sqler_core::DriverId;

    fn conn(driver: &str) -> PluginConnection {
        PluginConnection::new("c", "c", DriverId::new(driver).unwrap(), None)
    }

    fn code(e: &PluginError) -> ErrorCode {
        e.code.into()
    }

    /// The text of `fn explain_runs_query` (signature and body), CRLF and whitespace normalised, without a
    /// `pub(crate)` prefix.
    fn function_text(source: &str) -> String {
        let source = source.replace("\r\n", "\n");
        let start = source
            .find("fn explain_runs_query(")
            .expect("the function is in the source");
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("the function ends");
        rest[..end + 2]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn explain_runs_query_is_the_same_as_the_ai_tools_guard() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../sqler-ai-tools/src/guard.rs");
        let Ok(theirs) = std::fs::read_to_string(&path) else {
            eprintln!(
                "skipped: {} is not readable (the crate is built outside the workspace)",
                path.display()
            );
            return;
        };
        let ours = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/query/guard.rs"),
        )
        .expect("this file is readable");
        assert_eq!(
            function_text(&ours),
            function_text(&theirs),
            "explain_runs_query drifted from sqler-ai-tools/src/guard.rs; keep the two copies in step"
        );
    }

    #[test]
    fn one_statement_is_classified() {
        let p = plan(&conn("sqlite"), "select 1;").unwrap();
        assert_eq!(p.sql, "select 1");
        assert!(!p.needs_approval());
        let p = plan(&conn("sqlite"), "insert into t values (1)").unwrap();
        assert_eq!(p.reason, Some(WriteReason::Write));
        let p = plan(&conn("sqlite"), "drop table t").unwrap();
        assert_eq!(p.reason, Some(WriteReason::Ddl));
        let p = plan(&conn("postgres"), "begin").unwrap();
        assert_eq!(p.reason, Some(WriteReason::Other));
    }

    #[test]
    fn dialect_less_is_one_unknown_statement() {
        for driver in ["mssql", "duckdb"] {
            let p = plan(&conn(driver), "  select 1; select 2  ").unwrap();
            assert_eq!(p.sql, "select 1; select 2");
            assert_eq!(p.class, StatementClass::Unknown);
            assert!(p.needs_approval());
        }
    }

    fn tsql(sql: &str) -> Result<Planned, PluginError> {
        plan(
            &conn("sqlserver").with_dialect(Some(sqler_sql::Dialect::TSql)),
            sql,
        )
    }

    #[test]
    fn a_declared_tsql_dialect_classifies_statements() {
        let p = tsql("select top (5) * from [dbo].[t]").unwrap();
        assert!(!p.needs_approval());
        assert_eq!(p.sql, "select top (5) * from [dbo].[t]");
        let p = tsql("update t set a = 1").unwrap();
        assert_eq!(p.reason, Some(WriteReason::Write));
        for text in [
            "exec p",
            "execute('drop table t')",
            "exec sp_executesql N'delete from t'",
        ] {
            let p = tsql(text).unwrap();
            assert_eq!(p.class, StatementClass::Unknown, "{text}");
            assert!(p.needs_approval(), "{text}");
        }
        // `GO` is a separator: a read followed by a write is two statements, not one read.
        let e = tsql("select 1\nGO\ndelete from t").unwrap_err();
        assert_eq!(code(&e), ErrorCode::InvalidArgument);
        // Without the declared dialect the same read needs approval.
        let p = plan(&conn("sqlserver"), "select 1").unwrap();
        assert_eq!(p.class, StatementClass::Unknown);
    }

    #[test]
    fn none_or_many_statements_are_invalid() {
        for text in ["", "  \n", "-- only a comment", ";", "select 1; select 2"] {
            let e = plan(&conn("sqlite"), text).unwrap_err();
            assert_eq!(code(&e), ErrorCode::InvalidArgument, "{text:?}");
        }
        let e = plan(&conn("mssql"), " ").unwrap_err();
        assert_eq!(code(&e), ErrorCode::InvalidArgument);
    }

    // Ported from `sqler-ai-tools/src/guard.rs`.
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
                let p = plan(&conn(driver), text).unwrap();
                assert_eq!(
                    p.reason,
                    Some(WriteReason::ExplainAnalyze),
                    "[{driver}] {text:?}"
                );
                assert!(p.needs_approval());
            }
        }
    }

    #[test]
    fn mysql_executable_comment_in_explain_needs_approval() {
        let p = plan(&conn("mysql"), "EXPLAIN /*! ANALYZE */ select 1").unwrap();
        assert!(p.needs_approval());
    }

    #[test]
    fn explain_without_analyze_stays_a_read() {
        for driver in ["postgres", "mysql"] {
            for text in ["EXPLAIN select 1", "EXPLAIN (FORMAT JSON) select 1"] {
                let p = plan(&conn(driver), text).unwrap();
                assert!(!p.needs_approval(), "[{driver}] {text:?}");
            }
        }
    }
}
