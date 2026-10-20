// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! One snapshot per format over a fixture that holds every `Value` variant and the awkward cases.
//!
//! In the snapshot files NUL is shown as `<NUL>` and CR as `<CR>`, so the files stay text.

// Helpers of an integration test are outside the `allow-unwrap-in-tests` scope of clippy.toml.
#![allow(clippy::unwrap_used)]

use std::num::NonZeroUsize;

use sqler_core::{Column, DataType, Row, Value};
use sqler_export::{
    CsvFormat, ExportOptions, Format, JsonFormat, LineEnding, MarkdownFormat, SqlFormat,
    export_to_vec,
};
use sqler_sql::Dialect;

fn columns() -> Vec<Column> {
    vec![
        Column::new("label", DataType::Text),
        Column::new("value", DataType::Other),
        Column::new("value", DataType::Other),
        Column::new("odd \"name\" `x` | ,", DataType::Text),
    ]
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

fn parse<T: std::str::FromStr>(s: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    s.parse().unwrap()
}

/// `(label, value)` pairs: every variant, with awkward values.
fn cases() -> Vec<(&'static str, Value)> {
    vec![
        ("null", Value::Null),
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("int zero", Value::Int(0)),
        ("int min", Value::Int(i64::MIN)),
        ("int max", Value::Int(i64::MAX)),
        ("float", Value::Float(1.5)),
        ("float neg zero", Value::Float(-0.0)),
        ("float large", Value::Float(1e300)),
        ("float nan", Value::Float(f64::NAN)),
        ("float +inf", Value::Float(f64::INFINITY)),
        ("float -inf", Value::Float(f64::NEG_INFINITY)),
        ("text empty", text("")),
        ("text plain", text("plain")),
        ("text quote", text("it's")),
        ("text dquote", text("say \"hi\"")),
        ("text backslash", text("back\\slash")),
        ("text pipe", text("a|b")),
        ("text comma", text("a,b")),
        ("text crlf", text("line1\r\nline2")),
        ("text lf", text("line1\nline2")),
        ("text nul", text("nul\0byte")),
        ("text unicode", text("h\u{e9}llo \u{540d}\u{524d}")),
        ("text comment", text("x'; DROP TABLE t; --")),
        ("text formula", text("=1+1")),
        ("text null word", text("NULL")),
        ("bytes empty", Value::Bytes(vec![])),
        ("bytes", Value::Bytes(vec![0x00, 0xab, 0xff])),
        ("decimal", Value::Decimal(parse("-12.50"))),
        ("date", Value::Date(parse("2024-01-02"))),
        ("date far", Value::Date(parse("+12345-06-07"))),
        ("date neg", Value::Date(parse("-0044-03-15"))),
        ("time", Value::Time(parse("03:04:05.123456789"))),
        ("timestamp", Value::Timestamp(parse("2024-01-02 03:04:05"))),
        (
            "timestamptz",
            Value::TimestampTz(parse("2024-01-02 03:04:05-08:30")),
        ),
        (
            "json ok",
            Value::Json("{\"a\": [1, 2, {\"b\": null}]}".into()),
        ),
        ("json bad", Value::Json("{not json".into())),
    ]
}

fn rows() -> Vec<Row> {
    cases()
        .into_iter()
        .map(|(label, value)| Row(vec![text(label), value, Value::Null, text("x")]))
        .collect()
}

fn render(format: &dyn Format, options: &ExportOptions) -> String {
    let bytes = export_to_vec(format, options, &columns(), &rows()).unwrap();
    String::from_utf8(bytes)
        .unwrap()
        .replace('\0', "<NUL>")
        .replace('\r', "<CR>")
}

fn sql_options(dialect: Dialect) -> ExportOptions {
    let mut o = ExportOptions::default();
    o.sql.dialect = dialect;
    o.sql.schema = Some("my schema".into());
    "my table".clone_into(&mut o.sql.table);
    o.sql.rows_per_statement = NonZeroUsize::new(2).unwrap();
    o
}

#[test]
fn csv() {
    insta::assert_snapshot!(render(&CsvFormat, &ExportOptions::default()));
}

#[test]
fn csv_semicolon_no_header_null() {
    let mut o = ExportOptions::default();
    o.csv.delimiter = b';';
    o.csv.header = false;
    "NULL".clone_into(&mut o.csv.null_text);
    o.csv.line_ending = LineEnding::Lf;
    insta::assert_snapshot!(render(&CsvFormat, &o));
}

#[test]
fn csv_neutralized() {
    let mut o = ExportOptions::default();
    o.csv.neutralize_formulas = true;
    insta::assert_snapshot!(render(&CsvFormat, &o));
}

#[test]
fn json() {
    insta::assert_snapshot!(render(&JsonFormat, &ExportOptions::default()));
}

#[test]
fn markdown() {
    insta::assert_snapshot!(render(&MarkdownFormat, &ExportOptions::default()));
}

#[test]
fn sql_postgres() {
    insta::assert_snapshot!(render(&SqlFormat, &sql_options(Dialect::Postgres)));
}

#[test]
fn sql_mysql() {
    insta::assert_snapshot!(render(&SqlFormat, &sql_options(Dialect::MySql)));
}

#[test]
fn sql_sqlite() {
    insta::assert_snapshot!(render(&SqlFormat, &sql_options(Dialect::Sqlite)));
}

#[test]
fn json_output_parses() {
    let out = export_to_vec(&JsonFormat, &ExportOptions::default(), &columns(), &rows()).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let list = v.as_array().unwrap();
    assert_eq!(list.len(), cases().len());
    // Duplicate column names are made unique and JSON columns are embedded.
    assert!(list[0].get("value_2").is_some());
    let ok = list.iter().find(|o| o["label"] == "json ok").unwrap();
    assert_eq!(ok["value"]["a"][2]["b"], serde_json::Value::Null);
    let bad = list.iter().find(|o| o["label"] == "json bad").unwrap();
    assert_eq!(bad["value"], "{not json");
}
