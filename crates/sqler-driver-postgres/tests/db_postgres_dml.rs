// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Docker-gated checks that the bound DML of `sqler_sql::dml` (staged grid edits, ADR-0030) writes
//! exactly what was staged on a real `PostgreSQL` server (see AGENTS.md §3). The statements are
//! generated for the table the driver's introspection reports, so the native type spellings are
//! the server's own.

#![allow(clippy::unwrap_used)]

mod common;

use sqler_driver_api::Session;
use sqler_driver_api::sqler_core::{DbObject, ObjectKind, ObjectPath, Table, Value};
use sqler_sql::{Dialect, delete, insert, update};

use common::{connect, fresh_schema, query, run, sql, url};

async fn describe(s: &mut dyn Session, schema: &str, name: &str) -> Table {
    let object = DbObject::new(
        ObjectKind::Table,
        ObjectPath::root().in_schema(schema),
        name,
    );
    s.introspection()
        .unwrap()
        .table(&object)
        .await
        .unwrap()
        .unwrap()
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// The text of column `col` of the single row of `e`.
async fn read(s: &mut dyn Session, col: &str) -> Value {
    let rs = sql(s, &format!("select {col}::text from e where id = 1")).await;
    rs.rows()[0].0[0].clone()
}

/// An explicit cast to `character varying(5)`, `character(3)`, `bit(4)` or `numeric(5,2)`
/// truncates, pads or rounds silently; the generator must leave the column's own assignment check
/// in place, so a value that does not fit is an error and the row keeps its data.
#[test]
fn values_that_do_not_fit_are_errors_not_truncated() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_dml_fit").await;
        sql(
            &mut *s,
            "create table e (id bigint primary key, v varchar(5), c char(3), b bit(4), \
             n numeric(5,2))",
        )
        .await;
        sql(&mut *s, "insert into e values (1, 'x', 'x', B'0000', 1)").await;
        let table = describe(&mut *s, "sqler_t_dml_fit", "e").await;
        let id = Value::Int(1);

        let too_long = [
            ("v", text("123456")),
            ("c", text("abcd")),
            ("b", text("101")),
            ("b", text("10101")),
            ("n", Value::Decimal("12345.67".parse().unwrap())),
        ];
        for (col, value) in &too_long {
            let q = update(Dialect::Postgres, &table, &[(col, value)], &[("id", &id)]).unwrap();
            let error = query(&mut *s, &q).await.unwrap_err().to_string();
            assert!(!error.is_empty(), "{col}");
        }
        // Nothing was written by the failed statements.
        assert_eq!(read(&mut *s, "v").await, text("x"));
        assert_eq!(read(&mut *s, "c").await, text("x"));
        assert_eq!(read(&mut *s, "b").await, text("0000"));
        assert_eq!(read(&mut *s, "n").await, text("1.00"));

        // INSERT goes through the same casts.
        let nine = text("123456789");
        let q = insert(
            Dialect::Postgres,
            &table,
            &[("id", &Value::Int(2)), ("v", &nine)],
        )
        .unwrap();
        assert!(query(&mut *s, &q).await.is_err());
        let rs = sql(&mut *s, "select count(*) from e").await;
        assert_eq!(rs.rows()[0].0[0], Value::Int(1));

        // A NULL goes through CAST(CAST($k AS text) AS T) without the modifier.
        let q = update(
            Dialect::Postgres,
            &table,
            &[("v", &Value::Null), ("n", &Value::Null)],
            &[("id", &id)],
        )
        .unwrap();
        assert_eq!(query(&mut *s, &q).await.unwrap().rows_affected(), Some(1));
        assert_eq!(read(&mut *s, "v").await, Value::Null);

        // Values that fit are written: the length check is the column's.
        let fits = [
            ("v", text("12345"), text("12345")),
            ("c", text("ab"), text("ab")),
            ("b", text("1010"), text("1010")),
            (
                "n",
                Value::Decimal("123.45".parse().unwrap()),
                text("123.45"),
            ),
        ];
        for (col, value, expected) in &fits {
            let q = update(Dialect::Postgres, &table, &[(col, value)], &[("id", &id)]).unwrap();
            let done = query(&mut *s, &q).await.unwrap();
            assert_eq!(done.rows_affected(), Some(1), "{col}");
            assert_eq!(read(&mut *s, col).await, *expected, "{col}");
        }
        // A filter on a char(n) column matches the padded value.
        let q = delete(
            Dialect::Postgres,
            &table,
            &[("c", &text("ab")), ("v", &text("12345"))],
        )
        .unwrap();
        assert_eq!(query(&mut *s, &q).await.unwrap().rows_affected(), Some(1));
        s.close().await.unwrap();
    });
}

/// An enum whose name needs quotes (`"Mixed Enum"`) is cast with the name re-quoted, schema
/// qualified or not.
#[test]
fn enum_types_with_quoted_names_can_be_written() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_dml_enum").await;
        sql(
            &mut *s,
            "create type \"Mixed Enum\" as enum ('a b', 'c''d', 'e\"f')",
        )
        .await;
        sql(&mut *s, "drop schema if exists \"Odd Schema dml\" cascade").await;
        sql(&mut *s, "create schema \"Odd Schema dml\"").await;
        sql(
            &mut *s,
            "create type \"Odd Schema dml\".\"Other Enum\" as enum ('x', 'y')",
        )
        .await;
        sql(
            &mut *s,
            "create table e (id bigint primary key, m \"Mixed Enum\", o \"Odd Schema dml\".\"Other Enum\")",
        )
        .await;
        sql(&mut *s, "insert into e values (1, 'a b', 'x')").await;
        let table = describe(&mut *s, "sqler_t_dml_enum", "e").await;
        let id = Value::Int(1);
        for (col, value) in [
            ("m", "c'd"),
            ("m", "e\"f"),
            ("m", "a b"),
            ("o", "y"),
            ("o", "x"),
        ] {
            let value = text(value);
            let q = update(Dialect::Postgres, &table, &[(col, &value)], &[("id", &id)]).unwrap();
            let done = query(&mut *s, &q).await.unwrap();
            assert_eq!(done.rows_affected(), Some(1), "{col} {value:?}");
            assert_eq!(read(&mut *s, col).await, value);
            // The filter on the same value matches the row again.
            let q = update(
                Dialect::Postgres,
                &table,
                &[("id", &Value::Int(1))],
                &[(col, &value)],
            )
            .unwrap();
            assert_eq!(query(&mut *s, &q).await.unwrap().rows_affected(), Some(1));
        }
        let bad = text("nope");
        let q = update(Dialect::Postgres, &table, &[("m", &bad)], &[("id", &id)]).unwrap();
        assert!(query(&mut *s, &q).await.is_err());
        sql(&mut *s, "drop schema \"Odd Schema dml\" cascade").await;
        s.close().await.unwrap();
    });
}

/// A domain over `varchar(n)` / `char(n)` is reported as a `Text` column with the domain's name.
/// An explicit cast to the domain would truncate like `varchar(5)` does; the generator casts to
/// the logical type, so the server's assignment rules raise an error and the row keeps its data.
#[test]
fn domains_over_sized_strings_are_errors_not_truncated() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_dml_dom").await;
        sql(&mut *s, "create domain dv as varchar(5)").await;
        sql(&mut *s, "create domain dc as char(3)").await;
        sql(&mut *s, "create domain \"Odd Dom\" as varchar(4)").await;
        sql(&mut *s, "create domain dpos as integer check (value > 0)").await;
        sql(
            &mut *s,
            "create table e (id bigint primary key, v dv, c dc, q \"Odd Dom\", va dv[], \
             qa \"Odd Dom\"[], p dpos)",
        )
        .await;
        sql(
            &mut *s,
            "insert into e values (1, 'abc', 'abc', 'abc', '{ab}', '{ab}', 1)",
        )
        .await;
        let table = describe(&mut *s, "sqler_t_dml_dom", "e").await;
        let id = Value::Int(1);

        for (col, value) in [
            ("v", "1234567"),
            ("c", "abcdef"),
            ("q", "abcdefg"),
            ("va", "{1234567}"),
            ("qa", "{abcdefg}"),
        ] {
            let value = text(value);
            let q = update(Dialect::Postgres, &table, &[(col, &value)], &[("id", &id)]).unwrap();
            let error = query(&mut *s, &q).await.unwrap_err().to_string();
            assert!(error.contains("too long"), "{col}: {error}");
        }
        let q = insert(
            Dialect::Postgres,
            &table,
            &[("id", &Value::Int(2)), ("v", &text("1234567"))],
        )
        .unwrap();
        assert!(query(&mut *s, &q).await.is_err());
        // Nothing was written.
        assert_eq!(read(&mut *s, "v").await, text("abc"));
        assert_eq!(read(&mut *s, "c").await, text("abc"));
        assert_eq!(read(&mut *s, "q").await, text("abc"));
        assert_eq!(read(&mut *s, "va").await, text("{ab}"));
        assert_eq!(read(&mut *s, "qa").await, text("{ab}"));
        let rs = sql(&mut *s, "select count(*) from e").await;
        assert_eq!(rs.rows()[0].0[0], Value::Int(1));

        // Values that fit are written; the domain's check still applies.
        for (col, value, expected) in [
            ("v", "12345", "12345"),
            ("c", "ab", "ab"),
            ("q", "abcd", "abcd"),
            ("va", "{12345}", "{12345}"),
        ] {
            let value = text(value);
            let q = update(Dialect::Postgres, &table, &[(col, &value)], &[("id", &id)]).unwrap();
            assert_eq!(query(&mut *s, &q).await.unwrap().rows_affected(), Some(1));
            assert_eq!(read(&mut *s, col).await, text(expected), "{col}");
        }
        let q = update(
            Dialect::Postgres,
            &table,
            &[("p", &Value::Int(-1))],
            &[("id", &id)],
        )
        .unwrap();
        assert!(query(&mut *s, &q).await.is_err());
        s.close().await.unwrap();
    });
}

/// A text parameter is not read by the fields of an `interval year` column (`1` becomes
/// `00:00:00`), and `"char"` keeps one byte: the generator refuses to write both, and an
/// `interval` without fields still takes a bare number.
#[test]
fn intervals_with_fields_and_one_byte_chars_are_refused() {
    let Some(url) = url() else { return };
    run(async {
        let mut s = connect(&url).await;
        fresh_schema(&mut *s, "sqler_t_dml_iv").await;
        sql(
            &mut *s,
            "create table e (id bigint primary key, y interval year, d interval day to second(3), \
             i interval(2), c \"char\")",
        )
        .await;
        sql(
            &mut *s,
            "insert into e values (1, '2 years', '1 day', '1 hour', 'a')",
        )
        .await;
        let table = describe(&mut *s, "sqler_t_dml_iv", "e").await;
        let id = Value::Int(1);
        for col in ["y", "d", "c"] {
            let value = text("1");
            let result = update(Dialect::Postgres, &table, &[(col, &value)], &[("id", &id)]);
            assert!(
                matches!(result, Err(sqler_sql::DmlError::UnwritableType { .. })),
                "{col}"
            );
        }
        assert_eq!(read(&mut *s, "y").await, text("2 years"));
        assert_eq!(read(&mut *s, "c").await, text("a"));
        // Without fields a bare number is seconds, on the server as in the generator.
        let q = update(
            Dialect::Postgres,
            &table,
            &[("i", &text("90"))],
            &[("id", &id)],
        )
        .unwrap();
        assert_eq!(query(&mut *s, &q).await.unwrap().rows_affected(), Some(1));
        assert_eq!(read(&mut *s, "i").await, text("00:01:30"));
        s.close().await.unwrap();
    });
}
