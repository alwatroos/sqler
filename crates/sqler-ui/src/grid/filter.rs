// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Filtering: the filter specification and its compiled form.
//!
//! A [`FilterSpec`] is a quick filter (text contained in one column or in any) AND a list of
//! per-column conditions. [`FilterSpec::compile`] parses every operand once;
//! [`CompiledFilter::matches`] then tests a row without allocating in the common cases. NULL
//! fails every operator except `IsNull` (SQL semantics: `NotEquals` does not match NULL either).

use std::cmp::Ordering;
use std::fmt::Write;
use std::str::FromStr;

use sqler_driver_api::sqler_core::{Date, Decimal, Time, Timestamp, TimestampTz, Value};

use super::sort::{
    Collation, NumSrc, cmp_num, cmp_text_primary, date_instant, time_of_day, ts_instant, tz_instant,
};
use super::store::{CellRef, RowRef};

/// A comparison of a cell with an operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    /// Equal to the operand.
    Equals,
    /// Not equal to the operand (and not NULL).
    NotEquals,
    /// The display text contains the operand.
    Contains,
    /// Greater than the operand.
    Greater,
    /// Less than the operand.
    Less,
    /// The cell is NULL.
    IsNull,
    /// The cell is not NULL.
    NotNull,
}

impl FilterOp {
    /// Whether the operator takes an operand.
    #[must_use]
    pub fn needs_operand(self) -> bool {
        !matches!(self, Self::IsNull | Self::NotNull)
    }
}

/// One per-column condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    /// The model column.
    pub column: usize,
    /// The operator.
    pub op: FilterOp,
    /// The operand as typed; trimmed when compared.
    pub operand: String,
}

/// The quick filter: text contained in a column's display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFilter {
    /// The text to find; empty matches everything.
    pub text: String,
    /// The model column to look in; `None` is every column.
    pub column: Option<usize>,
}

/// A quick filter and conditions, combined with AND.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterSpec {
    /// The quick filter.
    pub quick: Option<QuickFilter>,
    /// The per-column conditions.
    pub conditions: Vec<Condition>,
}

impl FilterSpec {
    /// Whether the spec keeps every row: no conditions and no quick text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty() && self.quick.as_ref().is_none_or(|q| q.text.is_empty())
    }

    /// Compiles the spec: operands are parsed once, texts are case-folded per `c`.
    #[must_use]
    pub fn compile(&self, c: Collation) -> CompiledFilter {
        CompiledFilter {
            collation: c,
            quick: self
                .quick
                .as_ref()
                .filter(|q| !q.text.is_empty())
                .map(|q| (q.column, Needle::new(&q.text, c))),
            conditions: self
                .conditions
                .iter()
                .map(|cond| Compiled::new(cond, c))
                .collect(),
        }
    }
}

/// A case-folded needle.
#[derive(Debug, Clone)]
struct Needle {
    text: String,
    ascii: bool,
    fold: bool,
}

impl Needle {
    fn new(text: &str, c: Collation) -> Self {
        let fold = c == Collation::CaseInsensitive;
        let text = if fold {
            text.chars().flat_map(char::to_lowercase).collect()
        } else {
            text.to_owned()
        };
        Self {
            ascii: text.is_ascii(),
            text,
            fold,
        }
    }

    fn found_in(&self, hay: &str) -> bool {
        if self.text.is_empty() {
            return true;
        }
        if !self.fold {
            return hay.contains(self.text.as_str());
        }
        if self.ascii && hay.is_ascii() {
            let n = self.text.as_bytes();
            return hay
                .as_bytes()
                .windows(n.len())
                .any(|w| w.eq_ignore_ascii_case(n));
        }
        let folded: String = hay.chars().flat_map(char::to_lowercase).collect();
        folded.contains(self.text.as_str())
    }
}

/// The operand of a condition, read in every type it can be read in.
#[derive(Debug, Clone)]
struct Operand {
    text: String,
    needle: Needle,
    decimal: Option<Decimal>,
    int: Option<i64>,
    /// The operand as a double, only when that is exact for ordering (at most 15 digits).
    float: Option<f64>,
    boolean: Option<bool>,
    date: Option<Date>,
    time: Option<Time>,
    timestamp: Option<Timestamp>,
    timestamp_tz: Option<TimestampTz>,
}

impl Operand {
    fn new(raw: &str, c: Collation) -> Self {
        let text = raw.trim().to_owned();
        let decimal = Decimal::from_str(&text).ok();
        let int = decimal.as_ref().and_then(|_| text.parse::<i64>().ok());
        let float = decimal.as_ref().and_then(|d| {
            let body = d.as_str().trim_start_matches(['+', '-']);
            let digits = body.replace('.', "");
            (digits.trim_start_matches('0').len() <= 15 && digits.len() <= 15)
                .then(|| d.as_str().parse::<f64>().ok())
                .flatten()
        });
        let boolean = match text.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" => Some(true),
            "false" | "f" | "0" => Some(false),
            _ => None,
        };
        Self {
            needle: Needle::new(&text, c),
            decimal,
            int,
            float,
            boolean,
            date: Date::from_str(&text).ok(),
            time: Time::from_str(&text).ok(),
            timestamp: Timestamp::from_str(&text).ok(),
            timestamp_tz: TimestampTz::from_str(&text).ok(),
            text,
        }
    }
}

#[derive(Debug, Clone)]
struct Compiled {
    column: usize,
    op: FilterOp,
    operand: Operand,
}

impl Compiled {
    fn new(cond: &Condition, c: Collation) -> Self {
        Self {
            column: cond.column,
            op: cond.op,
            operand: Operand::new(&cond.operand, c),
        }
    }
}

/// A [`FilterSpec`] ready to test rows.
#[derive(Debug, Clone)]
pub struct CompiledFilter {
    collation: Collation,
    quick: Option<(Option<usize>, Needle)>,
    conditions: Vec<Compiled>,
}

/// The display text of a cell: borrowed from text cells, formatted into `scratch` otherwise.
fn display_text<'a>(cell: CellRef<'a>, scratch: &'a mut String) -> &'a str {
    match cell {
        CellRef::Text(s) | CellRef::Json(s) => s,
        CellRef::Value(Value::Text(s) | Value::Json(s)) => s,
        other => {
            scratch.clear();
            // Writing to a `String` cannot fail.
            let _ = write!(scratch, "{other}");
            scratch
        }
    }
}

fn contains(cell: CellRef<'_>, needle: &Needle, scratch: &mut String) -> bool {
    !cell.is_null() && needle.found_in(display_text(cell, scratch))
}

impl CompiledFilter {
    /// Whether the row passes the quick filter and every condition.
    #[must_use]
    pub fn matches(&self, row: RowRef<'_>, scratch: &mut String) -> bool {
        if let Some((column, needle)) = &self.quick {
            let hit = match column {
                Some(c) => row.get(*c).is_some_and(|x| contains(x, needle, scratch)),
                None => row.iter().any(|x| contains(x, needle, scratch)),
            };
            if !hit {
                return false;
            }
        }
        self.conditions.iter().all(|c| {
            let cell = row.get(c.column).unwrap_or(CellRef::Null);
            self.condition(c, cell, scratch)
        })
    }

    fn condition(&self, c: &Compiled, cell: CellRef<'_>, scratch: &mut String) -> bool {
        match c.op {
            FilterOp::IsNull => return cell.is_null(),
            FilterOp::NotNull => return !cell.is_null(),
            _ => {}
        }
        if cell.is_null() {
            return false;
        }
        if c.op == FilterOp::Contains {
            return contains(cell, &c.operand.needle, scratch);
        }
        let o = self.typed_cmp(cell, &c.operand, scratch);
        match c.op {
            FilterOp::Equals => o == Ordering::Equal,
            FilterOp::NotEquals => o != Ordering::Equal,
            FilterOp::Greater => o == Ordering::Greater,
            FilterOp::Less => o == Ordering::Less,
            _ => false,
        }
    }

    /// The cell against the operand, read in the cell's type; the display text under the
    /// collation when the operand does not parse as that type.
    fn typed_cmp(&self, cell: CellRef<'_>, op: &Operand, scratch: &mut String) -> Ordering {
        let value = match cell {
            CellRef::Value(v) => Some(v),
            _ => None,
        };
        let typed = match (cell, value) {
            (CellRef::Int(i), _) | (_, Some(&Value::Int(i))) => op.decimal.as_ref().map(|d| {
                op.int.map_or_else(
                    || cmp_num(NumSrc::Int(i), NumSrc::Dec(d.as_str())),
                    |n| i.cmp(&n),
                )
            }),
            (CellRef::Float(x), _) | (_, Some(&Value::Float(x))) => {
                op.decimal.as_ref().map(|d| match op.float {
                    Some(f) if x.is_finite() => x.partial_cmp(&f).unwrap_or(Ordering::Equal),
                    _ => cmp_num(NumSrc::Float(x), NumSrc::Dec(d.as_str())),
                })
            }
            (_, Some(Value::Decimal(x))) => op
                .decimal
                .as_ref()
                .map(|d| cmp_num(NumSrc::Dec(x.as_str()), NumSrc::Dec(d.as_str()))),
            (CellRef::Bool(b), _) | (_, Some(&Value::Bool(b))) => op.boolean.map(|o| b.cmp(&o)),
            (_, Some(Value::Date(d))) => op.date.map(|o| date_instant(*d).cmp(&date_instant(o))),
            (_, Some(Value::Timestamp(t))) => {
                let t = *t;
                op.timestamp
                    .map(|o| ts_instant(t).cmp(&ts_instant(o)))
                    .or_else(|| op.date.map(|o| ts_instant(t).cmp(&date_instant(o))))
            }
            (_, Some(Value::TimestampTz(t))) => {
                let t = *t;
                op.timestamp_tz.map(|o| tz_instant(t).cmp(&tz_instant(o)))
            }
            (_, Some(Value::Time(t))) => op.time.map(|o| time_of_day(*t).cmp(&time_of_day(o))),
            _ => None,
        };
        typed.unwrap_or_else(|| {
            cmp_text_primary(display_text(cell, scratch), &op.text, self.collation)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::order::testing;
    use super::*;

    /// A model that holds `rows`.
    fn model_of(rows: Vec<Vec<Value>>) -> crate::grid::GridModel {
        let width = rows.first().map_or(0, Vec::len);
        let (mut model, sink) = testing::channel(width);
        testing::push(&mut model, &sink, rows);
        model
    }

    fn pass(spec: &FilterSpec, c: Collation, row: Vec<Value>) -> bool {
        let model = model_of(vec![row]);
        let f = spec.compile(c);
        f.matches(model.row(0).unwrap(), &mut String::new())
    }

    fn cond(column: usize, op: FilterOp, operand: &str) -> FilterSpec {
        FilterSpec {
            quick: None,
            conditions: vec![Condition {
                column,
                op,
                operand: operand.to_owned(),
            }],
        }
    }

    fn one(op: FilterOp, operand: &str, v: Value) -> bool {
        pass(&cond(0, op, operand), Collation::CaseInsensitive, vec![v])
    }

    fn date(s: &str) -> Value {
        Value::Date(s.parse().unwrap())
    }

    #[test]
    fn empty_specs_keep_everything() {
        assert!(FilterSpec::default().is_empty());
        let blank = FilterSpec {
            quick: Some(QuickFilter {
                text: String::new(),
                column: Some(3),
            }),
            conditions: vec![],
        };
        assert!(blank.is_empty());
        assert!(pass(&blank, Collation::default(), vec![Value::Null]));
        assert!(!cond(0, FilterOp::IsNull, "").is_empty());
    }

    #[test]
    fn null_fails_everything_but_is_null() {
        for op in [
            FilterOp::Equals,
            FilterOp::NotEquals,
            FilterOp::Contains,
            FilterOp::Greater,
            FilterOp::Less,
            FilterOp::NotNull,
        ] {
            assert!(!one(op, "", Value::Null), "{op:?}");
            assert!(!one(op, "NULL", Value::Null), "{op:?}");
        }
        assert!(one(FilterOp::IsNull, "", Value::Null));
        assert!(!one(FilterOp::IsNull, "", Value::Int(0)));
        assert!(one(FilterOp::NotNull, "", Value::Int(0)));
        assert!(!FilterOp::IsNull.needs_operand() && FilterOp::Equals.needs_operand());
    }

    #[test]
    fn integers_compare_numerically() {
        assert!(one(FilterOp::Equals, " 42 ", Value::Int(42)));
        assert!(one(FilterOp::Equals, "42.0", Value::Int(42)));
        assert!(one(FilterOp::NotEquals, "43", Value::Int(42)));
        assert!(one(FilterOp::Greater, "9", Value::Int(10)));
        assert!(!one(FilterOp::Greater, "10", Value::Int(10)));
        assert!(one(FilterOp::Less, "10.5", Value::Int(10)));
        assert!(one(FilterOp::Greater, "-1", Value::Int(0)));
        assert!(one(
            FilterOp::Less,
            "9223372036854775808",
            Value::Int(i64::MAX)
        ));
    }

    #[test]
    fn floats_and_decimals_compare_by_displayed_value() {
        assert!(one(FilterOp::Equals, "1.5", Value::Float(1.5)));
        assert!(one(FilterOp::Greater, "500000", Value::Float(500_000.5)));
        assert!(one(FilterOp::Less, "0.1", Value::Float(0.05)));
        assert!(one(FilterOp::Equals, "0.1", Value::Float(0.1)));
        assert!(one(FilterOp::Greater, "1e1", Value::Float(f64::NAN)));
        assert!(one(FilterOp::Greater, "5", Value::Float(f64::INFINITY)));
        assert!(one(FilterOp::Less, "5", Value::Float(f64::NEG_INFINITY)));
        // More than 15 digits: the exact path.
        assert!(!one(
            FilterOp::Greater,
            "0.10000000000000000001",
            Value::Float(0.1)
        ));
        assert!(one(
            FilterOp::Less,
            "0.10000000000000000001",
            Value::Float(0.1)
        ));
        let dec = |s: &str| Value::Decimal(s.parse().unwrap());
        assert!(one(FilterOp::Equals, "2.5", dec("2.50")));
        assert!(one(FilterOp::Greater, "2", dec("2.01")));
        assert!(one(FilterOp::Less, "3", dec("-3.5")));
    }

    #[test]
    fn booleans_read_the_usual_spellings() {
        for t in ["true", "T", "1", "True"] {
            assert!(one(FilterOp::Equals, t, Value::Bool(true)), "{t}");
            assert!(one(FilterOp::NotEquals, t, Value::Bool(false)), "{t}");
        }
        for f in ["false", "f", "0"] {
            assert!(one(FilterOp::Equals, f, Value::Bool(false)), "{f}");
        }
        assert!(one(FilterOp::Greater, "false", Value::Bool(true)));
        assert!(one(FilterOp::Less, "true", Value::Bool(false)));
    }

    #[test]
    fn temporal_operands_are_read_in_the_cell_type() {
        assert!(one(FilterOp::Equals, "2024-02-29", date("2024-02-29")));
        assert!(one(FilterOp::Greater, "2024-02-28", date("2024-02-29")));
        assert!(one(FilterOp::Less, "2025-01-01", date("2024-02-29")));
        let ts = Value::Timestamp("2024-02-29 12:30:00".parse().unwrap());
        assert!(one(FilterOp::Greater, "2024-02-29 12:00:00", ts.clone()));
        assert!(one(FilterOp::Greater, "2024-02-28", ts.clone()));
        assert!(one(FilterOp::Equals, "2024-02-29 12:30:00", ts));
        let tm = Value::Time("12:30:00".parse().unwrap());
        assert!(one(FilterOp::Less, "13:00:00", tm.clone()));
        assert!(one(FilterOp::Equals, "12:30:00", tm));
        let tz = Value::TimestampTz("2024-02-29 12:00:00+00:00".parse().unwrap());
        assert!(one(
            FilterOp::Equals,
            "2024-02-29 14:00:00+02:00",
            tz.clone()
        ));
        assert!(one(FilterOp::Greater, "2024-02-29 13:00:00+02:00", tz));
    }

    #[test]
    fn operands_that_do_not_parse_compare_display_text() {
        // A number column and a non-number operand: text order of "42" against "abc".
        assert!(one(FilterOp::Less, "abc", Value::Int(42)));
        assert!(one(FilterOp::NotEquals, "abc", Value::Int(42)));
        assert!(!one(FilterOp::Equals, "abc", Value::Int(42)));
        assert!(one(FilterOp::Equals, "42x", Value::Text("42X".into())));
        assert!(!one(FilterOp::Equals, "tru", Value::Bool(true)));
        assert!(!one(FilterOp::Equals, "soon", date("2024-02-29")));
        assert!(one(FilterOp::Greater, "2024", date("2024-02-29")));
        // Empty operand against a number: text order.
        assert!(one(FilterOp::Greater, "", Value::Int(1)));
    }

    #[test]
    fn text_equality_and_order_follow_the_collation() {
        let eq = cond(0, FilterOp::Equals, "ä");
        assert!(pass(
            &eq,
            Collation::CaseInsensitive,
            vec![Value::Text("Ä".into())]
        ));
        assert!(!pass(
            &eq,
            Collation::CodePoint,
            vec![Value::Text("Ä".into())]
        ));
        let gt = cond(0, FilterOp::Greater, "b");
        assert!(pass(
            &gt,
            Collation::CaseInsensitive,
            vec![Value::Text("C".into())]
        ));
        assert!(!pass(
            &gt,
            Collation::CaseInsensitive,
            vec![Value::Text("B".into())]
        ));
        assert!(!pass(
            &gt,
            Collation::CodePoint,
            vec![Value::Text("C".into())]
        ));
        assert!(pass(
            &gt,
            Collation::CodePoint,
            vec![Value::Text("c".into())]
        ));
        assert!(!pass(
            &gt,
            Collation::CodePoint,
            vec![Value::Text("B".into())]
        ));
    }

    #[test]
    fn contains_folds_ascii_and_unicode() {
        assert!(one(FilterOp::Contains, "ell", Value::Text("Hello".into())));
        assert!(one(FilterOp::Contains, "ELL", Value::Text("hello".into())));
        assert!(one(FilterOp::Contains, "ÄB", Value::Text("xäby".into())));
        assert!(!one(FilterOp::Contains, "ab", Value::Text("xÄBy".into())));
        assert!(one(FilterOp::Contains, "äb", Value::Text("xÄBy".into())));
        assert!(one(FilterOp::Contains, "", Value::Text("x".into())));
        assert!(!one(FilterOp::Contains, "z", Value::Text("x".into())));
        // Non-text cells match on their display text.
        assert!(one(FilterOp::Contains, "34", Value::Int(1234)));
        assert!(one(FilterOp::Contains, "tru", Value::Bool(true)));
        assert!(one(FilterOp::Contains, "02-29", date("2024-02-29")));
        assert!(one(FilterOp::Contains, "0xab", Value::Bytes(vec![0xab])));
        assert!(one(
            FilterOp::Contains,
            "\"a\"",
            Value::Json("{\"a\":1}".into())
        ));
        // Match case.
        let spec = cond(0, FilterOp::Contains, "ELL");
        assert!(!pass(
            &spec,
            Collation::CodePoint,
            vec![Value::Text("hello".into())]
        ));
        assert!(pass(
            &spec,
            Collation::CodePoint,
            vec![Value::Text("HELLO".into())]
        ));
    }

    #[test]
    fn quick_filter_scopes_to_all_columns_or_one() {
        let row = || vec![Value::Text("alpha".into()), Value::Int(99), Value::Null];
        let q = |text: &str, column| FilterSpec {
            quick: Some(QuickFilter {
                text: text.to_owned(),
                column,
            }),
            conditions: vec![],
        };
        let ci = Collation::CaseInsensitive;
        assert!(pass(&q("ALP", None), ci, row()));
        assert!(pass(&q("99", None), ci, row()));
        assert!(
            !pass(&q("NULL", None), ci, row()),
            "NULL never matches contains"
        );
        assert!(pass(&q("99", Some(1)), ci, row()));
        assert!(!pass(&q("99", Some(0)), ci, row()));
        assert!(!pass(&q("x", Some(7)), ci, row()), "no such column");
        assert!(!pass(&q("x", Some(2)), ci, row()));
    }

    #[test]
    fn quick_and_conditions_combine_with_and() {
        let spec = FilterSpec {
            quick: Some(QuickFilter {
                text: "a".into(),
                column: Some(0),
            }),
            conditions: vec![
                Condition {
                    column: 1,
                    op: FilterOp::Greater,
                    operand: "5".into(),
                },
                Condition {
                    column: 1,
                    op: FilterOp::Less,
                    operand: "10".into(),
                },
            ],
        };
        let ci = Collation::CaseInsensitive;
        let row = |t: &str, n| vec![Value::Text(t.into()), Value::Int(n)];
        assert!(pass(&spec, ci, row("xax", 7)));
        assert!(!pass(&spec, ci, row("xyz", 7)));
        assert!(!pass(&spec, ci, row("xax", 5)));
        assert!(!pass(&spec, ci, row("xax", 10)));
        // A condition on a missing column sees NULL.
        assert!(pass(&cond(9, FilterOp::IsNull, ""), ci, row("a", 1)));
        assert!(!pass(&cond(9, FilterOp::Equals, "1"), ci, row("a", 1)));
    }
}
