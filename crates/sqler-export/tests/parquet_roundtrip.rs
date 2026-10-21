// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Parquet round trip: the file is written through the public writer API and read back with the
//! `parquet` crate's own reader.

// Helpers of an integration test are outside the `allow-unwrap-in-tests` scope of clippy.toml.
#![allow(clippy::unwrap_used)]
#![cfg(feature = "parquet")]

use std::num::NonZeroUsize;

use bytes::Bytes;
use parquet::basic::Type::{BOOLEAN, BYTE_ARRAY, DOUBLE, INT32, INT64};
use parquet::basic::{LogicalType, Repetition, TimeUnit, Type as PhysicalType};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use sqler_core::{Column, DataType, Date, Decimal, Row, Time, Timestamp, TimestampTz, Value};
use sqler_export::{
    CancelToken, ExportControl, ExportError, ExportOptions, Exporter, ParquetCompression,
    ParquetFormat, export_to_vec,
};

fn options(row_group_rows: usize) -> ExportOptions {
    let mut o = ExportOptions::default();
    o.parquet.row_group_rows = NonZeroUsize::new(row_group_rows).unwrap();
    o
}

fn write(
    options: &ExportOptions,
    columns: &[Column],
    rows: &[Row],
) -> Result<Vec<u8>, ExportError> {
    export_to_vec(&ParquetFormat, options, columns, rows)
}

fn reader(bytes: Vec<u8>) -> SerializedFileReader<Bytes> {
    SerializedFileReader::new(Bytes::from(bytes)).unwrap()
}

/// Every row as the list of its fields, read through the record API.
fn read_rows(bytes: Vec<u8>) -> Vec<Vec<(String, Field)>> {
    reader(bytes)
        .get_row_iter(None)
        .unwrap()
        .map(|row| {
            row.unwrap()
                .get_column_iter()
                .map(|(n, f)| (n.clone(), f.clone()))
                .collect()
        })
        .collect()
}

fn date(y: i32, m: u8, d: u8) -> Date {
    Date::new(y, m, d).unwrap()
}

fn time(h: u8, m: u8, s: u8, ns: u32) -> Time {
    Time::new(h, m, s, ns).unwrap()
}

fn all_columns() -> Vec<Column> {
    vec![
        Column::new("flag", DataType::Boolean),
        Column::new("n", DataType::Integer),
        Column::new("x", DataType::Float),
        Column::new("price", DataType::Decimal),
        Column::new("day", DataType::Date),
        Column::new("at", DataType::Time),
        Column::new("ts", DataType::Timestamp),
        Column::new("tstz", DataType::TimestampTz),
        Column::new("raw", DataType::Binary),
        Column::new("doc", DataType::Json),
        Column::new("name", DataType::Text),
        Column::new("other", DataType::Other),
    ]
}

fn full_row(i: i64) -> Row {
    let ts = Timestamp::new(date(2024, 2, 29), time(12, 30, 45, 123_456_789));
    // 12:30:45.123456789 at +05:30 is 07:00:45.123456789 UTC.
    let tz = TimestampTz::new(ts, 5 * 3600 + 1800).unwrap();
    Row(vec![
        Value::Bool(i % 2 == 0),
        Value::Int(i),
        Value::Float(half(i)),
        Value::Decimal("123.4500".parse::<Decimal>().unwrap()),
        Value::Date(date(2024, 2, 29)),
        Value::Time(time(23, 59, 59, 999_999_999)),
        Value::Timestamp(ts),
        Value::TimestampTz(tz),
        Value::Bytes(vec![0x00, 0xFF, 0x10]),
        Value::Json(r#"{"a": [1, 2], "b": null}"#.to_owned()),
        Value::Text("zażółć gęślą jaźń \u{1F980} 日本語".to_owned()),
        Value::Text("other".to_owned()),
    ])
}

fn null_row() -> Row {
    Row(vec![Value::Null; 12])
}

#[test]
fn every_type_round_trips_with_four_row_groups() {
    let mut rows: Vec<Row> = (0..10).map(full_row).collect();
    rows[4] = null_row();
    let bytes = write(&options(3), &all_columns(), &rows).unwrap();
    assert_eq!(&bytes[..4], b"PAR1");
    assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");

    let r = reader(bytes.clone());
    let meta = r.metadata();
    assert_eq!(meta.num_row_groups(), 4);
    let counts: Vec<i64> = meta
        .row_groups()
        .iter()
        .map(parquet::file::metadata::RowGroupMetaData::num_rows)
        .collect();
    assert_eq!(counts, [3, 3, 3, 1]);
    assert_eq!(meta.file_metadata().num_rows(), 10);
    let created_by = meta.file_metadata().created_by().unwrap();
    assert!(created_by.starts_with("sqler "), "{created_by}");

    // Schema: physical and logical types, all optional.
    let schema = meta.file_metadata().schema_descr();
    let phys: Vec<PhysicalType> = (0..12).map(|i| schema.column(i).physical_type()).collect();
    assert_eq!(
        phys,
        [
            BOOLEAN, INT64, DOUBLE, BYTE_ARRAY, INT32, INT64, INT64, INT64, BYTE_ARRAY, BYTE_ARRAY,
            BYTE_ARRAY, BYTE_ARRAY
        ]
    );
    let logical: Vec<Option<LogicalType>> = (0..12)
        .map(|i| {
            schema
                .column(i)
                .self_type()
                .get_basic_info()
                .logical_type_ref()
                .cloned()
        })
        .collect();
    assert_eq!(logical[3], Some(LogicalType::String));
    assert_eq!(logical[4], Some(LogicalType::Date));
    assert_eq!(logical[5], Some(LogicalType::time(false, TimeUnit::NANOS)));
    assert_eq!(
        logical[6],
        Some(LogicalType::timestamp(false, TimeUnit::MICROS))
    );
    assert_eq!(
        logical[7],
        Some(LogicalType::timestamp(true, TimeUnit::MICROS))
    );
    assert_eq!(logical[8], None);
    assert_eq!(logical[9], Some(LogicalType::String));
    for i in 0..12 {
        assert_eq!(
            schema.column(i).self_type().get_basic_info().repetition(),
            Repetition::OPTIONAL
        );
    }

    let got = read_rows(bytes);
    assert_eq!(got.len(), 10);
    let names: Vec<&str> = got[0].iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "flag", "n", "x", "price", "day", "at", "ts", "tstz", "raw", "doc", "name", "other"
        ]
    );
    // The all-null row.
    assert!(got[4].iter().all(|(_, f)| *f == Field::Null));

    let f = |row: usize, col: usize| got[row][col].1.clone();
    for (i, row) in got.iter().enumerate() {
        if i == 4 {
            continue;
        }
        let n = i64::try_from(i).unwrap();
        assert_eq!(row[0].1, Field::Bool(n % 2 == 0));
        assert_eq!(row[1].1, Field::Long(n));
        assert_eq!(row[2].1, Field::Double(half(n)));
        // Decimal keeps its exact text and scale.
        assert_eq!(row[3].1, Field::Str("123.4500".to_owned()));
        // 2024-02-29 is day 19,782.
        assert_eq!(row[4].1, Field::Date(19_782));
        // Time keeps nanoseconds (the record API shows the raw INT64).
        assert_eq!(row[5].1, Field::Long(86_399_999_999_999));
        // 2024-02-29T12:30:45.123456 (nanoseconds below a microsecond dropped).
        assert_eq!(row[6].1, Field::TimestampMicros(1_709_209_845_123_456));
        // The instant, in UTC: 5 h 30 min earlier.
        assert_eq!(
            row[7].1,
            Field::TimestampMicros(1_709_209_845_123_456 - (5 * 3600 + 1800) * 1_000_000)
        );
        assert_eq!(row[8].1, Field::Bytes(vec![0x00, 0xFF, 0x10].into()));
        assert_eq!(
            row[9].1,
            Field::Str(r#"{"a": [1, 2], "b": null}"#.to_owned())
        );
        assert_eq!(
            row[10].1,
            Field::Str("zażółć gęślą jaźń \u{1F980} 日本語".to_owned())
        );
    }
    assert_eq!(f(9, 1), Field::Long(9));
}

#[test]
fn timestamps_before_the_epoch_and_year_one() {
    let columns = [
        Column::new("ts", DataType::Timestamp),
        Column::new("d", DataType::Date),
    ];
    let t = Timestamp::new(date(1969, 12, 31), time(23, 59, 59, 500_000_000));
    let rows = [Row(vec![Value::Timestamp(t), Value::Date(date(1, 1, 1))])];
    let got = read_rows(write(&options(10), &columns, &rows).unwrap());
    assert_eq!(got[0][0].1, Field::TimestampMicros(-500_000));
    assert_eq!(got[0][1].1, Field::Date(-719_162));
}

#[test]
fn float_column_takes_integers_and_special_floats() {
    let columns = [Column::new("x", DataType::Float)];
    let rows = [
        Row(vec![Value::Int(3)]),
        Row(vec![Value::Float(f64::INFINITY)]),
        Row(vec![Value::Float(f64::NAN)]),
    ];
    let got = read_rows(write(&options(10), &columns, &rows).unwrap());
    assert_eq!(got[0][0].1, Field::Double(3.0));
    assert_eq!(got[1][0].1, Field::Double(f64::INFINITY));
    assert!(matches!(got[2][0].1, Field::Double(x) if x.is_nan()));
}

#[test]
fn binary_column_takes_text_and_text_columns_take_any_value() {
    let columns = [
        Column::new("b", DataType::Binary),
        Column::new("t", DataType::Text),
    ];
    let rows = [Row(vec![Value::Text("héllo".into()), Value::Int(42)])];
    let got = read_rows(write(&options(10), &columns, &rows).unwrap());
    assert_eq!(
        got[0][0].1,
        Field::Bytes("héllo".as_bytes().to_vec().into())
    );
    assert_eq!(got[0][1].1, Field::Str("42".to_owned()));
}

#[test]
fn zero_rows_is_a_valid_file_with_the_schema() {
    let bytes = write(&ExportOptions::default(), &all_columns(), &[]).unwrap();
    let r = reader(bytes.clone());
    assert_eq!(r.metadata().num_row_groups(), 0);
    assert_eq!(r.metadata().file_metadata().num_rows(), 0);
    assert_eq!(
        r.metadata().file_metadata().schema_descr().num_columns(),
        12
    );
    assert_eq!(read_rows(bytes).len(), 0);
}

#[test]
fn zero_columns_is_invalid_options() {
    let err = write(&ExportOptions::default(), &[], &[]).unwrap_err();
    assert!(matches!(err, ExportError::InvalidOptions(_)), "{err}");
}

#[test]
fn duplicate_and_odd_column_names_get_unique_names() {
    let columns = [
        Column::new("a", DataType::Integer),
        Column::new("a", DataType::Integer),
        Column::new("a", DataType::Text),
        Column::new("a_2", DataType::Text),
        Column::new("", DataType::Integer),
        Column::new("a b.c", DataType::Integer),
    ];
    let rows = [Row(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Text("x".into()),
        Value::Text("y".into()),
        Value::Int(5),
        Value::Int(6),
    ])];
    let got = read_rows(write(&options(10), &columns, &rows).unwrap());
    let names: Vec<&str> = got[0].iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["a", "a_2", "a_3", "a_2_2", "", "a b.c"]);
    assert_eq!(got[0][1].1, Field::Long(2));
    assert_eq!(got[0][2].1, Field::Str("x".to_owned()));
}

#[test]
fn a_value_that_does_not_fit_fails_and_names_the_column() {
    let columns = [
        Column::new("ok", DataType::Integer),
        Column::new("when", DataType::Timestamp),
    ];
    let rows = [Row(vec![Value::Int(1), Value::Text("infinity".into())])];
    let err = write(&ExportOptions::default(), &columns, &rows).unwrap_err();
    match err {
        ExportError::Unsupported { column, reason } => {
            assert_eq!(column, "when");
            assert!(
                reason.contains("text") && reason.contains("timestamp"),
                "{reason}"
            );
        }
        other => panic!("expected Unsupported, got {other}"),
    }
}

#[test]
fn a_date_beyond_i32_days_fails() {
    let columns = [Column::new("d", DataType::Date)];
    let rows = [Row(vec![Value::Date(date(i32::MAX, 1, 1))])];
    let err = write(&ExportOptions::default(), &columns, &rows).unwrap_err();
    assert!(matches!(err, ExportError::Unsupported { .. }), "{err}");
}

#[test]
fn schema_comes_from_the_columns_even_if_the_first_batch_is_all_null() {
    let columns = [
        Column::new("n", DataType::Integer),
        Column::new("s", DataType::Text),
    ];
    let mut out = Vec::new();
    let mut e = Exporter::new(
        &ParquetFormat,
        &options(2),
        Box::new(&mut out),
        ExportControl::default(),
    )
    .unwrap();
    e.begin(&columns).unwrap();
    e.write_batch(&vec![Row(vec![Value::Null, Value::Null]); 3])
        .unwrap();
    e.write_batch(&[Row(vec![Value::Int(7), Value::Text("late".into())])])
        .unwrap();
    assert_eq!(e.finish().unwrap().rows, 4);
    let got = read_rows(out);
    assert_eq!(got.len(), 4);
    assert_eq!(got[0][0].1, Field::Null);
    assert_eq!(got[3][0].1, Field::Long(7));
    assert_eq!(got[3][1].1, Field::Str("late".to_owned()));
}

#[test]
fn uncompressed_option_round_trips() {
    let mut o = options(5);
    o.parquet.compression = ParquetCompression::Uncompressed;
    let columns = [Column::new("t", DataType::Text)];
    let rows: Vec<Row> = (0..12)
        .map(|i| Row(vec![Value::Text(format!("v{i}"))]))
        .collect();
    let bytes = write(&o, &columns, &rows).unwrap();
    let r = reader(bytes.clone());
    let chunk = r.metadata().row_group(0).column(0);
    assert_eq!(
        chunk.compression(),
        parquet::basic::Compression::UNCOMPRESSED
    );
    assert_eq!(read_rows(bytes).len(), 12);

    let snappy = write(&options(5), &columns, &rows).unwrap();
    let chunk = reader(snappy)
        .metadata()
        .row_group(0)
        .column(0)
        .compression();
    assert_eq!(chunk, parquet::basic::Compression::SNAPPY);
}

#[test]
fn large_cells_flush_a_row_group_at_32_mib() {
    // 5 cells of 9 MiB: the buffer passes 32 MiB with the 4th, so 4 rows + 1 row.
    let cell = "a".repeat(9 * 1024 * 1024);
    let columns = [Column::new("big", DataType::Text)];
    let rows: Vec<Row> = (0..5)
        .map(|_| Row(vec![Value::Text(cell.clone())]))
        .collect();
    let bytes = write(&ExportOptions::default(), &columns, &rows).unwrap();
    let r = reader(bytes);
    let counts: Vec<i64> = r
        .metadata()
        .row_groups()
        .iter()
        .map(parquet::file::metadata::RowGroupMetaData::num_rows)
        .collect();
    assert_eq!(counts, [4, 1]);
    let first = r.get_row_iter(None).unwrap().next().unwrap().unwrap();
    let (_, field) = first.get_column_iter().next().unwrap();
    assert!(matches!(field, Field::Str(s) if s.len() == 9 * 1024 * 1024));
}

#[test]
fn default_row_group_is_65536_rows() {
    let columns = [Column::new("n", DataType::Integer)];
    let rows: Vec<Row> = (0..65_537).map(|i| Row(vec![Value::Int(i)])).collect();
    let bytes = write(&ExportOptions::default(), &columns, &rows).unwrap();
    let counts: Vec<i64> = reader(bytes)
        .metadata()
        .row_groups()
        .iter()
        .map(parquet::file::metadata::RowGroupMetaData::num_rows)
        .collect();
    assert_eq!(counts, [65_536, 1]);
}

#[test]
fn cancel_leaves_no_footer() {
    let columns = [Column::new("n", DataType::Integer)];
    let token = CancelToken::new();
    let control = ExportControl {
        cancel: token.clone(),
        progress: None,
    };
    let mut out = Vec::new();
    let mut e = Exporter::new(&ParquetFormat, &options(2), Box::new(&mut out), control).unwrap();
    e.begin(&columns).unwrap();
    e.write_batch(&vec![Row(vec![Value::Int(1)]); 5]).unwrap();
    token.cancel();
    assert!(matches!(
        e.write_batch(&[Row(vec![Value::Int(1)])]),
        Err(ExportError::Cancelled)
    ));
    assert!(matches!(e.finish(), Err(ExportError::Cancelled)));
    // Row groups written before the cancel are in the sink, but the footer is not.
    assert_eq!(&out[..4], b"PAR1");
    assert_ne!(&out[out.len().saturating_sub(4)..], b"PAR1");
    assert!(SerializedFileReader::new(Bytes::from(out)).is_err());
}

fn half(i: i64) -> f64 {
    f64::from(i32::try_from(i).unwrap()) + 0.5
}

#[test]
fn a_failed_write_locks_the_writer() {
    let columns = [
        Column::new("when", DataType::Timestamp),
        Column::new("n", DataType::Integer),
    ];
    let mut out = Vec::new();
    let mut e = Exporter::new(
        &ParquetFormat,
        &ExportOptions::default(),
        Box::new(&mut out),
        ExportControl::default(),
    )
    .unwrap();
    e.begin(&columns).unwrap();
    let bad = Row(vec![Value::Text("infinity".into()), Value::Int(1)]);
    assert!(matches!(
        e.write_batch(&[bad]),
        Err(ExportError::Unsupported { .. })
    ));
    let ok = Row(vec![Value::Null, Value::Int(2)]);
    assert!(matches!(
        e.write_batch(std::slice::from_ref(&ok)),
        Err(ExportError::State(_))
    ));
    assert!(e.finish().is_err());
}
