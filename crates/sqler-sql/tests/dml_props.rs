// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Properties of bound DML: values only ever travel as parameters, never as SQL text.

#![allow(clippy::unwrap_used)]

use proptest::prelude::*;
use sqler_core::{
    ColumnInfo, DataType, Date, DbObject, ObjectKind, ObjectPath, Query, Table, Time, Timestamp,
    TimestampTz, Value,
};
use sqler_sql::{Dialect, DmlError, PlaceholderKind, delete, insert, placeholders, update};

const LOCK: &str = "\u{1F512}";

/// Values that look like an injection attempt; every one contains a single quote.
const INJECTIONS: &[&str] = &[
    "'; DROP TABLE t; --",
    "' OR '1'='1",
    "\" OR \"\"=\"",
    "x'); DELETE FROM t; --",
    "\\'; SELECT pg_sleep(10); --",
    "'' UNION SELECT password FROM users --",
    "' || (SELECT 1) || '",
];

/// A column name that is plain ASCII, may hold quote characters of the dialects (but never a
/// single quote), `$` and `?`, and has no digits.
fn ascii_name() -> BoxedStrategy<String> {
    "[A-Za-z_ \"`;$?\\-]{1,8}".boxed()
}

/// Any non-empty, NUL-free name, with quotes and Unicode.
fn any_name() -> BoxedStrategy<String> {
    prop_oneof![
        any::<String>(),
        "[\"`'; .\\-a-zA-Z0-9 $?:@]{1,12}",
        Just("\u{1F512}\"`'$1?".to_owned()),
    ]
    .prop_filter("non-empty and NUL-free", |s| {
        !s.is_empty() && !s.contains('\0')
    })
    .boxed()
}

fn data_type() -> impl Strategy<Value = DataType> {
    prop_oneof![
        Just(DataType::Boolean),
        Just(DataType::Integer),
        Just(DataType::Float),
        Just(DataType::Decimal),
        Just(DataType::Text),
        Just(DataType::Binary),
        Just(DataType::Date),
        Just(DataType::Time),
        Just(DataType::Timestamp),
        Just(DataType::TimestampTz),
        Just(DataType::Json),
        Just(DataType::Other),
    ]
}

/// Native types: none, arbitrary text (almost always unsafe), safe-looking text without digits
/// (so it cannot collide with a value's text) and an injection attempt.
fn native_type() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        any::<String>().prop_map(Some),
        "[a-z_ ,.()\\[\\]\"]{0,20}".prop_map(Some),
        // Quoted type names, as `format_type` prints enums with odd names; the generator re-quotes.
        ("[a-zA-Z0-9_ $?:'@-]{1,8}", "[a-zA-Z0-9_ $?:'@-]{1,8}")
            .prop_map(|(a, b)| Some(format!("\"{a}\".\"{b}\"[]"))),
        Just(Some("text); DROP TABLE t; --".to_owned())),
        Just(Some("text'; --".to_owned())),
    ]
}

fn value() -> impl Strategy<Value = Value> {
    let date = Date::new(2024, 2, 29).unwrap();
    let time = Time::new(13, 14, 15, 0).unwrap();
    let ts = Timestamp::new(date, time);
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<f64>().prop_map(Value::Float),
        any::<String>().prop_map(|s| Value::Text(format!("{LOCK}{s}"))),
        any::<String>().prop_map(|s| Value::Json(format!("{LOCK}{s}"))),
        proptest::collection::vec(any::<u8>(), 1..16).prop_map(Value::Bytes),
        Just(Value::Decimal("12.50".parse().unwrap())),
        Just(Value::Date(date)),
        Just(Value::Time(time)),
        Just(Value::Timestamp(ts)),
        Just(Value::TimestampTz(TimestampTz::new(ts, 3600).unwrap())),
        proptest::sample::select(INJECTIONS).prop_map(|s| Value::Text(s.to_owned())),
        proptest::sample::select(INJECTIONS).prop_map(|s| Value::Json(s.to_owned())),
    ]
}

/// Whether the value's `Display` text is distinctive enough to be searched for in the SQL.
fn searchable(v: &Value) -> bool {
    matches!(
        v,
        Value::Text(_)
            | Value::Json(_)
            | Value::Bytes(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::Timestamp(_)
            | Value::TimestampTz(_)
    )
}

#[derive(Debug, Clone)]
struct Case {
    table: Table,
    values: Vec<Value>,
    in_set: Vec<bool>,
    in_filter: Vec<bool>,
}

fn case(names: BoxedStrategy<String>) -> impl Strategy<Value = Case> {
    let columns = proptest::collection::vec((names.clone(), data_type(), native_type()), 1..7);
    let object = (names.clone(), proptest::option::of(names));
    (
        columns,
        object,
        proptest::collection::vec(value(), 6),
        proptest::collection::vec(any::<bool>(), 6),
        proptest::collection::vec(any::<bool>(), 6),
    )
        .prop_map(
            move |(columns, (name, schema), values, in_set, in_filter)| {
                let mut seen = std::collections::HashSet::new();
                let columns: Vec<ColumnInfo> = columns
                    .into_iter()
                    .filter(|(n, _, _)| seen.insert(n.clone()))
                    .map(|(n, dt, native)| {
                        let mut c = ColumnInfo::new(n, dt);
                        c.native_type = native;
                        c
                    })
                    .collect();
                let mut parent = ObjectPath::root();
                if let Some(s) = schema {
                    parent = parent.in_schema(s);
                }
                Case {
                    table: Table::new(DbObject::new(ObjectKind::Table, parent, name))
                        .with_columns(columns),
                    values,
                    in_set,
                    in_filter,
                }
            },
        )
}

/// The `(name, value)` pairs of the columns picked by `mask`.
fn pick<'a>(case: &'a Case, mask: &[bool]) -> Vec<(&'a str, &'a Value)> {
    case.table
        .columns
        .iter()
        .zip(&case.values)
        .zip(mask)
        .filter(|(_, m)| **m)
        .map(|((c, v), _)| (c.name.as_str(), v))
        .collect()
}

fn check_placeholders(d: Dialect, q: &Query) -> Result<(), TestCaseError> {
    let found = placeholders(d, &q.sql);
    prop_assert_eq!(found.len(), q.params.len(), "{}", q.sql);
    for (i, p) in found.iter().enumerate() {
        let expected = match d {
            Dialect::MySql => PlaceholderKind::Anonymous,
            // `PostgreSQL` `$n`, `SQLite` `?n`, T-SQL `@Pn`.
            _ => PlaceholderKind::Positional(u32::try_from(i + 1).unwrap()),
        };
        prop_assert_eq!(&p.kind, &expected, "{}", q.sql);
    }
    Ok(())
}

fn non_null(pairs: &[(&str, &Value)]) -> Vec<Value> {
    pairs
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(_, v)| (*v).clone())
        .collect()
}

/// Everything that must hold for one generated statement.
fn check(
    d: Dialect,
    q: &Query,
    expected: &[Value],
    searched: &[&Value],
    ascii: bool,
) -> Result<(), TestCaseError> {
    // (c) the parameters are exactly the bound values, in order of appearance.
    prop_assert_eq!(&q.params, expected, "{}", q.sql);
    // (d) one placeholder per parameter, numbered per dialect.
    check_placeholders(d, q)?;
    prop_assert!(!q.sql.ends_with(';'));
    prop_assert!(!q.sql.contains('\0'));
    if ascii {
        // (a) no string literal can exist.
        prop_assert!(!q.sql.contains('\''), "{}", q.sql);
        // (b) the SQL is ASCII, so no generated value text is in it.
        prop_assert!(q.sql.is_ascii(), "{}", q.sql);
        for v in searched.iter().filter(|v| searchable(v)) {
            let text = v.to_string();
            prop_assert!(!q.sql.contains(&text), "{text:?} in {}", q.sql);
        }
    }
    Ok(())
}

fn run(case: &Case, ascii: bool) -> Result<(), TestCaseError> {
    let t = &case.table;
    let set = pick(case, &case.in_set);
    let filter = pick(case, &case.in_filter);
    for d in Dialect::ALL {
        // UPDATE
        let r = update(d, t, &set, &filter);
        if set.is_empty() {
            prop_assert_eq!(r, Err(DmlError::EmptySet));
        } else if filter.is_empty() {
            prop_assert_eq!(r, Err(DmlError::EmptyFilter));
        } else {
            let q = r.unwrap();
            let mut expected: Vec<Value> = set.iter().map(|(_, v)| (*v).clone()).collect();
            expected.extend(non_null(&filter));
            let searched: Vec<&Value> = set.iter().chain(&filter).map(|(_, v)| *v).collect();
            check(d, &q, &expected, &searched, ascii)?;
            prop_assert!(q.sql.starts_with("UPDATE "));
            prop_assert!(q.sql.contains(" WHERE "));
        }
        // INSERT
        let r = insert(d, t, &set);
        if set.is_empty() {
            prop_assert_eq!(r, Err(DmlError::EmptyInsert));
        } else {
            let q = r.unwrap();
            let expected: Vec<Value> = set.iter().map(|(_, v)| (*v).clone()).collect();
            let searched: Vec<&Value> = set.iter().map(|(_, v)| *v).collect();
            check(d, &q, &expected, &searched, ascii)?;
            prop_assert!(q.sql.starts_with("INSERT INTO "));
        }
        // DELETE
        let r = delete(d, t, &filter);
        if filter.is_empty() {
            prop_assert_eq!(r, Err(DmlError::EmptyFilter));
        } else {
            let q = r.unwrap();
            let searched: Vec<&Value> = filter.iter().map(|(_, v)| *v).collect();
            check(d, &q, &non_null(&filter), &searched, ascii)?;
            prop_assert!(q.sql.starts_with("DELETE FROM "));
            prop_assert!(q.sql.contains(" WHERE "));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// ASCII identifiers: no single quote, all-ASCII SQL, no value text in the SQL.
    #[test]
    fn values_are_never_spliced(c in case(ascii_name())) {
        run(&c, true)?;
    }

    /// Arbitrary identifiers (quotes, Unicode, placeholder look-alikes): the placeholder count
    /// and order still match the parameters.
    #[test]
    fn placeholders_match_params_for_any_identifier(c in case(any_name())) {
        run(&c, false)?;
    }
}
