// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Parameters: the keys found in a plan, the prompt form, value parsing and binding.
//!
//! Values are never spliced into SQL. [`bind`] rewrites each placeholder to the dialect's native
//! positional marker and returns the values in [`Query::params`], which every driver binds.

use sqler_driver_api::sqler_core::{Query, Value};
use sqler_sql::{Dialect, StatementClass};
use thiserror::Error;

use super::plan::Plan;

/// Identifies one value the user supplies.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParamKey {
    /// `$n` or `?n`.
    Positional(u32),
    /// A named placeholder with its sigil, such as `:id`.
    Named(String),
    /// A `?`; each one is its own key.
    Anonymous {
        /// The statement's index in the plan, from 0.
        statement: usize,
        /// The position among the `?` of that statement, from 0.
        ordinal: usize,
    },
}

impl ParamKey {
    /// The label shown in the prompt: `$1`, `:id` or `? #1 (statement 2)`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Positional(n) => format!("${n}"),
            Self::Named(name) => name.clone(),
            Self::Anonymous { statement, ordinal } => {
                format!("? #{} (statement {})", ordinal + 1, statement + 1)
            }
        }
    }
}

/// How the text of a parameter becomes a [`Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ParamKind {
    /// Guess: `null`, an integer, a float, `true`/`false`, else text.
    #[default]
    Auto,
    /// The text as typed.
    Text,
    /// A 64-bit integer.
    Integer,
    /// A finite 64-bit float.
    Float,
    /// `true`, `false`, `1` or `0`.
    Boolean,
    /// SQL `NULL`; the text is ignored.
    Null,
}

impl ParamKind {
    /// Every kind, in menu order.
    pub const ALL: [ParamKind; 6] = [
        Self::Auto,
        Self::Text,
        Self::Integer,
        Self::Float,
        Self::Boolean,
        Self::Null,
    ];

    /// The name shown in the prompt.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Text => "Text",
            Self::Integer => "Integer",
            Self::Float => "Float",
            Self::Boolean => "Boolean",
            Self::Null => "Null",
            // `ParamKind` may grow; a new kind needs its own label.
        }
    }
}

/// What the user typed for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamInput {
    /// The parameter.
    pub key: ParamKey,
    /// How to read the text.
    pub kind: ParamKind,
    /// The text.
    pub text: String,
}

/// Why parameters cannot be bound.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ParamError {
    /// The text does not fit the kind (from [`parse_value`], which does not know the key).
    #[error("{0}")]
    Parse(String),
    /// The text of `key` does not fit its kind.
    #[error("{}: {reason}", key.label())]
    Invalid {
        /// The parameter.
        key: ParamKey,
        /// What is wrong.
        reason: String,
    },
    /// No input was given for `key`.
    #[error("no value for {}", .0.label())]
    Missing(ParamKey),
}

/// Turns the text of a parameter into a value.
///
/// `Auto`: trimmed `null` (any case) is `Null`, then an `i64` is `Int`, a finite `f64` is
/// `Float`, `true`/`false` (any case) is `Bool`, anything else is `Text` with the text untrimmed.
/// `Integer`, `Float` and `Boolean` (`true`, `false`, `1`, `0`) ignore surrounding blanks and fail
/// on a mismatch. `Text` keeps the text as is. `Null` ignores the text.
///
/// # Errors
/// [`ParamError::Parse`] when the text does not fit the kind.
pub fn parse_value(kind: ParamKind, text: &str) -> Result<Value, ParamError> {
    let t = text.trim();
    let bad = |what: &str| ParamError::Parse(format!("`{t}` is not {what}"));
    match kind {
        ParamKind::Text => Ok(Value::Text(text.to_owned())),
        ParamKind::Null => Ok(Value::Null),
        ParamKind::Integer => t
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| bad("an integer")),
        ParamKind::Float => finite(t).map(Value::Float).ok_or_else(|| bad("a number")),
        ParamKind::Boolean => boolean(t).map(Value::Bool).ok_or_else(|| {
            ParamError::Parse(format!("`{t}` is not a boolean (true, false, 1 or 0)"))
        }),
        _ => Ok(auto(text, t)),
    }
}

fn auto(text: &str, t: &str) -> Value {
    if t.eq_ignore_ascii_case("null") {
        Value::Null
    } else if let Ok(i) = t.parse::<i64>() {
        Value::Int(i)
    } else if let Some(f) = finite(t) {
        Value::Float(f)
    } else if t.eq_ignore_ascii_case("true") {
        Value::Bool(true)
    } else if t.eq_ignore_ascii_case("false") {
        Value::Bool(false)
    } else {
        Value::Text(text.to_owned())
    }
}

fn finite(t: &str) -> Option<f64> {
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

fn boolean(t: &str) -> Option<bool> {
    if t.eq_ignore_ascii_case("true") || t == "1" {
        Some(true)
    } else if t.eq_ignore_ascii_case("false") || t == "0" {
        Some(false)
    } else {
        None
    }
}

/// One row of the parameter prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamRow {
    /// The parameter.
    pub key: ParamKey,
    /// How to read the text.
    pub kind: ParamKind,
    /// The text.
    pub text: String,
    /// Why the row is invalid, after a failed [`ParamForm::validate`].
    pub error: Option<String>,
}

/// The parameter prompt: one row per key, in plan order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamForm {
    rows: Vec<ParamRow>,
}

impl ParamForm {
    /// A form with an empty `Auto` row for each key.
    #[must_use]
    pub fn new(keys: &[ParamKey]) -> Self {
        Self {
            rows: keys
                .iter()
                .map(|key| ParamRow {
                    key: key.clone(),
                    kind: ParamKind::Auto,
                    text: String::new(),
                    error: None,
                })
                .collect(),
        }
    }

    /// The rows.
    #[must_use]
    pub fn rows(&self) -> &[ParamRow] {
        &self.rows
    }

    /// Sets the text of row `index`; out of range is ignored.
    pub fn set_text(&mut self, index: usize, text: impl Into<String>) {
        if let Some(row) = self.rows.get_mut(index) {
            row.text = text.into();
            row.error = None;
        }
    }

    /// Sets the kind of row `index`; out of range is ignored.
    pub fn set_kind(&mut self, index: usize, kind: ParamKind) {
        if let Some(row) = self.rows.get_mut(index) {
            row.kind = kind;
            row.error = None;
        }
    }

    /// The rows as inputs for [`bind`].
    #[must_use]
    pub fn inputs(&self) -> Vec<ParamInput> {
        self.rows
            .iter()
            .map(|r| ParamInput {
                key: r.key.clone(),
                kind: r.kind,
                text: r.text.clone(),
            })
            .collect()
    }

    /// Checks every row, records the reason on the invalid ones and tells whether all are valid.
    pub fn validate(&mut self) -> bool {
        let mut ok = true;
        for row in &mut self.rows {
            row.error = match parse_value(row.kind, &row.text) {
                Ok(_) => None,
                Err(e) => {
                    ok = false;
                    Some(e.to_string())
                }
            };
        }
        ok
    }

    /// Records `error` on the row of `key`.
    pub(crate) fn set_error(&mut self, key: &ParamKey, error: String) {
        if let Some(row) = self.rows.iter_mut().find(|r| &r.key == key) {
            row.error = Some(error);
        }
    }
}

/// A statement ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundStatement {
    /// Position in the plan, from 0.
    pub index: usize,
    /// The exact body text, as shown to the user.
    pub sql: String,
    /// How dangerous the statement is.
    pub class: StatementClass,
    /// What is executed: placeholders rewritten, values in `params`.
    pub query: Query,
    /// The values in the order they are bound, with their keys.
    pub values: Vec<(ParamKey, Value)>,
}

/// Binds `inputs` to the statements of `plan`.
///
/// Each placeholder is rewritten to the dialect's marker. `PostgreSQL` gets `$k` and `SQLite` gets
/// `?k`, where `k` is the 1-based index of the key among the distinct keys of that statement (so
/// a repeated `:id` is one value and `$2` alone becomes `$1`). `MySQL` gets `?` for each
/// occurrence and one value per occurrence. A statement with no placeholders is executed
/// byte for byte as written.
///
/// # Errors
/// [`ParamError::Missing`] when a key has no input, [`ParamError::Invalid`] when its text does
/// not fit its kind.
pub fn bind(
    dialect: Option<Dialect>,
    plan: &Plan,
    inputs: &[ParamInput],
) -> Result<Vec<BoundStatement>, ParamError> {
    let value_of = |key: &ParamKey| -> Result<Value, ParamError> {
        let input = inputs
            .iter()
            .find(|i| &i.key == key)
            .ok_or_else(|| ParamError::Missing(key.clone()))?;
        parse_value(input.kind, &input.text).map_err(|e| ParamError::Invalid {
            key: key.clone(),
            reason: match e {
                ParamError::Parse(reason) => reason,
                other => other.to_string(),
            },
        })
    };
    let mut out = Vec::with_capacity(plan.statements.len());
    for st in &plan.statements {
        let keys = st.keys();
        let mut sql = st.sql.clone();
        let mut values: Vec<(ParamKey, Value)> = Vec::new();
        if dialect.is_some() {
            let mut distinct: Vec<&ParamKey> = Vec::new();
            let mut markers = Vec::with_capacity(keys.len());
            for key in &keys {
                let marker = if dialect == Some(Dialect::MySql) {
                    values.push((key.clone(), value_of(key)?));
                    "?".to_owned()
                } else {
                    let pos = distinct.iter().position(|k| *k == key).unwrap_or_else(|| {
                        distinct.push(key);
                        distinct.len() - 1
                    });
                    if pos + 1 == distinct.len() && values.len() < distinct.len() {
                        values.push((key.clone(), value_of(key)?));
                    }
                    if dialect == Some(Dialect::Postgres) {
                        format!("${}", pos + 1)
                    } else {
                        format!("?{}", pos + 1)
                    }
                };
                markers.push(marker);
            }
            for (ph, marker) in st.placeholders.iter().zip(&markers).rev() {
                sql.replace_range(ph.range.clone(), marker);
            }
        }
        let params = values.iter().map(|(_, v)| v.clone()).collect();
        out.push(BoundStatement {
            index: st.index,
            sql: st.sql.clone(),
            class: st.class,
            query: Query::new(sql).with_params(params),
            values,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::buffer::{BufferSnapshot, RunMode};
    use super::super::plan::plan;
    use super::*;

    fn named(s: &str) -> ParamKey {
        ParamKey::Named(s.to_owned())
    }

    fn input(key: ParamKey, text: &str) -> ParamInput {
        ParamInput {
            key,
            kind: ParamKind::Auto,
            text: text.to_owned(),
        }
    }

    fn planned(d: Dialect, text: &str) -> Plan {
        plan(Some(d), &BufferSnapshot::at_cursor(text, 0), RunMode::All).unwrap()
    }

    #[test]
    fn labels() {
        assert_eq!(ParamKey::Positional(1).label(), "$1");
        assert_eq!(named(":id").label(), ":id");
        assert_eq!(
            ParamKey::Anonymous {
                statement: 1,
                ordinal: 0
            }
            .label(),
            "? #1 (statement 2)"
        );
    }

    #[test]
    fn auto_guesses() {
        let v = |t| parse_value(ParamKind::Auto, t).unwrap();
        assert_eq!(v(" NULL "), Value::Null);
        assert_eq!(v("42"), Value::Int(42));
        assert_eq!(v(" -7 "), Value::Int(-7));
        assert_eq!(v("1.5"), Value::Float(1.5));
        assert_eq!(v("1e3"), Value::Float(1000.0));
        assert_eq!(v("TRUE"), Value::Bool(true));
        assert_eq!(v("false"), Value::Bool(false));
        assert_eq!(v(" abc "), Value::Text(" abc ".to_owned()));
        assert_eq!(v(""), Value::Text(String::new()));
        assert_eq!(v("inf"), Value::Text("inf".to_owned()));
        assert_eq!(v("NaN"), Value::Text("NaN".to_owned()));
        assert_eq!(v("99999999999999999999"), Value::Float(1e20));
    }

    #[test]
    fn explicit_kinds() {
        let ok = |k, t| parse_value(k, t).unwrap();
        assert_eq!(ok(ParamKind::Text, " 42 "), Value::Text(" 42 ".to_owned()));
        assert_eq!(ok(ParamKind::Text, "null"), Value::Text("null".to_owned()));
        assert_eq!(ok(ParamKind::Null, "whatever"), Value::Null);
        assert_eq!(ok(ParamKind::Integer, " 5"), Value::Int(5));
        assert_eq!(ok(ParamKind::Float, "5"), Value::Float(5.0));
        assert_eq!(ok(ParamKind::Boolean, "1"), Value::Bool(true));
        assert_eq!(ok(ParamKind::Boolean, "0"), Value::Bool(false));
        assert_eq!(ok(ParamKind::Boolean, "False"), Value::Bool(false));
        for (k, t) in [
            (ParamKind::Integer, "1.5"),
            (ParamKind::Integer, ""),
            (ParamKind::Float, "abc"),
            (ParamKind::Float, "inf"),
            (ParamKind::Boolean, "yes"),
            (ParamKind::Boolean, "2"),
        ] {
            assert!(
                matches!(parse_value(k, t), Err(ParamError::Parse(_))),
                "{k:?} {t:?}"
            );
        }
    }

    #[test]
    fn postgres_repeated_named_is_one_value() {
        let p = planned(Dialect::Postgres, "select :id, :name, :id");
        let b = bind(
            Some(Dialect::Postgres),
            &p,
            &[input(named(":id"), "7"), input(named(":name"), "bo")],
        )
        .unwrap();
        assert_eq!(b[0].query.sql, "select $1, $2, $1");
        assert_eq!(
            b[0].query.params,
            [Value::Int(7), Value::Text("bo".to_owned())]
        );
        assert_eq!(b[0].sql, "select :id, :name, :id");
        assert_eq!(b[0].values.len(), 2);
        assert_eq!(b[0].values[0].0, named(":id"));
    }

    #[test]
    fn postgres_second_positional_alone_is_renumbered() {
        let p = planned(Dialect::Postgres, "select $2::text");
        let b = bind(
            Some(Dialect::Postgres),
            &p,
            &[input(ParamKey::Positional(2), "x")],
        )
        .unwrap();
        assert_eq!(b[0].query.sql, "select $1::text");
        assert_eq!(b[0].query.params, [Value::Text("x".to_owned())]);
    }

    #[test]
    fn sqlite_numbers_by_distinct_key() {
        let p = planned(Dialect::Sqlite, "select ?, @b, ?, :a, @b");
        let keys = &p.params;
        assert_eq!(keys.len(), 4);
        let inputs: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| input(k.clone(), &i.to_string()))
            .collect();
        let b = bind(Some(Dialect::Sqlite), &p, &inputs).unwrap();
        assert_eq!(b[0].query.sql, "select ?1, ?2, ?3, ?4, ?2");
        assert_eq!(
            b[0].query.params,
            [Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3)]
        );
    }

    #[test]
    fn mysql_binds_each_occurrence() {
        let p = planned(Dialect::MySql, "select :id, ?, :id; select :id");
        assert_eq!(p.params.len(), 2);
        let b = bind(
            Some(Dialect::MySql),
            &p,
            &[input(named(":id"), "7"), input(p.params[1].clone(), "x")],
        )
        .unwrap();
        assert_eq!(b[0].query.sql, "select ?, ?, ?");
        assert_eq!(
            b[0].query.params,
            [Value::Int(7), Value::Text("x".to_owned()), Value::Int(7)]
        );
        assert_eq!(b[0].values.len(), 3);
        assert_eq!(b[1].query.sql, "select ?");
        assert_eq!(b[1].query.params, [Value::Int(7)]);
    }

    #[test]
    fn values_are_never_spliced() {
        let p = planned(Dialect::Sqlite, "select :a");
        let evil = "1; drop table t; --";
        let b = bind(Some(Dialect::Sqlite), &p, &[input(named(":a"), evil)]).unwrap();
        assert_eq!(b[0].query.sql, "select ?1");
        assert_eq!(b[0].query.params, [Value::Text(evil.to_owned())]);
    }

    #[test]
    fn no_placeholders_keeps_the_text_byte_for_byte() {
        let text = "select  'a:b',\t\"?\" -- :x\nfrom t";
        for d in Dialect::ALL {
            let p = planned(d, text);
            let b = bind(Some(d), &p, &[]).unwrap();
            assert_eq!(b[0].query.sql, text);
            assert_eq!(b[0].query.params.len(), 0);
            assert_eq!(b[0].values.len(), 0);
        }
        let p = plan(None, &BufferSnapshot::at_cursor(text, 0), RunMode::All).unwrap();
        assert_eq!(bind(None, &p, &[]).unwrap()[0].query.sql, text);
    }

    #[test]
    fn multibyte_text_around_placeholders() {
        let p = planned(Dialect::Postgres, "select 'é', :a, 'ü' || :a");
        let b = bind(Some(Dialect::Postgres), &p, &[input(named(":a"), "x")]).unwrap();
        assert_eq!(b[0].query.sql, "select 'é', $1, 'ü' || $1");
    }

    #[test]
    fn missing_and_invalid() {
        let p = planned(Dialect::Postgres, "select :a");
        assert_eq!(
            bind(Some(Dialect::Postgres), &p, &[]),
            Err(ParamError::Missing(named(":a")))
        );
        let bad = ParamInput {
            key: named(":a"),
            kind: ParamKind::Integer,
            text: "x".to_owned(),
        };
        let err = bind(Some(Dialect::Postgres), &p, &[bad]).unwrap_err();
        assert!(matches!(&err, ParamError::Invalid { key, .. } if *key == named(":a")));
        assert!(err.to_string().starts_with(":a: "));
        assert_eq!(err.to_string(), ":a: `x` is not an integer");
        assert_eq!(
            ParamError::Missing(named(":a")).to_string(),
            "no value for :a"
        );
    }

    #[test]
    fn form_collects_and_validates() {
        let mut f = ParamForm::new(&[named(":a"), ParamKey::Positional(1)]);
        assert_eq!(f.rows().len(), 2);
        f.set_text(0, "5");
        f.set_kind(1, ParamKind::Integer);
        f.set_text(1, "x");
        f.set_text(9, "ignored");
        assert!(!f.validate());
        assert_eq!(f.rows()[0].error, None);
        assert!(f.rows()[1].error.is_some());
        f.set_text(1, "3");
        assert!(f.validate());
        let inputs = f.inputs();
        assert_eq!(inputs[0].text, "5");
        assert_eq!(inputs[1].kind, ParamKind::Integer);
        assert_eq!(ParamKind::ALL.len(), 6);
        assert_eq!(ParamKind::Auto.label(), "Auto");
    }
}
