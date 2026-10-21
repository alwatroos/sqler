// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `XLSX` writer read back with `calamine`.

#![cfg(feature = "xlsx")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::io::Cursor;

use calamine::{Data, Reader, Xlsx, open_workbook_from_rs};
use sqler_core::{Column, DataType, Date, Decimal, Row, Time, Timestamp, TimestampTz, Value};
use sqler_export::{
    ExportControl, ExportError, ExportOptions, Exporter, Format, XlsxFormat, export_to_vec,
};

fn read(bytes: Vec<u8>) -> calamine::Range<Data> {
    let mut book: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes)).unwrap();
    assert_eq!(book.sheet_names(), ["Sheet1"]);
    book.worksheet_range("Sheet1").unwrap()
}

fn export(columns: &[Column], rows: &[Row]) -> Vec<u8> {
    export_to_vec(&XlsxFormat, &ExportOptions::default(), columns, rows).unwrap()
}

fn date(y: i32, m: u8, d: u8) -> Date {
    Date::new(y, m, d).unwrap()
}

fn time(h: u8, m: u8, s: u8, n: u32) -> Time {
    Time::new(h, m, s, n).unwrap()
}

fn text(range: &calamine::Range<Data>, row: u32, col: u32) -> String {
    match range.get_value((row, col)).unwrap() {
        Data::String(s) => s.clone(),
        other => panic!("not a string at ({row},{col}): {other:?}"),
    }
}

fn serial(range: &calamine::Range<Data>, row: u32, col: u32) -> f64 {
    match range.get_value((row, col)).unwrap() {
        Data::DateTime(d) => {
            assert!(d.is_datetime());
            d.as_f64()
        }
        other => panic!("not a date at ({row},{col}): {other:?}"),
    }
}

#[test]
fn typed_cells_and_header() {
    let columns = [
        Column::new("id", DataType::Integer),
        Column::new("price", DataType::Float),
        Column::new("ok", DataType::Boolean),
        Column::new("day", DataType::Date),
        Column::new("at", DataType::Timestamp),
        Column::new("tz", DataType::TimestampTz),
        Column::new("clock", DataType::Time),
        Column::new("name", DataType::Text),
        Column::new("dec", DataType::Decimal),
        Column::new("blob", DataType::Binary),
    ];
    let local = Timestamp::new(date(2024, 3, 5), time(13, 45, 30, 0));
    let tz = TimestampTz::new(
        Timestamp::new(date(2024, 3, 5), time(18, 15, 30, 0)),
        5 * 3600 + 30 * 60,
    )
    .unwrap();
    let rows = [
        Row(vec![
            Value::Int(7),
            Value::Float(2.5),
            Value::Bool(true),
            Value::Date(date(2024, 3, 5)),
            Value::Timestamp(local),
            Value::TimestampTz(tz),
            Value::Time(time(6, 0, 0, 0)),
            Value::Text("héllo <&> \"q\"".into()),
            Value::Decimal("123.4500".parse::<Decimal>().unwrap()),
            Value::Bytes(vec![0, 0xab]),
        ]),
        Row(vec![Value::Null; 10]),
    ];
    let range = read(export(&columns, &rows));
    // calamine drops trailing rows without cells.
    assert_eq!(range.height(), 2);
    let headers: Vec<_> = (0..10).map(|c| text(&range, 0, c)).collect();
    assert_eq!(
        headers,
        [
            "id", "price", "ok", "day", "at", "tz", "clock", "name", "dec", "blob"
        ]
    );
    assert!(matches!(
        range.get_value((1, 0)),
        Some(Data::Int(7) | Data::Float(7.0))
    ));
    assert_eq!(range.get_value((1, 1)), Some(&Data::Float(2.5)));
    assert_eq!(range.get_value((1, 2)), Some(&Data::Bool(true)));
    // 2024-03-05 is serial 45356.
    assert!((serial(&range, 1, 3) - 45_356.0).abs() < 1e-9);
    let secs = |h: f64, m: f64, s: f64| (h * 3600.0 + m * 60.0 + s) / 86_400.0;
    assert!((serial(&range, 1, 4) - (45_356.0 + secs(13.0, 45.0, 30.0))).abs() < 1e-6);
    // 18:15:30 at +05:30 is 12:45:30 UTC.
    assert!((serial(&range, 1, 5) - (45_356.0 + secs(12.0, 45.0, 30.0))).abs() < 1e-6);
    assert!((serial(&range, 1, 6) - 0.25).abs() < 1e-9);
    assert_eq!(text(&range, 1, 7), "héllo <&> \"q\"");
    assert_eq!(range.get_value((1, 8)), Some(&Data::Float(123.45)));
    assert_eq!(text(&range, 1, 9), "0x00ab");
    for c in 0..10 {
        assert!(range.get_value((2, c)).is_none_or(|v| *v == Data::Empty));
    }
}

#[test]
fn formula_like_text_stays_text() {
    let columns = [Column::new("v", DataType::Text)];
    let values = [
        "=1+1",
        "+x",
        "-x",
        "@x",
        "=HYPERLINK(\"http://x\")",
        "\tTab",
        "-5",
    ];
    let rows: Vec<_> = values
        .iter()
        .map(|v| Row(vec![Value::Text((*v).to_owned())]))
        .collect();
    let range = read(export(&columns, &rows));
    for (i, v) in values.iter().enumerate() {
        assert_eq!(text(&range, u32::try_from(i).unwrap() + 1, 0), *v);
    }
}

#[test]
fn formula_like_header_stays_text() {
    let columns = [Column::new("=1+1", DataType::Text)];
    let range = read(export(&columns, &[]));
    assert_eq!(text(&range, 0, 0), "=1+1");
}

#[test]
fn awkward_characters_survive() {
    let columns = [Column::new("v", DataType::Text)];
    let values = [
        "line1\nline2",
        "a\tb",
        "  padded  ",
        "_x0041_",
        "\u{1}ctl\u{1f}",
        "😀",
        "",
    ];
    let rows: Vec<_> = values
        .iter()
        .map(|v| Row(vec![Value::Text((*v).to_owned())]))
        .collect();
    let range = read(export(&columns, &rows));
    for (i, v) in values.iter().enumerate() {
        let got = range.get_value((u32::try_from(i).unwrap() + 1, 0));
        if v.is_empty() {
            assert!(got.is_none_or(|d| match d {
                Data::String(s) => s.is_empty(),
                other => *other == Data::Empty,
            }));
        } else {
            assert_eq!(text(&range, u32::try_from(i).unwrap() + 1, 0), *v);
        }
    }
}

#[test]
fn big_integers_and_non_finite_floats_are_text() {
    let columns = [
        Column::new("a", DataType::Integer),
        Column::new("b", DataType::Integer),
        Column::new("c", DataType::Float),
        Column::new("d", DataType::Float),
        Column::new("e", DataType::Decimal),
    ];
    let rows = [Row(vec![
        Value::Int(i64::MAX),
        Value::Int(-(1 << 53)),
        Value::Float(f64::NAN),
        Value::Float(f64::NEG_INFINITY),
        Value::Decimal("3.14159265358979323846".parse::<Decimal>().unwrap()),
    ])];
    let range = read(export(&columns, &rows));
    assert_eq!(text(&range, 1, 0), "9223372036854775807");
    assert_eq!(
        range.get_value((1, 1)),
        Some(&Data::Float(-9_007_199_254_740_992.0))
    );
    assert_eq!(text(&range, 1, 2), "NaN");
    assert_eq!(text(&range, 1, 3), "-Infinity");
    assert_eq!(text(&range, 1, 4), "3.14159265358979323846");
}

#[test]
fn out_of_range_dates_are_iso_text() {
    let columns = [Column::new("d", DataType::Date)];
    let rows = [
        Row(vec![Value::Date(date(1899, 12, 31))]),
        Row(vec![Value::Date(date(1900, 3, 1))]),
    ];
    let range = read(export(&columns, &rows));
    assert_eq!(text(&range, 1, 0), "1899-12-31");
    assert!((serial(&range, 2, 0) - 61.0).abs() < 1e-9);
}

#[test]
fn row_limit_from_a_cheap_stream() {
    let columns = [Column::new("n", DataType::Integer)];
    let chunk = vec![Row(vec![Value::Null]); 1024];
    let mut out = Vec::new();
    let mut exporter = Exporter::new(
        &XlsxFormat,
        &ExportOptions::default(),
        Box::new(&mut out),
        ExportControl::default(),
    )
    .unwrap();
    exporter.begin(&columns).unwrap();
    let mut written = 0_usize;
    let err = loop {
        match exporter.write_batch(&chunk) {
            Ok(()) => written += chunk.len(),
            Err(e) => break e,
        }
    };
    assert!(
        matches!(&err, ExportError::Limit(m) if m.contains("CSV or Parquet")),
        "{err}"
    );
    // The chunk that would pass the limit was refused whole.
    assert_eq!(written, 1_048_575 / 1024 * 1024);
}

#[test]
fn exactly_the_row_limit_is_accepted() {
    let columns = [Column::new("n", DataType::Integer)];
    let empty = vec![Row(vec![Value::Null]); 1023];
    let mut out = Vec::new();
    let mut exporter = Exporter::new(
        &XlsxFormat,
        &ExportOptions::default(),
        Box::new(&mut out),
        ExportControl::default(),
    )
    .unwrap();
    exporter.begin(&columns).unwrap();
    // 1,048,575 = 1024 * 1023 + 1023; the last row holds the only value.
    for _ in 0..1024 {
        exporter.write_batch(&empty).unwrap();
    }
    let mut last = empty;
    last[1022] = Row(vec![Value::Int(99)]);
    exporter.write_batch(&last).unwrap();
    exporter.finish().unwrap();
    let range = read(out);
    assert_eq!(range.height(), 1_048_576);
    assert!(matches!(
        range.get_value((1_048_575, 0)),
        Some(Data::Int(99) | Data::Float(99.0))
    ));
}

#[test]
fn cell_length_error() {
    let columns = [Column::new("t", DataType::Text)];
    let rows = [Row(vec![Value::Text("x".repeat(32_768))])];
    let err = export_to_vec(&XlsxFormat, &ExportOptions::default(), &columns, &rows).unwrap_err();
    assert!(
        matches!(&err, ExportError::Limit(m) if m.contains("32767")),
        "{err}"
    );
    let ok = [Row(vec![Value::Text("x".repeat(32_767))])];
    let range = read(export(&columns, &ok));
    assert_eq!(text(&range, 1, 0).len(), 32_767);
}

#[test]
fn column_limit() {
    let many: Vec<_> = (0..16_385)
        .map(|i| Column::new(format!("c{i}"), DataType::Integer))
        .collect();
    let err = export_to_vec(&XlsxFormat, &ExportOptions::default(), &many, &[]).unwrap_err();
    assert!(matches!(err, ExportError::Limit(_)));
    let range = read(export(&many[..16_384], &[]));
    assert_eq!(range.width(), 16_384);
    assert_eq!(text(&range, 0, 16_383), "c16383");
}

#[test]
fn zero_rows_and_format_metadata() {
    let columns = [Column::new("a", DataType::Text)];
    let range = read(export(&columns, &[]));
    assert_eq!((range.height(), range.width()), (1, 1));
    assert_eq!(XlsxFormat.extension(), "xlsx");
    assert_eq!(XlsxFormat.id(), "xlsx");
}
