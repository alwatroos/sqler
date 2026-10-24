// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Conversions between `tiberius` cells and core `Value`s, without `chrono` or `time`.
//!
//! Temporal values are decoded from the raw day and increment fields of the TDS wire format.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::str::FromStr as _;

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{
    Column, DataType, Date, Decimal, Row, Time, Timestamp, TimestampTz, Value,
};
use tiberius::numeric::Numeric;
use tiberius::time::{Date as TdsDate, DateTime2, DateTimeOffset, Time as TdsTime};
use tiberius::{ColumnData, ColumnType};

const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_DAY: u64 = 86_400 * NS_PER_SEC;
/// Days from 0001-01-01 (the `date` epoch of SQL Server) to 1970-01-01.
const DAYS_0001_TO_1970: i64 = 719_162;
/// Days from 0001-01-01 to 1900-01-01 (the `datetime` epoch).
const DAYS_0001_TO_1900: i64 = 693_595;

fn bad(what: &str) -> DriverError {
    DriverError::Protocol(format!("the server sent an invalid {what} value"))
}

/// The logical type of a result column.
pub(crate) fn data_type(ty: ColumnType) -> DataType {
    match ty {
        ColumnType::Bit | ColumnType::Bitn => DataType::Boolean,
        ColumnType::Int1 | ColumnType::Int2 | ColumnType::Int4 | ColumnType::Int8 => {
            DataType::Integer
        }
        ColumnType::Intn => DataType::Integer,
        ColumnType::Float4
        | ColumnType::Float8
        | ColumnType::Floatn
        | ColumnType::Money
        | ColumnType::Money4 => DataType::Float,
        ColumnType::Decimaln | ColumnType::Numericn => DataType::Decimal,
        ColumnType::Guid
        | ColumnType::BigVarChar
        | ColumnType::BigChar
        | ColumnType::NVarchar
        | ColumnType::NChar
        | ColumnType::Xml
        | ColumnType::Text
        | ColumnType::NText => DataType::Text,
        ColumnType::BigVarBin | ColumnType::BigBinary | ColumnType::Image => DataType::Binary,
        ColumnType::Daten => DataType::Date,
        ColumnType::Timen => DataType::Time,
        ColumnType::Datetime
        | ColumnType::Datetime4
        | ColumnType::Datetimen
        | ColumnType::Datetime2 => DataType::Timestamp,
        ColumnType::DatetimeOffsetn => DataType::TimestampTz,
        _ => DataType::Other,
    }
}

/// The T-SQL type family name of a result column (the wire type does not carry the length).
pub(crate) fn native_type(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Null => "null",
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Decimaln | ColumnType::Numericn => "numeric",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::BigVarBin => "varbinary",
        ColumnType::BigBinary => "binary",
        ColumnType::BigVarChar => "varchar",
        ColumnType::BigChar => "char",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::NChar => "nchar",
        ColumnType::Xml => "xml",
        ColumnType::Udt => "udt",
        ColumnType::Text => "text",
        ColumnType::Image => "image",
        ColumnType::NText => "ntext",
        ColumnType::SSVariant => "sql_variant",
    }
}

/// Converts the metadata of a result column.
pub(crate) fn column(col: &tiberius::Column) -> Column {
    let ty = col.column_type();
    Column::new(col.name(), data_type(ty)).with_native_type(native_type(ty))
}

/// Converts one row.
pub(crate) fn decode_row(row: &tiberius::Row) -> Result<Row, DriverError> {
    row.cells()
        .map(|(_, data)| value(data))
        .collect::<Result<Vec<_>, _>>()
        .map(Row)
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> Result<(i32, u8, u8), DriverError> {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Ok((
        i32::try_from(year).map_err(|_| bad("date"))?,
        u8::try_from(month).map_err(|_| bad("date"))?,
        u8::try_from(day).map_err(|_| bad("date"))?,
    ))
}

fn date_from_epoch_days(days: i64) -> Result<Date, DriverError> {
    let (year, month, day) = civil_from_days(days)?;
    Date::new(year, month, day).map_err(|_| bad("date"))
}

fn time_from_ns(ns: u64) -> Result<Time, DriverError> {
    if ns >= NS_PER_DAY {
        return Err(bad("time"));
    }
    let secs = ns / NS_PER_SEC;
    let nanos = u32::try_from(ns % NS_PER_SEC).map_err(|_| bad("time"))?;
    let part = |v: u64| u8::try_from(v).map_err(|_| bad("time"));
    Time::new(
        part(secs / 3600)?,
        part(secs / 60 % 60)?,
        part(secs % 60)?,
        nanos,
    )
    .map_err(|_| bad("time"))
}

fn timestamp(epoch_days: i64, ns_of_day: u64) -> Result<Timestamp, DriverError> {
    Ok(Timestamp::new(
        date_from_epoch_days(epoch_days)?,
        time_from_ns(ns_of_day)?,
    ))
}

/// Nanoseconds since midnight of a `time(n)` value.
fn time_ns(time: TdsTime) -> Result<u64, DriverError> {
    let scale = u32::from(time.scale());
    if scale > 7 {
        return Err(bad("time"));
    }
    time.increments()
        .checked_mul(10u64.pow(9 - scale))
        .filter(|ns| *ns < NS_PER_DAY)
        .ok_or_else(|| bad("time"))
}

fn tds_date_days(date: TdsDate) -> i64 {
    i64::from(date.days()) - DAYS_0001_TO_1970
}

fn datetime2(dt: DateTime2) -> Result<Timestamp, DriverError> {
    timestamp(tds_date_days(dt.date()), time_ns(dt.time())?)
}

/// `datetimeoffset` is sent as UTC plus an offset in minutes; the value keeps the wall clock at
/// that offset.
fn datetime_offset(dto: DateTimeOffset) -> Result<TimestampTz, DriverError> {
    let dt = dto.datetime2();
    let offset_seconds = i32::from(dto.offset()) * 60;
    let total = i128::from(tds_date_days(dt.date())) * i128::from(NS_PER_DAY)
        + i128::from(time_ns(dt.time())?)
        + i128::from(offset_seconds) * i128::from(NS_PER_SEC);
    let day = i128::from(NS_PER_DAY);
    let days = i64::try_from(total.div_euclid(day)).map_err(|_| bad("datetimeoffset"))?;
    let ns = u64::try_from(total.rem_euclid(day)).map_err(|_| bad("datetimeoffset"))?;
    TimestampTz::new(timestamp(days, ns)?, offset_seconds).map_err(|_| bad("datetimeoffset"))
}

/// `datetime`: days since 1900-01-01 and 1/300 s ticks, rounded to the millisecond as the server
/// does.
fn datetime(days: i32, ticks: u32) -> Result<Timestamp, DriverError> {
    let ms = (u64::from(ticks) * 10 + 1) / 3;
    timestamp(
        i64::from(days) + DAYS_0001_TO_1900 - DAYS_0001_TO_1970,
        ms * 1_000_000,
    )
}

/// `smalldatetime`: days since 1900-01-01 and minutes since midnight.
fn small_datetime(days: u16, minutes: u16) -> Result<Timestamp, DriverError> {
    timestamp(
        i64::from(days) + DAYS_0001_TO_1900 - DAYS_0001_TO_1970,
        u64::from(minutes) * 60 * NS_PER_SEC,
    )
}

fn numeric(n: Numeric) -> Result<Value, DriverError> {
    let digits = n.value().unsigned_abs().to_string();
    let scale = usize::from(n.scale());
    let mut text = if n.value() < 0 { "-" } else { "" }.to_owned();
    if scale == 0 {
        text.push_str(&digits);
    } else {
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (int, frac) = padded.split_at(padded.len() - scale);
        text.push_str(int);
        text.push('.');
        text.push_str(frac);
    }
    Decimal::from_str(&text)
        .map(Value::Decimal)
        .map_err(|_| bad("numeric"))
}

fn float32(v: f32) -> f64 {
    // The shortest decimal form of the f32, so 0.1 stays 0.1.
    v.to_string().parse().unwrap_or_else(|_| f64::from(v))
}

fn some<T>(
    v: Option<T>,
    f: impl FnOnce(T) -> Result<Value, DriverError>,
) -> Result<Value, DriverError> {
    v.map_or(Ok(Value::Null), f)
}

/// Converts one cell.
pub(crate) fn value(data: &ColumnData<'_>) -> Result<Value, DriverError> {
    match data {
        ColumnData::Bit(v) => some(*v, |v| Ok(Value::Bool(v))),
        ColumnData::U8(v) => some(*v, |v| Ok(Value::Int(i64::from(v)))),
        ColumnData::I16(v) => some(*v, |v| Ok(Value::Int(i64::from(v)))),
        ColumnData::I32(v) => some(*v, |v| Ok(Value::Int(i64::from(v)))),
        ColumnData::I64(v) => some(*v, |v| Ok(Value::Int(v))),
        ColumnData::F32(v) => some(*v, |v| Ok(Value::Float(float32(v)))),
        ColumnData::F64(v) => some(*v, |v| Ok(Value::Float(v))),
        ColumnData::Numeric(v) => some(*v, numeric),
        ColumnData::String(v) => some(v.as_ref(), |s| Ok(Value::Text(s.to_string()))),
        ColumnData::Xml(v) => some(v.as_ref(), |x| {
            Ok(Value::Text(x.clone().into_owned().into_string()))
        }),
        ColumnData::Guid(v) => some(*v, |g| Ok(Value::Text(g.to_string().to_uppercase()))),
        ColumnData::Binary(v) => some(v.as_ref(), |b| Ok(Value::Bytes(b.to_vec()))),
        ColumnData::Date(v) => some(*v, |d| {
            date_from_epoch_days(tds_date_days(d)).map(Value::Date)
        }),
        ColumnData::Time(v) => some(*v, |t| time_from_ns(time_ns(t)?).map(Value::Time)),
        ColumnData::DateTime(v) => some(*v, |d| {
            datetime(d.days(), d.seconds_fragments()).map(Value::Timestamp)
        }),
        ColumnData::SmallDateTime(v) => some(*v, |d| {
            small_datetime(d.days(), d.seconds_fragments()).map(Value::Timestamp)
        }),
        ColumnData::DateTime2(v) => some(*v, |d| datetime2(d).map(Value::Timestamp)),
        ColumnData::DateTimeOffset(v) => some(*v, |d| datetime_offset(d).map(Value::TimestampTz)),
    }
}

/// Keeps at most 7 fraction digits: SQL Server rejects longer ones when it parses a string.
fn trim_fraction(text: &str) -> String {
    let Some(dot) = text.find('.') else {
        return text.to_owned();
    };
    let digits = text[dot + 1..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    if digits <= 7 {
        return text.to_owned();
    }
    format!("{}{}", &text[..dot + 8], &text[dot + 1 + digits..])
}

/// Binds `value` as the next positional parameter (`@P1`, `@P2`, ...). Values without a direct
/// tiberius type are bound as text.
pub(crate) fn bind(query: &mut tiberius::Query<'static>, value: &Value) {
    match value {
        Value::Null => query.bind(Option::<Cow<'static, str>>::None),
        Value::Bool(b) => query.bind(*b),
        Value::Int(i) => query.bind(*i),
        Value::Float(f) => query.bind(*f),
        Value::Bytes(b) => query.bind(Cow::<'static, [u8]>::Owned(b.clone())),
        other => query.bind(Cow::<'static, str>::Owned(text_param(other))),
    }
}

/// Binds `value` to the named parameter `name` of an RPC command. It binds exactly what [`bind`]
/// binds: the same values and the same text forms.
pub(crate) fn bind_named(cmd: &mut tiberius::Command<'static>, name: String, value: &Value) {
    match value {
        Value::Null => cmd.bind_param(name, Option::<Cow<'static, str>>::None),
        Value::Bool(b) => cmd.bind_param(name, *b),
        Value::Int(i) => cmd.bind_param(name, *i),
        Value::Float(f) => cmd.bind_param(name, *f),
        Value::Bytes(b) => cmd.bind_param(name, Cow::<'static, [u8]>::Owned(b.clone())),
        other => cmd.bind_param(name, Cow::<'static, str>::Owned(text_param(other))),
    }
}

/// The `@params` declaration of `sp_executesql` for `params` bound as `@P1`, `@P2`, ... It mirrors
/// the type names tiberius derives from the bound data (`ColumnData::type_name`), with a comma and
/// no space between entries.
pub(crate) fn declaration(params: &[Value]) -> String {
    let mut out = String::new();
    for (i, param) in params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let ty = match param {
            Value::Null => "nvarchar(4000)",
            Value::Bool(_) => "bit",
            Value::Int(_) => "bigint",
            Value::Float(_) => "float(53)",
            Value::Bytes(b) if b.len() <= 8000 => "varbinary(8000)",
            Value::Bytes(_) => "varbinary(max)",
            other if text_param(other).len() <= 4000 => "nvarchar(4000)",
            _ => "nvarchar(max)",
        };
        let _ = write!(out, "@P{} {ty}", i + 1);
    }
    out
}

/// The text a value without a direct tiberius type is bound as. Dates and timestamps use forms that
/// do not depend on the session `DATEFORMAT` or language: `YYYYMMDD` for a date and ISO 8601 with a
/// `T` for a timestamp (`2024-03-05 10:00:00` would read as year-day-month under `dmy`).
fn text_param(value: &Value) -> String {
    match value {
        Value::Date(d) => d.to_string().replace('-', ""),
        Value::Time(t) => trim_fraction(&t.to_string()),
        Value::Timestamp(t) => iso_timestamp(&trim_fraction(&t.to_string())),
        Value::TimestampTz(t) => iso_timestamp(&trim_fraction(&t.to_string())),
        other => other.to_string(),
    }
}

/// `2024-03-05 10:00:00` as `2024-03-05T10:00:00`.
fn iso_timestamp(text: &str) -> String {
    match text.split_once(' ') {
        Some((date, rest)) => format!("{date}T{rest}"),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use tiberius::time::SmallDateTime;

    use super::*;

    #[test]
    fn column_types() {
        let cases = [
            (ColumnType::Bit, DataType::Boolean),
            (ColumnType::Bitn, DataType::Boolean),
            (ColumnType::Int1, DataType::Integer),
            (ColumnType::Int8, DataType::Integer),
            (ColumnType::Intn, DataType::Integer),
            (ColumnType::Float4, DataType::Float),
            (ColumnType::Money, DataType::Float),
            (ColumnType::Numericn, DataType::Decimal),
            (ColumnType::Guid, DataType::Text),
            (ColumnType::NVarchar, DataType::Text),
            (ColumnType::Xml, DataType::Text),
            (ColumnType::BigVarBin, DataType::Binary),
            (ColumnType::Image, DataType::Binary),
            (ColumnType::Daten, DataType::Date),
            (ColumnType::Timen, DataType::Time),
            (ColumnType::Datetime, DataType::Timestamp),
            (ColumnType::Datetime2, DataType::Timestamp),
            (ColumnType::Datetime4, DataType::Timestamp),
            (ColumnType::DatetimeOffsetn, DataType::TimestampTz),
            (ColumnType::Udt, DataType::Other),
            (ColumnType::SSVariant, DataType::Other),
        ];
        for (ty, expected) in cases {
            assert_eq!(data_type(ty), expected, "{ty:?}");
        }
        assert_eq!(native_type(ColumnType::NVarchar), "nvarchar");
        assert_eq!(native_type(ColumnType::Int4), "int");
    }

    fn ok(data: &ColumnData<'_>) -> Value {
        value(data).unwrap()
    }

    #[test]
    fn scalars() {
        assert_eq!(ok(&ColumnData::Bit(Some(true))), Value::Bool(true));
        assert_eq!(ok(&ColumnData::U8(Some(255))), Value::Int(255));
        assert_eq!(ok(&ColumnData::I16(Some(-2))), Value::Int(-2));
        assert_eq!(ok(&ColumnData::I32(Some(7))), Value::Int(7));
        assert_eq!(ok(&ColumnData::I64(Some(i64::MIN))), Value::Int(i64::MIN));
        assert_eq!(ok(&ColumnData::F64(Some(2.25))), Value::Float(2.25));
        assert_eq!(ok(&ColumnData::F32(Some(0.1))), Value::Float(0.1));
        assert_eq!(ok(&ColumnData::I32(None)), Value::Null);
        assert_eq!(ok(&ColumnData::String(None)), Value::Null);
        assert_eq!(ok(&ColumnData::Date(None)), Value::Null);
    }

    #[test]
    fn text_binary_guid_xml() {
        assert_eq!(
            ok(&ColumnData::String(Some("héllo".into()))),
            Value::Text("héllo".to_owned())
        );
        assert_eq!(
            ok(&ColumnData::Binary(Some(vec![1_u8, 2].into()))),
            Value::Bytes(vec![1, 2])
        );
        let guid = tiberius::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(
            ok(&ColumnData::Guid(Some(guid))),
            Value::Text("01234567-89AB-CDEF-0123-456789ABCDEF".to_owned())
        );
        let xml = tiberius::xml::XmlData::new("<a/>");
        assert_eq!(
            ok(&ColumnData::Xml(Some(Cow::Owned(xml)))),
            Value::Text("<a/>".to_owned())
        );
    }

    #[test]
    fn numeric_keeps_scale_and_sign() {
        let n = |v: i128, s: u8| ok(&ColumnData::Numeric(Some(Numeric::new_with_scale(v, s))));
        assert_eq!(n(12_345_678, 3).to_string(), "12345.678");
        assert_eq!(n(-5, 2).to_string(), "-0.05");
        assert_eq!(n(150, 2).to_string(), "1.50");
        assert_eq!(n(42, 0).to_string(), "42");
        assert_eq!(n(0, 3).to_string(), "0.000");
    }

    #[test]
    fn dates() {
        let d = |days: u32| ok(&ColumnData::Date(Some(TdsDate::new(days))));
        // 0001-01-01 is day 0, 1970-01-01 is day 719162.
        assert_eq!(d(0).to_string(), "0001-01-01");
        assert_eq!(d(719_162).to_string(), "1970-01-01");
        assert_eq!(d(738_885).to_string(), "2024-01-01");
        assert_eq!(d(738_944).to_string(), "2024-02-29");
        assert_eq!(d(3_652_058).to_string(), "9999-12-31");
    }

    #[test]
    fn times_by_scale() {
        let t = |inc: u64, scale: u8| ok(&ColumnData::Time(Some(TdsTime::new(inc, scale))));
        assert_eq!(t(0, 7).to_string(), "00:00:00");
        assert_eq!(t(12 * 3600 + 34 * 60 + 56, 0).to_string(), "12:34:56");
        assert_eq!(t(123, 2).to_string(), "00:00:01.23");
        assert_eq!(t(863_999_999_999, 7).to_string(), "23:59:59.9999999");
        assert!(value(&ColumnData::Time(Some(TdsTime::new(86_400, 0)))).is_err());
    }

    #[test]
    fn datetime2_values() {
        let dt = DateTime2::new(TdsDate::new(738_944), TdsTime::new(5_000_000, 7));
        assert_eq!(
            ok(&ColumnData::DateTime2(Some(dt))).to_string(),
            "2024-02-29 00:00:00.5"
        );
    }

    #[test]
    fn legacy_datetimes() {
        // 1900-01-01 00:00:00 and 2000-01-01 12:00:00.003.
        let dt = |days, ticks| {
            ok(&ColumnData::DateTime(Some(tiberius::time::DateTime::new(
                days, ticks,
            ))))
            .to_string()
        };
        assert_eq!(dt(0, 0), "1900-01-01 00:00:00");
        assert_eq!(dt(36_524, 12 * 3600 * 300 + 1), "2000-01-01 12:00:00.003");
        assert_eq!(dt(-1, 0), "1899-12-31 00:00:00");
        let small = ColumnData::SmallDateTime(Some(SmallDateTime::new(1, 90)));
        assert_eq!(ok(&small).to_string(), "1900-01-02 01:30:00");
    }

    #[test]
    fn datetimeoffset_is_wall_clock_at_offset() {
        // Stored as UTC 2024-01-01 23:30 with +02:00: the wall clock is 2024-01-02 01:30.
        let utc = DateTime2::new(TdsDate::new(738_885), TdsTime::new(23 * 3600 + 30 * 60, 0));
        let v = ok(&ColumnData::DateTimeOffset(Some(DateTimeOffset::new(
            utc, 120,
        ))));
        assert_eq!(v.to_string(), "2024-01-02 01:30:00+02:00");
        let Value::TimestampTz(tz) = v else {
            panic!("not a timestamptz");
        };
        assert_eq!(tz.offset_seconds(), 7200);
        let west = DateTime2::new(TdsDate::new(738_885), TdsTime::new(60, 0));
        let v = ok(&ColumnData::DateTimeOffset(Some(DateTimeOffset::new(
            west, -300,
        ))));
        assert_eq!(v.to_string(), "2023-12-31 19:01:00-05:00");
    }

    #[test]
    fn dates_and_timestamps_bind_in_locale_proof_forms() {
        let date = ok(&ColumnData::Date(Some(TdsDate::new(738_944))));
        assert_eq!(text_param(&date), "20240229");
        let dt2 = DateTime2::new(TdsDate::new(738_944), TdsTime::new(5_000_000, 7));
        assert_eq!(
            text_param(&ok(&ColumnData::DateTime2(Some(dt2)))),
            "2024-02-29T00:00:00.5"
        );
        let legacy = ok(&ColumnData::DateTime(Some(tiberius::time::DateTime::new(
            36_524,
            12 * 3600 * 300,
        ))));
        assert_eq!(text_param(&legacy), "2000-01-01T12:00:00");
        let utc = DateTime2::new(TdsDate::new(738_885), TdsTime::new(23 * 3600 + 30 * 60, 0));
        let tz = ok(&ColumnData::DateTimeOffset(Some(DateTimeOffset::new(
            utc, 120,
        ))));
        assert_eq!(text_param(&tz), "2024-01-02T01:30:00+02:00");
        let time = ok(&ColumnData::Time(Some(TdsTime::new(123, 2))));
        assert_eq!(text_param(&time), "00:00:01.23");
    }

    #[test]
    fn declaration_per_value_kind() {
        let one = |v: Value| declaration(&[v]);
        assert_eq!(one(Value::Null), "@P1 nvarchar(4000)");
        assert_eq!(one(Value::Bool(true)), "@P1 bit");
        assert_eq!(one(Value::Int(1)), "@P1 bigint");
        assert_eq!(one(Value::Float(1.5)), "@P1 float(53)");
        assert_eq!(one(Value::Bytes(vec![1])), "@P1 varbinary(8000)");
        assert_eq!(one(Value::Text("a".to_owned())), "@P1 nvarchar(4000)");
        let date = ok(&ColumnData::Date(Some(TdsDate::new(738_944))));
        assert_eq!(one(date), "@P1 nvarchar(4000)");
    }

    #[test]
    fn declaration_thresholds() {
        let one = |v: Value| declaration(&[v]);
        assert_eq!(one(Value::Bytes(vec![0; 8000])), "@P1 varbinary(8000)");
        assert_eq!(one(Value::Bytes(vec![0; 8001])), "@P1 varbinary(max)");
        assert_eq!(one(Value::Text("a".repeat(4000))), "@P1 nvarchar(4000)");
        assert_eq!(one(Value::Text("a".repeat(4001))), "@P1 nvarchar(max)");
        // The length is in UTF-8 bytes, as tiberius counts it.
        assert_eq!(one(Value::Text("é".repeat(2001))), "@P1 nvarchar(max)");
    }

    #[test]
    fn declaration_joins_with_commas_and_no_spaces() {
        let params = [Value::Int(1), Value::Text("x".to_owned()), Value::Null];
        assert_eq!(
            declaration(&params),
            "@P1 bigint,@P2 nvarchar(4000),@P3 nvarchar(4000)"
        );
        assert_eq!(declaration(&[]), "");
    }

    #[test]
    fn fraction_is_trimmed_for_binding() {
        assert_eq!(trim_fraction("12:00:00"), "12:00:00");
        assert_eq!(trim_fraction("12:00:00.123456789"), "12:00:00.1234567");
        assert_eq!(
            trim_fraction("2024-01-01 12:00:00.123456789+02:00"),
            "2024-01-01 12:00:00.1234567+02:00"
        );
        assert_eq!(trim_fraction("12:00:00.5"), "12:00:00.5");
    }
}
