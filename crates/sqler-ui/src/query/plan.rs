// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! From a buffer snapshot to the statements to run.

use std::ops::Range;

use sqler_sql::{Dialect, Placeholder, PlaceholderKind, SegmentKind, StatementClass};
use sqler_sql::{classify, placeholders, split, statement_at};
use thiserror::Error;

use super::buffer::{BufferSnapshot, RunMode};
use super::params::ParamKey;

/// Why nothing can run.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PlanError {
    /// The buffer (or selection) holds no statement.
    #[error("No statement to run")]
    NothingToRun,
}

/// One statement to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStatement {
    /// Position in the plan, from 0.
    pub index: usize,
    /// The body range, in buffer offsets.
    pub range: Range<usize>,
    /// The exact body text.
    pub sql: String,
    /// How dangerous the statement is.
    pub class: StatementClass,
    /// The placeholders; ranges are relative to `sql`.
    pub placeholders: Vec<Placeholder>,
}

impl PlannedStatement {
    /// The parameter key of each placeholder, in the order of `placeholders`.
    ///
    /// Each `?` is its own key: `ordinal` counts the anonymous placeholders of this statement from 0.
    #[must_use]
    pub fn keys(&self) -> Vec<ParamKey> {
        let mut ordinal = 0;
        self.placeholders
            .iter()
            .map(|p| match &p.kind {
                PlaceholderKind::Positional(n) => ParamKey::Positional(*n),
                PlaceholderKind::Named(name) => ParamKey::Named(name.clone()),
                _ => {
                    let key = ParamKey::Anonymous {
                        statement: self.index,
                        ordinal,
                    };
                    ordinal += 1;
                    key
                }
            })
            .collect()
    }
}

/// The statements of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The statements, in execution order.
    pub statements: Vec<PlannedStatement>,
    /// The distinct parameter keys, in order of first appearance.
    pub params: Vec<ParamKey>,
}

/// Plans a run of `buf` in `mode`.
///
/// `Selection` runs the selected text (an empty selection acts like `Statement`); `Statement`
/// runs the selected text when there is a selection, else the statement under the cursor, up to
/// its `;`; `All` runs every statement. A selection runs only its own text, split at `;`, the
/// `MySQL` delimiter and T-SQL `GO`. Delimiter lines and comment-only pieces never run. Without
/// a `dialect` the trimmed text is one statement of class `Unknown` with no placeholders.
///
/// A blank line does not end a statement.
///
/// # Errors
/// [`PlanError::NothingToRun`] when there is no statement.
pub fn plan(
    dialect: Option<Dialect>,
    buf: &BufferSnapshot,
    mode: RunMode,
) -> Result<Plan, PlanError> {
    let sel = buf.selection.clone();
    let selected = (mode != RunMode::All && sel.start < sel.end)
        .then(|| buf.text.get(sel.clone()))
        .flatten();
    let (text, base) = match selected {
        Some(text) => (text, sel.start),
        None => (buf.text.as_str(), 0),
    };
    let mode = match (selected.is_some(), mode) {
        (true, RunMode::Statement) => RunMode::Selection,
        (false, RunMode::Selection) => RunMode::Statement,
        _ => mode,
    };
    let ranges = match dialect {
        Some(d) => segment_ranges(d, text, base, buf.cursor, mode),
        None => trimmed_range(text, base).into_iter().collect(),
    };
    let mut statements = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.into_iter().enumerate() {
        let sql = buf
            .text
            .get(range.clone())
            .ok_or(PlanError::NothingToRun)?
            .to_owned();
        let (class, placeholders) = match dialect {
            Some(d) => (classify(d, &sql), placeholders(d, &sql)),
            None => (StatementClass::Unknown, Vec::new()),
        };
        statements.push(PlannedStatement {
            index,
            range,
            sql,
            class,
            placeholders,
        });
    }
    if statements.is_empty() {
        return Err(PlanError::NothingToRun);
    }
    let mut params: Vec<ParamKey> = Vec::new();
    for key in statements.iter().flat_map(PlannedStatement::keys) {
        if !params.contains(&key) {
            params.push(key);
        }
    }
    Ok(Plan { statements, params })
}

fn segment_ranges(
    dialect: Dialect,
    text: &str,
    base: usize,
    cursor: usize,
    mode: RunMode,
) -> Vec<Range<usize>> {
    let segments = split(dialect, text);
    let shift = |r: &Range<usize>| r.start + base..r.end + base;
    match mode {
        RunMode::All | RunMode::Selection => segments
            .iter()
            .filter(|s| s.kind == SegmentKind::Statement)
            .map(|s| shift(&s.body))
            .collect(),
        _ => statement_at(&segments, cursor.saturating_sub(base))
            .map(|s| shift(&s.body))
            .into_iter()
            .collect(),
    }
}

fn trimmed_range(text: &str, base: usize) -> Option<Range<usize>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let start = text.len() - text.trim_start().len();
    Some(base + start..base + start + trimmed.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: Option<Dialect> = Some(Dialect::Postgres);

    fn sqls(p: &Plan) -> Vec<&str> {
        p.statements.iter().map(|s| s.sql.as_str()).collect()
    }

    #[test]
    fn selection_is_never_split_at_blank_lines() {
        let text = "insert into t (a)\n\nselect 1";
        for d in Dialect::ALL {
            for mode in [RunMode::Selection, RunMode::Statement] {
                let b = BufferSnapshot::with_selection(text, 0..text.len());
                let p = plan(Some(d), &b, mode).unwrap();
                assert_eq!(sqls(&p), [text], "{d:?} {mode:?}");
            }
            let b = BufferSnapshot::at_cursor(text, 2);
            let p = plan(Some(d), &b, RunMode::Statement).unwrap();
            assert_eq!(sqls(&p), [text], "{d:?}");
        }
    }

    #[test]
    fn all_runs_every_statement_in_order() {
        let b = BufferSnapshot::at_cursor("select 1; -- c\n\ninsert into t values (1);\n-- end", 0);
        let p = plan(PG, &b, RunMode::All).unwrap();
        // A comment before a statement belongs to its body (T3).
        assert_eq!(sqls(&p), ["select 1", "-- c\n\ninsert into t values (1)"]);
        assert_eq!(p.statements[0].index, 0);
        assert_eq!(p.statements[1].index, 1);
        assert_eq!(p.statements[0].class, StatementClass::Read);
        assert_eq!(p.statements[1].class, StatementClass::Write);
        assert_eq!(&b.text[p.statements[1].range.clone()], p.statements[1].sql);
    }

    #[test]
    fn statement_uses_the_cursor() {
        let text = "select 1;\nselect 2;\nselect 3;";
        let at = |c| plan(PG, &BufferSnapshot::at_cursor(text, c), RunMode::Statement).unwrap();
        assert_eq!(sqls(&at(0)), ["select 1"]);
        assert_eq!(sqls(&at(12)), ["select 2"]);
        assert_eq!(sqls(&at(text.len())), ["select 3"]);
    }

    #[test]
    fn cursor_in_whitespace_or_comment_takes_the_previous_statement() {
        let text = "select 1;\n\n-- note\n\nselect 2;";
        let at = |c| plan(PG, &BufferSnapshot::at_cursor(text, c), RunMode::Statement).unwrap();
        assert_eq!(sqls(&at(10)), ["select 1"]);
        // In the comment in front of a statement the cursor is inside that statement.
        assert_eq!(sqls(&at(13)), ["-- note\n\nselect 2"]);
    }

    #[test]
    fn selection_is_split_on_its_own_text() {
        let text = "select 1; select 2; select 3;";
        let b = BufferSnapshot::with_selection(text, 10..29);
        let p = plan(PG, &b, RunMode::Selection).unwrap();
        assert_eq!(sqls(&p), ["select 2", "select 3"]);
        assert_eq!(p.statements[0].range, 10..18);
        assert_eq!(&text[p.statements[1].range.clone()], "select 3");
    }

    #[test]
    fn selection_may_cut_a_statement() {
        let b = BufferSnapshot::with_selection("select 1, 2 from t", 0..9);
        let p = plan(PG, &b, RunMode::Selection).unwrap();
        assert_eq!(sqls(&p), ["select 1,"]);
    }

    #[test]
    fn empty_selection_behaves_like_statement() {
        let text = "select 1;\nselect 2;";
        let b = BufferSnapshot::at_cursor(text, 12);
        let p = plan(PG, &b, RunMode::Selection).unwrap();
        assert_eq!(sqls(&p), ["select 2"]);
    }

    #[test]
    fn invalid_selection_falls_back_to_statement() {
        let b = BufferSnapshot {
            text: "select 1;".to_owned(),
            selection: 3..99,
            cursor: 0,
        };
        let p = plan(PG, &b, RunMode::Selection).unwrap();
        assert_eq!(sqls(&p), ["select 1"]);
    }

    #[test]
    fn blank_line_is_inside_one_statement() {
        let text = "update t\n\nset x=1";
        for d in Dialect::ALL {
            for cursor in [2, 12] {
                let b = BufferSnapshot::at_cursor(text, cursor);
                let p = plan(Some(d), &b, RunMode::Statement).unwrap();
                assert_eq!(sqls(&p), [text], "{d:?} {cursor}");
                assert_eq!(p.statements[0].class, StatementClass::Write, "{d:?}");
            }
        }
    }

    #[test]
    fn three_line_selection_runs_exactly_those_lines() {
        let lines = [
            "select 1;",
            "select 2;",
            "select 3;",
            "select 4;",
            "select 5;",
        ];
        let text = lines.join("\n");
        let start = lines[0].len() + 1;
        let end = start + lines[1..4].join("\n").len();
        for d in Dialect::ALL {
            for mode in [RunMode::Statement, RunMode::Selection] {
                let b = BufferSnapshot::with_selection(&text, start..end);
                let p = plan(Some(d), &b, mode).unwrap();
                assert_eq!(
                    sqls(&p),
                    ["select 2", "select 3", "select 4"],
                    "{d:?} {mode:?}"
                );
            }
        }
    }

    #[test]
    fn statement_with_selection_runs_the_selection() {
        let text = "select 1;\nselect 2;\nselect 3;";
        for d in Dialect::ALL {
            let b = BufferSnapshot {
                text: text.to_owned(),
                selection: 0..19,
                cursor: 19,
            };
            let a = plan(Some(d), &b, RunMode::Statement).unwrap();
            let s = plan(Some(d), &b, RunMode::Selection).unwrap();
            assert_eq!(a, s, "{d:?}");
            assert_eq!(sqls(&a), ["select 1", "select 2"]);
            // Run All keeps running the whole buffer.
            let all = plan(Some(d), &b, RunMode::All).unwrap();
            assert_eq!(sqls(&all), ["select 1", "select 2", "select 3"]);
        }
    }

    #[test]
    fn statement_with_partial_line_selection() {
        let b = BufferSnapshot {
            text: "select 1, 2 from t".to_owned(),
            selection: 0..9,
            cursor: 9,
        };
        let p = plan(PG, &b, RunMode::Statement).unwrap();
        assert_eq!(sqls(&p), ["select 1,"]);
        assert_eq!(p.statements[0].range, 0..9);
    }

    #[test]
    fn statement_with_invalid_or_empty_selection_uses_the_cursor() {
        let text = "select 1;\nselect 2;";
        let bad = BufferSnapshot {
            text: text.to_owned(),
            selection: 3..99,
            cursor: 12,
        };
        assert_eq!(
            sqls(&plan(PG, &bad, RunMode::Statement).unwrap()),
            ["select 2"]
        );
        let b = BufferSnapshot::at_cursor(text, 12);
        assert_eq!(
            sqls(&plan(PG, &b, RunMode::Statement).unwrap()),
            ["select 2"]
        );
    }

    #[test]
    fn nothing_to_run() {
        for text in ["", "   \n", "-- only a comment", ";"] {
            for mode in [RunMode::Selection, RunMode::Statement, RunMode::All] {
                let b = BufferSnapshot::at_cursor(text, 0);
                assert_eq!(plan(PG, &b, mode), Err(PlanError::NothingToRun), "{text:?}");
                if !text.contains("--") && text != ";" {
                    assert_eq!(plan(None, &b, mode), Err(PlanError::NothingToRun));
                }
            }
        }
        assert_eq!(PlanError::NothingToRun.to_string(), "No statement to run");
    }

    #[test]
    fn mysql_delimiter_lines_never_run() {
        let text = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\nDELIMITER ;\nSELECT 3;";
        let b = BufferSnapshot::at_cursor(text, 0);
        let p = plan(Some(Dialect::MySql), &b, RunMode::All).unwrap();
        assert_eq!(p.statements.len(), 2);
        assert!(p.statements[0].sql.starts_with("CREATE PROCEDURE"));
        assert_eq!(p.statements[1].sql, "SELECT 3");
        assert_eq!(p.params.len(), 0);
    }

    #[test]
    fn no_dialect_is_one_unknown_statement() {
        let text = "  select 1; select :a  \n";
        let b = BufferSnapshot::at_cursor(text, 0);
        for mode in [RunMode::Selection, RunMode::Statement, RunMode::All] {
            let p = plan(None, &b, mode).unwrap();
            assert_eq!(p.statements.len(), 1);
            assert_eq!(p.statements[0].sql, "select 1; select :a");
            assert_eq!(p.statements[0].range, 2..21);
            assert_eq!(p.statements[0].class, StatementClass::Unknown);
            assert_eq!(p.statements[0].placeholders.len(), 0);
            assert_eq!(p.params.len(), 0);
        }
        let b = BufferSnapshot::with_selection("a b c", 2..3);
        let p = plan(None, &b, RunMode::Selection).unwrap();
        assert_eq!(p.statements[0].sql, "b");
        assert_eq!(p.statements[0].range, 2..3);
    }

    #[test]
    fn params_are_distinct_in_first_appearance_order() {
        let b = BufferSnapshot::at_cursor("select :b, :a, :b; select :a, :c", 0);
        let p = plan(PG, &b, RunMode::All).unwrap();
        assert_eq!(
            p.params,
            [
                ParamKey::Named(":b".into()),
                ParamKey::Named(":a".into()),
                ParamKey::Named(":c".into())
            ]
        );
    }

    #[test]
    fn anonymous_placeholders_are_separate_keys() {
        let b = BufferSnapshot::at_cursor("select ?, ?; select ?", 0);
        let p = plan(Some(Dialect::MySql), &b, RunMode::All).unwrap();
        assert_eq!(
            p.params,
            [
                ParamKey::Anonymous {
                    statement: 0,
                    ordinal: 0
                },
                ParamKey::Anonymous {
                    statement: 0,
                    ordinal: 1
                },
                ParamKey::Anonymous {
                    statement: 1,
                    ordinal: 0
                },
            ]
        );
    }

    #[test]
    fn positional_keys_are_shared_across_statements() {
        let b = BufferSnapshot::at_cursor("select $2; select $2, $1", 0);
        let p = plan(PG, &b, RunMode::All).unwrap();
        assert_eq!(p.params, [ParamKey::Positional(2), ParamKey::Positional(1)]);
    }
}
