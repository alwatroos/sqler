// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The apply plan: every statement that will run, in order, and its preview.

use std::fmt::Write as _;

use sqler_driver_api::sqler_core::{Query, Row, Value};
use sqler_sql::{
    BoundDml, Dialect, TxStep, delete_bound, insert_bound, param_marker, transaction_sql,
    update_bound,
};

use super::editability::{EditTarget, RowIdentity};
use super::staged::{ChangeKind, EditError, RowRef, StagedEdits};
use crate::grid::type_label;

/// One data statement of the plan.
///
/// `Debug` prints the kind, the row and counts: the bound values are user data.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct PlannedStatement {
    /// What it does.
    pub kind: ChangeKind,
    /// The row it is for.
    pub row: RowRef,
    /// The SQL and its bound values.
    pub query: Query,
    /// The catalog column each entry of `query.params` is bound for.
    pub param_columns: Vec<String>,
    /// The rows it must affect; any other count rolls the apply back.
    pub expected_rows: u64,
}

impl std::fmt::Debug for PlannedStatement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlannedStatement")
            .field("kind", &self.kind)
            .field("row", &self.row)
            .field("sql_len", &self.query.sql.len())
            .field("params", &self.query.params.len())
            .field("expected_rows", &self.expected_rows)
            .finish_non_exhaustive()
    }
}

/// The complete, ordered list of statements of one apply.
///
/// `apply` executes exactly [`Self::begin`], [`Self::statements`], [`Self::commit`] and, on a
/// failure, [`Self::rollback`], and nothing else.
///
/// `Debug` prints the dialect and the statement count only: statement text and bound values are
/// user data.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct ApplyPlan {
    dialect: Dialect,
    begin: Query,
    statements: Vec<PlannedStatement>,
    commit: Query,
    rollback: Query,
    column_types: Vec<(String, &'static str)>,
}

impl std::fmt::Debug for ApplyPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyPlan")
            .field("dialect", &self.dialect)
            .field("statements", &self.statements.len())
            .finish_non_exhaustive()
    }
}

/// A production plan was not confirmed by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("applying to a production connection needs the user's confirmation")]
pub struct ConfirmationRequired;

/// An [`ApplyPlan`] that went through [`ApplyPlan::confirm`] or
/// [`ApplyPlan::confirm_non_production`]: [`apply`](super::apply()) accepts nothing else, so a
/// caller cannot forget the confirmation step.
///
/// What this does and does not enforce: `confirm` takes the caller's assertions (`production` and
/// `user_confirmed`) as given; the types cannot check that they are true. The types make
/// *forgetting* the step impossible, not *lying* to it, so the caller derives `production` from
/// the connection's environment and `user_confirmed` from a real user step.
///
/// It owns the plan and the confirming methods take the plan by value, so one `ApplyPlan` yields
/// at most one `ConfirmedPlan`, and `apply` consumes that. Applying again (a retry after a
/// failure, or a second apply after `Committed`, which would repeat the `INSERT`s) means
/// building a new plan from the staged edits and confirming it again.
///
/// `Debug` prints the flags and the statement count only.
pub struct ConfirmedPlan {
    plan: ApplyPlan,
    production: bool,
    user_confirmed: bool,
}

impl std::fmt::Debug for ConfirmedPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmedPlan")
            .field("statements", &self.plan.statements.len())
            .field("production", &self.production)
            .field("user_confirmed", &self.user_confirmed)
            .finish()
    }
}

impl ConfirmedPlan {
    /// The plan.
    #[must_use]
    pub fn plan(&self) -> &ApplyPlan {
        &self.plan
    }

    /// The plan, consuming the confirmation (what `apply` uses).
    pub(super) fn into_plan(self) -> ApplyPlan {
        self.plan
    }

    /// Whether the caller asserted that the connection was `prod`.
    #[must_use]
    pub fn production(&self) -> bool {
        self.production
    }

    /// Whether the caller asserted that the user confirmed (always true for a non-empty `prod`
    /// plan).
    #[must_use]
    pub fn user_confirmed(&self) -> bool {
        self.user_confirmed
    }
}

/// When a previewed statement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PreviewWhen {
    /// On every apply.
    Always,
    /// Only when the apply fails.
    OnFailure,
}

/// One bound value of a previewed statement.
///
/// `Debug` prints the marker, the column and the length of the value, never the value.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreviewParam {
    /// The marker in the SQL (`$1`, `?1`, `?`).
    pub marker: String,
    /// The column it is bound for.
    pub column: String,
    /// The value as text, see [`display_value`].
    pub value: String,
    /// The column's type label, see [`type_label`].
    pub type_label: &'static str,
}

impl std::fmt::Debug for PreviewParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewParam")
            .field("marker", &self.marker)
            .field("column", &self.column)
            .field("value_len", &self.value.len())
            .field("type_label", &self.type_label)
            .finish()
    }
}

/// One statement of the preview.
///
/// `Debug` prints counts only.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreviewStatement {
    /// Position in the list, from 1.
    pub ordinal: usize,
    /// The SQL text; no value is part of it.
    pub sql: String,
    /// The bound values, in order.
    pub params: Vec<PreviewParam>,
    /// The rows it must affect, for data statements.
    pub expected_rows: Option<u64>,
    /// When it runs.
    pub when: PreviewWhen,
}

impl std::fmt::Debug for PreviewStatement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewStatement")
            .field("ordinal", &self.ordinal)
            .field("sql_len", &self.sql.len())
            .field("params", &self.params.len())
            .field("expected_rows", &self.expected_rows)
            .field("when", &self.when)
            .finish()
    }
}

/// A value as shown in the preview: `NULL`, text and JSON in Rust debug quoting, bytes as hex with
/// their length, other kinds as their `Display`. User data is not redacted.
#[must_use]
pub fn display_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Text(s) | Value::Json(s) => format!("{s:?}"),
        Value::Bytes(b) => {
            let mut out = String::with_capacity(b.len() * 2 + 16);
            for byte in b {
                let _ = write!(out, "{byte:02x}");
            }
            let _ = write!(out, " ({} bytes)", b.len());
            out
        }
        other => other.to_string(),
    }
}

/// The key (or all) columns of `original` with their values: how the row is found again.
fn filter_of(target: &EditTarget, original: &Row) -> Vec<(String, Value)> {
    let indices: Vec<usize> = match target.identity() {
        RowIdentity::Key { columns } => columns.clone(),
        _ => (0..target.width()).collect(),
    };
    indices
        .into_iter()
        .filter_map(|i| {
            original
                .get(i)
                .map(|v| (target.table_column(i).to_owned(), v.clone()))
        })
        .collect()
}

fn pairs(list: &[(String, Value)]) -> Vec<(&str, &Value)> {
    list.iter().map(|(c, v)| (c.as_str(), v)).collect()
}

impl ApplyPlan {
    pub(super) fn build(edits: &StagedEdits) -> Result<Self, EditError> {
        if edits.is_empty() {
            return Err(EditError::NothingStaged);
        }
        let target = edits.target();
        let dialect = target.dialect();
        let table = target.table();
        let mut statements = Vec::with_capacity(edits.change_count());

        for (&row, original) in &edits.deletes {
            let filter = filter_of(target, original);
            let bound =
                delete_bound(dialect, table, &pairs(&filter)).map_err(|error| EditError::Dml {
                    row: RowRef::Model(row),
                    error,
                })?;
            statements.push(statement(ChangeKind::Delete, RowRef::Model(row), bound));
        }
        for (&row, entry) in &edits.updates {
            let filter = filter_of(target, &entry.original);
            let set: Vec<(String, Value)> = entry
                .cells
                .iter()
                .map(|(&c, v)| (target.table_column(c).to_owned(), v.clone()))
                .collect();
            let bound =
                update_bound(dialect, table, &pairs(&set), &pairs(&filter)).map_err(|error| {
                    EditError::Dml {
                        row: RowRef::Model(row),
                        error,
                    }
                })?;
            statements.push(statement(ChangeKind::Update, RowRef::Model(row), bound));
        }
        for (id, cells) in &edits.inserts {
            let values: Vec<(String, Value)> = cells
                .iter()
                .enumerate()
                .filter_map(|(i, v)| v.clone().map(|v| (target.table_column(i).to_owned(), v)))
                .collect();
            let bound =
                insert_bound(dialect, table, &pairs(&values)).map_err(|error| EditError::Dml {
                    row: RowRef::New(*id),
                    error,
                })?;
            statements.push(statement(ChangeKind::Insert, RowRef::New(*id), bound));
        }

        Ok(Self {
            dialect,
            begin: Query::new(transaction_sql(dialect, TxStep::Begin)),
            statements,
            commit: Query::new(transaction_sql(dialect, TxStep::Commit)),
            rollback: Query::new(transaction_sql(dialect, TxStep::Rollback)),
            column_types: table
                .columns
                .iter()
                .map(|c| (c.name.clone(), type_label(c.data_type)))
                .collect(),
        })
    }

    /// The statement that starts the transaction.
    #[must_use]
    pub fn begin(&self) -> &Query {
        &self.begin
    }

    /// The data statements, in the order they run.
    #[must_use]
    pub fn statements(&self) -> &[PlannedStatement] {
        &self.statements
    }

    /// The statement that commits.
    #[must_use]
    pub fn commit(&self) -> &Query {
        &self.commit
    }

    /// The statement that undoes a failed apply.
    #[must_use]
    pub fn rollback(&self) -> &Query {
        &self.rollback
    }

    /// Whether the caller must obtain the user's confirmation before applying.
    ///
    /// Pass whether the connection's environment is `prod` (ADR-0024). Every plan consists of
    /// writes, so this is `production` for a non-empty plan. Show [`Self::render_preview`] when it
    /// is true, then call [`Self::confirm`]: [`apply`](super::apply()) takes only a
    /// [`ConfirmedPlan`], so the confirmation step cannot be forgotten. The flag is the caller's
    /// assertion; nothing here can verify it.
    #[must_use]
    pub fn needs_confirmation(&self, production: bool) -> bool {
        production && !self.statements.is_empty()
    }

    /// The way to get a [`ConfirmedPlan`] for a connection that may be `prod`; it consumes the
    /// plan.
    ///
    /// `production` is whether the caller says the connection's environment is `prod`;
    /// `user_confirmed` is whether the caller says the user saw [`Self::render_preview`] and
    /// agreed. Both are taken as given. A production plan is confirmed only with
    /// `user_confirmed`.
    ///
    /// # Errors
    ///
    /// [`ConfirmationRequired`] when `production` is true and `user_confirmed` is false. The plan
    /// is dropped then; build it again from the staged edits after the user agrees.
    pub fn confirm(
        self,
        production: bool,
        user_confirmed: bool,
    ) -> Result<ConfirmedPlan, ConfirmationRequired> {
        if self.needs_confirmation(production) && !user_confirmed {
            return Err(ConfirmationRequired);
        }
        Ok(ConfirmedPlan {
            plan: self,
            production,
            user_confirmed,
        })
    }

    /// A [`ConfirmedPlan`] for a connection the caller says is not `prod`; it consumes the plan.
    ///
    /// Use it only when the connection's environment is known not to be `prod`; for any other
    /// connection call [`Self::confirm`] with the real flag. This is the same as
    /// `confirm(false, false)` and cannot fail.
    #[must_use]
    pub fn confirm_non_production(self) -> ConfirmedPlan {
        ConfirmedPlan {
            plan: self,
            production: false,
            user_confirmed: false,
        }
    }

    /// The statements in order with their bound values: `BEGIN`, the data statements and `COMMIT`
    /// (always), then `ROLLBACK` (on failure).
    #[must_use]
    pub fn preview(&self) -> Vec<PreviewStatement> {
        let mut out = Vec::with_capacity(self.statements.len() + 3);
        let mut push = |query: &Query, columns: &[String], expected, when| {
            let params = query
                .params
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let column = columns.get(i).cloned().unwrap_or_default();
                    let label = self
                        .column_types
                        .iter()
                        .find(|(n, _)| *n == column)
                        .map_or("other", |(_, l)| *l);
                    PreviewParam {
                        marker: param_marker(self.dialect, i + 1),
                        column,
                        value: display_value(v),
                        type_label: label,
                    }
                })
                .collect();
            out.push(PreviewStatement {
                ordinal: out.len() + 1,
                sql: query.sql.clone(),
                params,
                expected_rows: expected,
                when,
            });
        };
        push(&self.begin, &[], None, PreviewWhen::Always);
        for s in &self.statements {
            push(
                &s.query,
                &s.param_columns,
                Some(s.expected_rows),
                PreviewWhen::Always,
            );
        }
        push(&self.commit, &[], None, PreviewWhen::Always);
        push(&self.rollback, &[], None, PreviewWhen::OnFailure);
        out
    }

    /// [`Self::preview`] as plain text, values listed under their statement.
    #[must_use]
    pub fn render_preview(&self) -> String {
        let mut out = String::new();
        for s in self.preview() {
            let _ = writeln!(out, "{}. {}", s.ordinal, s.sql);
            for p in &s.params {
                let _ = writeln!(
                    out,
                    "     {} {} ({}) = {}",
                    p.marker, p.column, p.type_label, p.value
                );
            }
            if let Some(n) = s.expected_rows {
                let _ = writeln!(out, "     must affect exactly {n} row");
            }
            if s.when == PreviewWhen::OnFailure {
                let _ = writeln!(out, "     (runs only if the apply fails)");
            }
        }
        out
    }
}

/// The generator's own statement and parameter columns: the preview never re-derives either.
fn statement(kind: ChangeKind, row: RowRef, bound: BoundDml) -> PlannedStatement {
    PlannedStatement {
        kind,
        row,
        query: bound.query,
        param_columns: bound.columns,
        expected_rows: 1,
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::Value;

    use super::*;
    use crate::grid::edit::ModelRow;
    use crate::grid::edit::testing::{row, target};

    #[test]
    fn debug_never_shows_values_sql_or_keys() {
        let mut s = StagedEdits::new(target(false));
        let n = s.add_row();
        s.set_new_cell(n, 0, Some(Value::Int(616_161))).unwrap();
        s.set_new_cell(n, 1, Some(Value::Text("sekrit-ins".into())))
            .unwrap();
        s.set_cell(
            ModelRow(5),
            &row(424_242, Some("sekrit-old")),
            1,
            Value::Text("sekrit-new".into()),
        )
        .unwrap();
        s.delete_row(ModelRow(9), &row(515_151, Some("sekrit-del")))
            .unwrap();
        let plan = s.plan().unwrap();
        let preview = plan.preview();
        let mut shown = format!("{plan:?} {:?} {preview:?}", plan.statements());
        for st in &preview {
            let _ = write!(shown, "{st:?} {:?}", st.params);
        }
        let confirmed = plan.clone().confirm(true, true).unwrap();
        let _ = write!(shown, "{confirmed:?}");
        for secret in [
            "sekrit", "424242", "515151", "616161", "UPDATE", "DELETE", "INSERT",
        ] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
        assert!(shown.contains("statements: 3"), "{shown}");
        assert!(shown.contains("production: true"), "{shown}");
    }

    #[test]
    fn display_values() {
        assert_eq!(display_value(&Value::Null), "NULL");
        assert_eq!(display_value(&Value::Text("a\"b".into())), "\"a\\\"b\"");
        assert_eq!(display_value(&Value::Json("{}".into())), "\"{}\"");
        assert_eq!(
            display_value(&Value::Bytes(vec![0, 255, 16])),
            "00ff10 (3 bytes)"
        );
        assert_eq!(display_value(&Value::Int(-4)), "-4");
        assert_eq!(display_value(&Value::Bool(true)), "true");
    }

    #[test]
    fn order_is_delete_update_insert_and_filters_use_originals() {
        let mut s = StagedEdits::new(target(false));
        let n = s.add_row();
        s.set_new_cell(n, 0, Some(Value::Int(10))).unwrap();
        s.set_new_cell(n, 1, Some(Value::Null)).unwrap();
        s.set_cell(ModelRow(5), &row(7, Some("a")), 0, Value::Int(70))
            .unwrap();
        s.set_cell(ModelRow(5), &row(7, Some("a")), 1, Value::Null)
            .unwrap();
        s.set_cell(ModelRow(2), &row(3, None), 1, Value::Text("x".into()))
            .unwrap();
        s.delete_row(ModelRow(9), &row(8, Some("z"))).unwrap();
        let plan = s.plan().unwrap();
        let kinds: Vec<_> = plan.statements().iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            [
                ChangeKind::Delete,
                ChangeKind::Update,
                ChangeKind::Update,
                ChangeKind::Insert
            ]
        );
        let sql: Vec<_> = plan
            .statements()
            .iter()
            .map(|p| p.query.sql.as_str())
            .collect();
        assert_eq!(sql[0], "DELETE FROM \"main\".\"t\" WHERE \"id\" = ?1");
        assert_eq!(
            sql[1],
            "UPDATE \"main\".\"t\" SET \"name\" = ?1 WHERE \"id\" = ?2"
        );
        assert_eq!(
            sql[2],
            "UPDATE \"main\".\"t\" SET \"id\" = ?1, \"name\" = ?2 WHERE \"id\" = ?3"
        );
        assert_eq!(
            sql[3],
            "INSERT INTO \"main\".\"t\" (\"id\", \"name\") VALUES (?1, ?2)"
        );
        let u = &plan.statements()[2];
        assert_eq!(u.query.params, [Value::Int(70), Value::Null, Value::Int(7)]);
        assert_eq!(u.param_columns, ["id", "name", "id"]);
        assert!(plan.statements().iter().all(|p| p.expected_rows == 1));
        assert_eq!(plan.begin().sql, "BEGIN");
        assert_eq!(plan.commit().sql, "COMMIT");
        assert_eq!(plan.rollback().sql, "ROLLBACK");
    }

    #[test]
    fn keyless_filter_uses_all_columns_with_is_null() {
        let mut s = StagedEdits::new(target(true));
        s.set_cell(ModelRow(0), &row(1, None), 1, Value::Text("x".into()))
            .unwrap();
        let plan = s.plan().unwrap();
        let q = &plan.statements()[0].query;
        assert_eq!(
            q.sql,
            "UPDATE \"main\".\"t\" SET \"name\" = ?1 WHERE \"id\" = ?2 AND \"name\" IS NULL"
        );
        assert_eq!(q.params, [Value::Text("x".into()), Value::Int(1)]);
        assert_eq!(plan.statements()[0].param_columns, ["name", "id"]);
    }

    #[test]
    fn empty_new_row_is_refused_not_defaulted() {
        let mut s = StagedEdits::new(target(false));
        let id = s.add_row();
        match s.plan().unwrap_err() {
            EditError::Dml { row, error } => {
                assert_eq!(row, RowRef::New(id));
                assert_eq!(error, sqler_sql::DmlError::EmptyInsert);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn values_never_appear_in_sql_and_preview_lists_them_apart() {
        let mut s = StagedEdits::new(target(false));
        s.set_cell(
            ModelRow(0),
            &row(1, Some("a")),
            1,
            Value::Text("x'; DROP TABLE t; --".into()),
        )
        .unwrap();
        let plan = s.plan().unwrap();
        let preview = plan.preview();
        assert_eq!(preview.len(), 4);
        assert!(preview.iter().all(|p| !p.sql.contains("DROP")));
        let ordinals: Vec<_> = preview.iter().map(|p| p.ordinal).collect();
        assert_eq!(ordinals, [1, 2, 3, 4]);
        assert_eq!(preview[3].when, PreviewWhen::OnFailure);
        assert_eq!(preview[1].expected_rows, Some(1));
        assert_eq!(preview[1].params[0].marker, "?1");
        assert_eq!(preview[1].params[0].column, "name");
        assert_eq!(preview[1].params[0].type_label, "text");
        assert_eq!(preview[1].params[1].type_label, "integer");
        let text = plan.render_preview();
        assert!(text.contains("1. BEGIN"));
        assert!(text.contains("?1 name (text) = \"x'; DROP TABLE t; --\""));
        assert!(text.contains("4. ROLLBACK"));
        assert!(text.contains("only if the apply fails"));
    }

    #[test]
    fn confirmation_hook() {
        let mut s = StagedEdits::new(target(false));
        s.delete_row(ModelRow(0), &row(1, None)).unwrap();
        let plan = s.plan().unwrap();
        assert!(plan.needs_confirmation(true));
        assert!(!plan.needs_confirmation(false));
    }

    #[test]
    fn confirmed_plan_is_only_built_through_the_gate() {
        let build = || {
            let mut s = StagedEdits::new(target(false));
            s.delete_row(ModelRow(0), &row(1, None)).unwrap();
            s.plan().unwrap()
        };
        // Production without the user's consent: no token.
        assert_eq!(
            build().confirm(true, false).unwrap_err(),
            ConfirmationRequired
        );
        let plan = build();
        let expected = plan.clone();
        let ok = plan.confirm(true, true).unwrap();
        assert!(ok.production() && ok.user_confirmed());
        assert_eq!(ok.plan(), &expected);
        // Non-production needs no consent.
        let ok = build().confirm(false, false).unwrap();
        assert!(!ok.production() && !ok.user_confirmed());
        let ok = build().confirm_non_production();
        assert!(!ok.production());
    }

    #[test]
    fn preview_params_are_the_executed_params() {
        // Every dialect and every statement kind: the preview's markers, columns and values are
        // those of the Query that apply executes, and the markers are those written in the SQL.
        for dialect in [Dialect::Sqlite, Dialect::Postgres, Dialect::MySql] {
            let mut s = StagedEdits::new(crate::grid::edit::testing::target_for(dialect, true));
            let n = s.add_row();
            s.set_new_cell(n, 0, Some(Value::Int(10))).unwrap();
            s.set_new_cell(n, 1, Some(Value::Null)).unwrap();
            s.set_cell(ModelRow(5), &row(7, None), 1, Value::Text("a".into()))
                .unwrap();
            s.set_cell(ModelRow(5), &row(7, None), 0, Value::Int(70))
                .unwrap();
            s.delete_row(ModelRow(9), &row(8, Some("z"))).unwrap();
            let plan = s.plan().unwrap();
            let preview = plan.preview();
            for (stmt, shown) in plan.statements().iter().zip(&preview[1..]) {
                assert_eq!(shown.sql, stmt.query.sql);
                assert_eq!(shown.params.len(), stmt.query.params.len());
                let found = sqler_sql::placeholders(dialect, &stmt.query.sql);
                assert_eq!(found.len(), stmt.query.params.len(), "{}", stmt.query.sql);
                for (i, p) in shown.params.iter().enumerate() {
                    assert_eq!(p.value, display_value(&stmt.query.params[i]));
                    assert_eq!(p.column, stmt.param_columns[i]);
                    assert_eq!(&stmt.query.sql[found[i].range.clone()], p.marker);
                }
            }
        }
    }
}
