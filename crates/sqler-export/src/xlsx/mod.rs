// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Excel writer: one worksheet in a streamed `.xlsx` package.
//!
//! Row 1 is the header in bold. Text is always an inline string and never a formula: no cell
//! carries an `<f>` element, and text that starts with `=`, `+`, `-`, `@`, a tab or a carriage
//! return gets the quote-prefix style, so Excel keeps it as text when it is edited too.
//! Excel's limits (16,384 columns, 1,048,576 rows with the header, 32,767 characters per cell)
//! and the 4 GiB of a zip without `ZIP64` are errors, never a silent cut.
//!
//! Numbers keep every digit: integers beyond 2^53 and decimals of more than 15 significant
//! digits become text. Dates, times and timestamps are serial numbers with a number format;
//! the ones before 1900-03-01 (Excel's calendar starts there without its leap-year bug) and
//! after 9999-12-31 stay ISO text. The time of day is cut down to whole milliseconds (never
//! rounded up, so 23:59:59.9999 stays on its day and 9999-12-31 stays in Excel's range). A timestamp with an offset is stored as the UTC instant.
//! NaN and the infinities are the texts `NaN`, `Infinity` and `-Infinity`.

mod xml;
mod zip;

use std::fmt::Write as _;

use sqler_core::{Column, Row, Time, Timestamp, Value};

use self::xml::{
    STYLE_DATE, STYLE_DATETIME, STYLE_HEADER, STYLE_HEADER_QUOTE, STYLE_QUOTE, STYLE_TIME,
    column_letters, push_escaped,
};
use self::zip::ZipWriter;
use crate::render::unique_names;
use crate::temporal::{days_from_civil, nanos_of_day, utc_micros};
use crate::{ExportError, ExportOptions, Format, RowWriter, Sink, XLSX};

/// Columns in a worksheet.
const MAX_COLUMNS: usize = 16_384;
/// Rows of data: a worksheet has 1,048,576 rows and the header takes one.
const MAX_DATA_ROWS: u64 = 1_048_575;
/// UTF-16 units in a cell.
const MAX_CELL_UNITS: usize = 32_767;
/// Integers up to this magnitude are exact in an Excel number (a double).
const MAX_EXACT_INT: i64 = 1 << 53;
/// Decimals up to this many significant digits survive a double.
const MAX_DECIMAL_DIGITS: usize = 15;
/// Excel serial of 1970-01-01.
const EPOCH_SERIAL: i64 = 25_569;
/// 1900-03-01 and 9999-12-31 as days since 1970-01-01.
const FIRST_DAY: i64 = -25_508;
const LAST_DAY: i64 = 2_932_896;
const NANOS_PER_DAY: f64 = 86_400_000_000_000.0;
const MICROS_PER_DAY: i64 = 86_400_000_000;
/// The row XML is handed to the zip when it grows past this.
const FLUSH_BYTES: usize = 64 * 1024;

/// The Excel format.
#[derive(Debug, Clone, Copy, Default)]
pub struct XlsxFormat;

impl Format for XlsxFormat {
    fn id(&self) -> &'static str {
        XLSX
    }

    fn label(&self) -> &'static str {
        "Excel"
    }

    fn extension(&self) -> &'static str {
        "xlsx"
    }

    fn create<'a>(
        &self,
        _options: &ExportOptions,
        sink: Sink<'a>,
    ) -> Result<Box<dyn RowWriter + 'a>, ExportError> {
        Ok(Box::new(XlsxWriter::new(ZipWriter::new(sink))))
    }
}

struct ColumnInfo {
    letters: String,
    name: String,
}

struct XlsxWriter<W: std::io::Write + Send> {
    zip: Option<ZipWriter<W>>,
    columns: Vec<ColumnInfo>,
    rows: u64,
    began: bool,
    /// Set by the first error; later calls fail instead of finishing a damaged sheet.
    failed: bool,
    /// The sheet XML not yet handed to the zip; reused.
    buf: String,
    scratch: String,
}

fn limit(message: impl Into<String>) -> ExportError {
    ExportError::Limit(format!(
        "{}; export to CSV or Parquet instead",
        message.into()
    ))
}

impl<W: std::io::Write + Send> XlsxWriter<W> {
    fn new(zip: ZipWriter<W>) -> Self {
        Self {
            zip: Some(zip),
            columns: Vec::new(),
            rows: 0,
            began: false,
            failed: false,
            buf: String::new(),
            scratch: String::new(),
        }
    }

    fn zip(&mut self) -> Result<&mut ZipWriter<W>, ExportError> {
        self.zip
            .as_mut()
            .ok_or(ExportError::State("the XLSX writer is finished"))
    }

    fn flush_buf(&mut self) -> Result<(), ExportError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        let result = self.zip()?.write_data(buf.as_bytes());
        self.buf = buf;
        self.buf.clear();
        result
    }
}

/// Whether a spreadsheet could take the text for a formula.
fn starts_like_formula(text: &str) -> bool {
    text.starts_with(['=', '+', '-', '@', '\t', '\r'])
}

/// Appends `<c r=".." t="inlineStr" ..><is>..</is></c>`.
fn push_text_cell(
    out: &mut String,
    reference: &str,
    column: &str,
    text: &str,
    header: bool,
) -> Result<(), ExportError> {
    if text.len() > MAX_CELL_UNITS && text.encode_utf16().count() > MAX_CELL_UNITS {
        return Err(limit(format!(
            "column {column:?} has a text of more than {MAX_CELL_UNITS} characters, the most an Excel cell holds"
        )));
    }
    let style = match (header, starts_like_formula(text)) {
        (true, true) => STYLE_HEADER_QUOTE,
        (true, false) => STYLE_HEADER,
        (false, true) => STYLE_QUOTE,
        (false, false) => 0,
    };
    out.push_str("<c r=\"");
    out.push_str(reference);
    out.push_str("\" t=\"inlineStr\"");
    if style != 0 {
        let _ = write!(out, " s=\"{style}\"");
    }
    out.push_str("><is><t xml:space=\"preserve\">");
    push_escaped(out, text);
    out.push_str("</t></is></c>");
    Ok(())
}

fn push_number_cell(out: &mut String, reference: &str, style: u8, number: impl std::fmt::Display) {
    let _ = write!(out, "<c r=\"{reference}\"");
    if style != 0 {
        let _ = write!(out, " s=\"{style}\"");
    }
    let _ = write!(out, "><v>{number}</v></c>");
}

/// The number of significant digits of a decimal text (leading zeros do not count).
fn significant_digits(decimal: &str) -> usize {
    let digits = decimal.trim_start_matches(['+', '-']);
    let digits: &str = digits.trim_start_matches(['0', '.']).trim_end_matches('.');
    digits.bytes().filter(u8::is_ascii_digit).count()
}

/// Appends a decimal as a number when a double holds it exactly enough, else as exact text.
fn push_decimal(
    out: &mut String,
    reference: &str,
    column: &str,
    text: &str,
) -> Result<(), ExportError> {
    let fraction_len = text.split_once('.').map_or(0, |(_, f)| f.len());
    if significant_digits(text) <= MAX_DECIMAL_DIGITS && fraction_len <= 30 {
        let (sign, body) = match text.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", text.strip_prefix('+').unwrap_or(text)),
        };
        let body = body.strip_suffix('.').unwrap_or(body);
        let lead = if body.starts_with('.') { "0" } else { "" };
        let _ = write!(out, "<c r=\"{reference}\"><v>{sign}{lead}{body}</v></c>");
        Ok(())
    } else {
        push_text_cell(out, reference, column, text, false)
    }
}

/// An Excel serial for a day and a nanosecond of the day, if the day is in Excel's range.
fn serial(days: i64, nanos: i64) -> Option<f64> {
    if !(FIRST_DAY..=LAST_DAY).contains(&days) {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "days and nanoseconds fit a double"
    )]
    Some((days + EPOCH_SERIAL) as f64 + nanos as f64 / NANOS_PER_DAY)
}

/// Nanoseconds cut down to whole milliseconds, Excel's resolution. Cutting (never rounding)
/// keeps 23:59:59.9999 on its own day and inside Excel's last day.
fn to_millis(nanos: i64) -> i64 {
    nanos - nanos % 1_000_000
}

fn time_fraction(t: Time) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "nanoseconds of a day fit a double"
    )]
    let nanos = to_millis(nanos_of_day(t)) as f64;
    nanos / NANOS_PER_DAY
}

fn timestamp_serial(ts: Timestamp) -> Option<f64> {
    serial(
        days_from_civil(ts.date()),
        to_millis(nanos_of_day(ts.time())),
    )
}

/// Appends one cell. `Null` appends nothing.
fn push_cell(
    out: &mut String,
    scratch: &mut String,
    reference: &str,
    column: &str,
    value: &Value,
) -> Result<(), ExportError> {
    match value {
        Value::Null => {}
        Value::Bool(b) => {
            let _ = write!(
                out,
                "<c r=\"{reference}\" t=\"b\"><v>{}</v></c>",
                u8::from(*b)
            );
        }
        Value::Int(n) if n.unsigned_abs() <= MAX_EXACT_INT.unsigned_abs() => {
            push_number_cell(out, reference, 0, n);
        }
        Value::Float(x) if x.is_finite() => {
            push_number_cell(out, reference, 0, format_args!("{x:?}"));
        }
        Value::Decimal(d) => push_decimal(out, reference, column, d.as_str())?,
        Value::Date(d) => match serial(days_from_civil(*d), 0) {
            Some(s) => push_number_cell(out, reference, STYLE_DATE, s),
            None => push_text_cell(out, reference, column, &d.to_string(), false)?,
        },
        Value::Timestamp(ts) => match timestamp_serial(*ts) {
            Some(s) => push_number_cell(out, reference, STYLE_DATETIME, s),
            None => push_text_cell(out, reference, column, &ts.to_string(), false)?,
        },
        Value::TimestampTz(ts) => {
            let utc = utc_micros(*ts).and_then(|m| {
                serial(
                    m.div_euclid(MICROS_PER_DAY),
                    to_millis(m.rem_euclid(MICROS_PER_DAY).saturating_mul(1000)),
                )
            });
            match utc {
                Some(s) => push_number_cell(out, reference, STYLE_DATETIME, s),
                None => push_text_cell(out, reference, column, &ts.to_string(), false)?,
            }
        }
        Value::Time(t) => push_number_cell(out, reference, STYLE_TIME, time_fraction(*t)),
        Value::Text(t) | Value::Json(t) => push_text_cell(out, reference, column, t, false)?,
        other => {
            // Large integers, non-finite floats and bytes.
            scratch.clear();
            match other {
                Value::Float(x) if x.is_nan() => scratch.push_str("NaN"),
                Value::Float(x) if x.is_sign_negative() => scratch.push_str("-Infinity"),
                Value::Float(_) => scratch.push_str("Infinity"),
                v => {
                    let _ = write!(scratch, "{v}");
                }
            }
            push_text_cell(out, reference, column, scratch, false)?;
        }
    }
    Ok(())
}

impl<W: std::io::Write + Send> RowWriter for XlsxWriter<W> {
    fn begin(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if self.began {
            return Err(ExportError::State("begin was already called"));
        }
        let result = self.start(columns);
        self.failed = result.is_err();
        result
    }

    fn write_batch(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        if !self.began {
            return Err(ExportError::State("write_batch was called before begin"));
        }
        if self.failed {
            return Err(ExportError::State("the writer failed earlier"));
        }
        let result = self.write_rows(rows);
        self.failed = result.is_err();
        result
    }

    fn finish(mut self: Box<Self>) -> Result<(), ExportError> {
        if !self.began {
            return Err(ExportError::State("finish was called before begin"));
        }
        if self.failed {
            return Err(ExportError::State("the writer failed earlier"));
        }
        self.buf.push_str(xml::SHEET_END);
        self.flush_buf()?;
        let mut zip = self
            .zip
            .take()
            .ok_or(ExportError::State("the XLSX writer is finished"))?;
        zip.end_entry()?;
        zip.finish()?;
        Ok(())
    }
}

impl<W: std::io::Write + Send> XlsxWriter<W> {
    fn start(&mut self, columns: &[Column]) -> Result<(), ExportError> {
        if columns.len() > MAX_COLUMNS {
            return Err(limit(format!(
                "{} columns are more than the {MAX_COLUMNS} of an Excel worksheet",
                columns.len()
            )));
        }
        self.began = true;
        self.columns = unique_names(columns)
            .into_iter()
            .enumerate()
            .map(|(i, name)| ColumnInfo {
                letters: column_letters(i),
                name,
            })
            .collect();
        let zip = self.zip()?;
        zip.add("[Content_Types].xml", xml::CONTENT_TYPES.as_bytes())?;
        zip.add("_rels/.rels", xml::ROOT_RELS.as_bytes())?;
        zip.add("xl/workbook.xml", xml::WORKBOOK.as_bytes())?;
        zip.add("xl/_rels/workbook.xml.rels", xml::WORKBOOK_RELS.as_bytes())?;
        zip.add("xl/styles.xml", xml::STYLES.as_bytes())?;
        zip.start_entry("xl/worksheets/sheet1.xml")?;
        self.buf.push_str(&xml::sheet_start());
        self.buf.push_str("<row r=\"1\">");
        let mut reference = String::new();
        for column in &self.columns {
            reference.clear();
            reference.push_str(&column.letters);
            reference.push('1');
            push_text_cell(&mut self.buf, &reference, &column.name, &column.name, true)?;
        }
        self.buf.push_str("</row>");
        self.flush_buf()
    }

    fn write_rows(&mut self, rows: &[Row]) -> Result<(), ExportError> {
        if self.rows + rows.len() as u64 > MAX_DATA_ROWS {
            return Err(limit(format!(
                "more than {MAX_DATA_ROWS} rows do not fit one Excel worksheet"
            )));
        }
        let mut reference = String::new();
        for row in rows {
            self.rows += 1;
            let number = self.rows + 1;
            let _ = write!(self.buf, "<row r=\"{number}\">");
            for (column, value) in self.columns.iter().zip(&row.0) {
                if value.is_null() {
                    continue;
                }
                reference.clear();
                let _ = write!(reference, "{}{number}", column.letters);
                push_cell(
                    &mut self.buf,
                    &mut self.scratch,
                    &reference,
                    &column.name,
                    value,
                )?;
            }
            self.buf.push_str("</row>");
            if self.buf.len() >= FLUSH_BYTES {
                self.flush_buf()?;
            }
        }
        self.flush_buf()
    }
}

#[cfg(test)]
mod tests {
    use sqler_core::{DataType, Date, Decimal, TimestampTz};

    use super::*;

    fn cell(value: &Value) -> String {
        let mut out = String::new();
        let mut scratch = String::new();
        push_cell(&mut out, &mut scratch, "B2", "col", value).unwrap();
        out
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    fn time(h: u8, m: u8, s: u8) -> Time {
        Time::new(h, m, s, 0).unwrap()
    }

    #[test]
    fn null_has_no_cell() {
        assert_eq!(cell(&Value::Null), "");
    }

    #[test]
    fn booleans_and_numbers() {
        assert_eq!(cell(&Value::Bool(true)), "<c r=\"B2\" t=\"b\"><v>1</v></c>");
        assert_eq!(
            cell(&Value::Bool(false)),
            "<c r=\"B2\" t=\"b\"><v>0</v></c>"
        );
        assert_eq!(cell(&Value::Int(-42)), "<c r=\"B2\"><v>-42</v></c>");
        assert_eq!(cell(&Value::Float(1.5)), "<c r=\"B2\"><v>1.5</v></c>");
        assert_eq!(cell(&Value::Float(1e21)), "<c r=\"B2\"><v>1e21</v></c>");
        assert_eq!(cell(&Value::Float(-0.0)), "<c r=\"B2\"><v>-0.0</v></c>");
    }

    #[test]
    fn integers_beyond_2_pow_53_are_text() {
        let exact = 1_i64 << 53;
        assert_eq!(
            cell(&Value::Int(exact)),
            format!("<c r=\"B2\"><v>{exact}</v></c>")
        );
        let big = cell(&Value::Int(exact + 1));
        assert!(big.contains("t=\"inlineStr\"") && big.contains("9007199254740993"));
        // Negative: text, and the minus sign gets the quote prefix.
        let neg = cell(&Value::Int(i64::MIN));
        assert!(neg.contains("t=\"inlineStr\"") && neg.contains("s=\"5\""));
        assert!(neg.contains("-9223372036854775808"));
    }

    #[test]
    fn non_finite_floats_are_text() {
        assert!(cell(&Value::Float(f64::NAN)).contains(">NaN<"));
        assert!(cell(&Value::Float(f64::INFINITY)).contains(">Infinity<"));
        let neg = cell(&Value::Float(f64::NEG_INFINITY));
        assert!(neg.contains(">-Infinity<") && neg.contains("inlineStr"));
    }

    #[test]
    fn decimals_keep_every_digit() {
        let d = |s: &str| cell(&Value::Decimal(s.parse::<Decimal>().unwrap()));
        assert_eq!(d("123.4500"), "<c r=\"B2\"><v>123.4500</v></c>");
        assert_eq!(d("-0.5"), "<c r=\"B2\"><v>-0.5</v></c>");
        assert_eq!(d(".5"), "<c r=\"B2\"><v>0.5</v></c>");
        assert_eq!(d("0.000001"), "<c r=\"B2\"><v>0.000001</v></c>");
        assert_eq!(
            d("123456789012345"),
            "<c r=\"B2\"><v>123456789012345</v></c>"
        );
        let long = d("1234567890123456");
        assert!(long.contains("inlineStr") && long.contains("1234567890123456"));
        let precise = d("3.14159265358979323846");
        assert!(precise.contains("inlineStr") && precise.contains("3.14159265358979323846"));
        // Few significant digits but beyond a double's range.
        assert!(d(&format!("1{}", "0".repeat(40))).contains("inlineStr"));
    }

    #[test]
    fn dates_and_times() {
        let style = |v: &Value| cell(v);
        assert_eq!(
            style(&Value::Date(date(1970, 1, 1))),
            "<c r=\"B2\" s=\"2\"><v>25569</v></c>"
        );
        assert_eq!(
            style(&Value::Date(date(1900, 3, 1))),
            "<c r=\"B2\" s=\"2\"><v>61</v></c>"
        );
        assert_eq!(
            style(&Value::Date(date(9999, 12, 31))),
            "<c r=\"B2\" s=\"2\"><v>2958465</v></c>"
        );
        // Outside the range: ISO text.
        assert!(cell(&Value::Date(date(1900, 2, 28))).contains(">1900-02-28<"));
        assert!(cell(&Value::Date(date(1, 1, 1))).contains(">0001-01-01<"));
        assert!(cell(&Value::Date(date(10_000, 1, 1))).contains("inlineStr"));
        let noon = Timestamp::new(date(2024, 1, 1), time(12, 0, 0));
        assert_eq!(
            cell(&Value::Timestamp(noon)),
            "<c r=\"B2\" s=\"3\"><v>45292.5</v></c>"
        );
        assert_eq!(
            cell(&Value::Time(time(6, 0, 0))),
            "<c r=\"B2\" s=\"4\"><v>0.25</v></c>"
        );
        assert!(
            cell(&Value::Timestamp(Timestamp::new(
                date(1800, 1, 1),
                time(1, 2, 3)
            )))
            .contains(">1800-01-01 01:02:03<")
        );
    }

    #[test]
    fn the_end_of_the_day_is_cut_not_rounded_up() {
        let last = Time::new(23, 59, 59, 999_999_999).unwrap();
        let out = cell(&Value::Time(last));
        let v: f64 = out
            .split("<v>")
            .nth(1)
            .and_then(|s| s.split("</v>").next())
            .unwrap()
            .parse()
            .unwrap();
        assert!(v < 1.0 && v > 0.999_999, "{out}");
        // The last instant of Excel's calendar stays inside it.
        let end = Timestamp::new(date(9999, 12, 31), last);
        let out = cell(&Value::Timestamp(end));
        assert!(out.contains("s=\"3\""), "{out}");
        let v: f64 = out
            .split("<v>")
            .nth(1)
            .and_then(|s| s.split("</v>").next())
            .unwrap()
            .parse()
            .unwrap();
        assert!((2_958_465.0..2_958_466.0).contains(&v), "{out}");
        let tz = TimestampTz::new(end, 0).unwrap();
        let out = cell(&Value::TimestampTz(tz));
        assert!(out.contains("s=\"3\""), "{out}");
    }

    #[test]
    fn timestamp_with_offset_is_stored_as_utc() {
        let local = Timestamp::new(date(2024, 1, 1), time(17, 30, 0));
        let tz = TimestampTz::new(local, 5 * 3600 + 30 * 60).unwrap();
        // 12:00 UTC.
        assert_eq!(
            cell(&Value::TimestampTz(tz)),
            "<c r=\"B2\" s=\"3\"><v>45292.5</v></c>"
        );
        // Midnight local at +05:30 is the day before in UTC.
        let early = TimestampTz::new(
            Timestamp::new(date(2024, 1, 1), time(1, 0, 0)),
            5 * 3600 + 30 * 60,
        )
        .unwrap();
        let out = cell(&Value::TimestampTz(early));
        assert!(out.contains("<v>45291.8125"), "{out}");
    }

    #[test]
    fn text_is_never_a_formula() {
        for text in [
            "=1+1",
            "+1",
            "-1",
            "@SUM(A1)",
            "\tx",
            "\rx",
            "=cmd|' /C calc'!A0",
        ] {
            let out = cell(&Value::Text(text.to_owned()));
            assert!(out.contains("t=\"inlineStr\""), "{out}");
            assert!(out.contains("s=\"5\""), "{out}");
            assert!(!out.contains("<f"), "{out}");
        }
        for text in ["plain", "a=b", "", " =x"] {
            let out = cell(&Value::Text(text.to_owned()));
            assert!(!out.contains("s=\""), "{out}");
        }
        let json = cell(&Value::Json("-1".to_owned()));
        assert!(json.contains("s=\"5\""));
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            cell(&Value::Text("a<b>&\"c\"\u{1}\r_x0041_".to_owned())),
            "<c r=\"B2\" t=\"inlineStr\"><is><t xml:space=\"preserve\">\
             a&lt;b&gt;&amp;&quot;c&quot;_x0001_&#13;_x005F_x0041_</t></is></c>"
        );
    }

    #[test]
    fn bytes_are_hex_text() {
        let out = cell(&Value::Bytes(vec![0, 255]));
        assert!(out.contains(">0x00ff<"));
    }

    #[test]
    fn cell_length_limit_counts_utf16_units() {
        let mut out = String::new();
        let ok = "a".repeat(MAX_CELL_UNITS);
        push_text_cell(&mut out, "A1", "c", &ok, false).unwrap();
        let too_long = "a".repeat(MAX_CELL_UNITS + 1);
        assert!(matches!(
            push_text_cell(&mut out, "A1", "c", &too_long, false),
            Err(ExportError::Limit(m)) if m.contains("CSV or Parquet") && m.contains("\"c\"")
        ));
        // 16,384 emoji are 32,768 UTF-16 units but only 65,536 bytes.
        let emoji = "😀".repeat(16_384);
        assert!(push_text_cell(&mut out, "A1", "c", &emoji, false).is_err());
        let emoji_ok = "😀".repeat(16_383);
        assert!(push_text_cell(&mut out, "A1", "c", &emoji_ok, false).is_ok());
    }

    fn writer() -> XlsxWriter<Vec<u8>> {
        XlsxWriter::new(ZipWriter::new(Vec::new()))
    }

    #[test]
    fn too_many_columns() {
        let columns: Vec<_> = (0..=MAX_COLUMNS)
            .map(|i| Column::new(format!("c{i}"), DataType::Text))
            .collect();
        let err = writer().begin(&columns).unwrap_err();
        assert!(matches!(err, ExportError::Limit(m) if m.contains("16384") && m.contains("CSV")));
        let ok = &columns[..MAX_COLUMNS];
        assert!(writer().begin(ok).is_ok());
    }

    #[test]
    fn header_over_the_cell_limit() {
        let columns = [Column::new("h".repeat(MAX_CELL_UNITS + 1), DataType::Text)];
        assert!(matches!(
            writer().begin(&columns),
            Err(ExportError::Limit(_))
        ));
    }

    #[test]
    fn row_limit_is_checked_before_the_chunk() {
        let mut w = writer();
        w.begin(&[Column::new("a", DataType::Integer)]).unwrap();
        let rows = vec![Row(vec![Value::Null]); 1000];
        w.rows = MAX_DATA_ROWS - 1000;
        w.write_batch(&rows).unwrap();
        assert_eq!(w.rows, MAX_DATA_ROWS);
        let err = w.write_batch(&rows[..1]).unwrap_err();
        assert!(matches!(err, ExportError::Limit(m) if m.contains("1048575") && m.contains("CSV")));
        assert_eq!(w.rows, MAX_DATA_ROWS);
    }

    #[test]
    fn a_cell_error_mid_row_locks_the_writer() {
        let mut w = writer();
        w.begin(&[
            Column::new("a", DataType::Text),
            Column::new("b", DataType::Text),
        ])
        .unwrap();
        let long = "x".repeat(MAX_CELL_UNITS + 1);
        let bad = Row(vec![Value::Text("ok".into()), Value::Text(long)]);
        assert!(matches!(w.write_batch(&[bad]), Err(ExportError::Limit(_))));
        assert!(matches!(w.write_batch(&[]), Err(ExportError::State(_))));
        assert!(matches!(Box::new(w).finish(), Err(ExportError::State(_))));
    }

    #[test]
    fn the_row_limit_error_locks_the_writer() {
        let mut w = writer();
        w.begin(&[Column::new("a", DataType::Integer)]).unwrap();
        w.rows = MAX_DATA_ROWS;
        assert!(matches!(
            w.write_batch(&[Row(vec![Value::Null])]),
            Err(ExportError::Limit(_))
        ));
        assert!(matches!(Box::new(w).finish(), Err(ExportError::State(_))));
    }

    #[test]
    fn calls_in_the_wrong_order() {
        let mut w = writer();
        assert!(matches!(w.write_batch(&[]), Err(ExportError::State(_))));
        assert!(matches!(
            Box::new(writer()).finish(),
            Err(ExportError::State(_))
        ));
    }
}
