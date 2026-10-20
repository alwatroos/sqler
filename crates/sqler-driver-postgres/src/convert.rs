// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Type mapping, binary wire decoding, parameter binding and error mapping.
//!
//! `sqlx` asks the server for results in the binary format. Cells are decoded here from the raw
//! bytes by type OID, so no `chrono`, `time` or decimal crate is needed.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};

use sqler_driver_api::DriverError;
use sqler_driver_api::sqler_core::{Column, DataType, Date, Row, Time, Timestamp, Value};
use sqlx::postgres::{
    PgArguments, PgColumn, PgRow, PgTypeInfo, PgTypeKind, PgValueFormat, PgValueRef,
};
use sqlx::query::Query;
use sqlx::{
    AssertSqlSafe, Column as _, Postgres, Row as _, SqlSafeStr as _, TypeInfo as _, ValueRef as _,
};

/// SQLSTATE `query_canceled`.
const QUERY_CANCELED: &str = "57014";

const MICROS_PER_SEC: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SEC;
/// Days from 1970-01-01 to 2000-01-01, the `PostgreSQL` epoch.
const PG_EPOCH_DAYS: i64 = 10_957;

/// Whether a type is an array: by kind, or by a name ending in `[]`.
pub(crate) fn is_array(info: &PgTypeInfo) -> bool {
    matches!(info.kind(), PgTypeKind::Array(_)) || info.name().ends_with("[]")
}

/// Maps a type OID to a [`DataType`].
pub(crate) fn data_type(oid: Option<u32>, array: bool) -> DataType {
    if array {
        return DataType::Text;
    }
    match oid {
        Some(16) => DataType::Boolean,
        Some(20 | 21 | 23 | 26) => DataType::Integer,
        Some(700 | 701) => DataType::Float,
        Some(1700) => DataType::Decimal,
        Some(25 | 1042 | 1043 | 19 | 142 | 705 | 2950 | 1266 | 1186 | 869 | 650) => DataType::Text,
        Some(17) => DataType::Binary,
        Some(1082) => DataType::Date,
        Some(1083) => DataType::Time,
        Some(1114) => DataType::Timestamp,
        Some(1184) => DataType::TimestampTz,
        Some(114 | 3802) => DataType::Json,
        _ => DataType::Other,
    }
}

fn oid_of(info: &PgTypeInfo) -> Option<u32> {
    info.oid().map(|o| o.0)
}

/// Builds the core [`Column`] for a prepared-statement column.
pub(crate) fn column(col: &PgColumn) -> Column {
    let info = col.type_info();
    Column::new(col.name(), data_type(oid_of(info), is_array(info)))
        .with_native_type(info.name().to_owned())
}

/// Decodes one row. A cell that does not decode as its type falls back to text or bytes.
pub(crate) fn decode_row(row: &PgRow) -> Result<Row, DriverError> {
    let mut values = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let raw = row.try_get_raw(i).map_err(|e| query_error(e, false))?;
        if raw.is_null() {
            values.push(Value::Null);
            continue;
        }
        values.push(decode_value(&raw));
    }
    Ok(Row(values))
}

fn decode_value(raw: &PgValueRef<'_>) -> Value {
    let Ok(bytes) = raw.as_bytes() else {
        return Value::Null;
    };
    let info = raw.type_info();
    let oid = oid_of(&info);
    let array = is_array(&info);
    match raw.format() {
        PgValueFormat::Binary => decode_binary(oid, array, bytes),
        PgValueFormat::Text => decode_text(oid, bytes),
    }
}

/// Decodes the binary wire format of one non-null cell.
pub(crate) fn decode_binary(oid: Option<u32>, array: bool, bytes: &[u8]) -> Value {
    if array {
        return decode_array(bytes).map_or_else(|| fallback(bytes), Value::Text);
    }
    let typed = match oid {
        Some(16) => bytes.first().map(|b| Value::Bool(*b != 0)),
        Some(21) => be_i16(bytes).map(|v| Value::Int(i64::from(v))),
        Some(23) => be_i32(bytes).map(|v| Value::Int(i64::from(v))),
        Some(20) => be_i64(bytes).map(Value::Int),
        Some(26) => be_u32(bytes).map(|v| Value::Int(i64::from(v))),
        Some(700) => be_f32(bytes).map(float4),
        Some(701) => be_f64(bytes).map(Value::Float),
        Some(1700) => decode_numeric(bytes).map(|text| {
            text.parse()
                .map_or_else(|_| Value::Text(text.clone()), Value::Decimal)
        }),
        Some(25 | 1042 | 1043 | 19 | 142 | 705) => std::str::from_utf8(bytes)
            .ok()
            .map(|s| Value::Text(s.to_owned())),
        Some(17) => Some(Value::Bytes(bytes.to_vec())),
        Some(114) => std::str::from_utf8(bytes)
            .ok()
            .map(|s| Value::Json(s.to_owned())),
        Some(3802) => match bytes.split_first() {
            Some((1, rest)) => std::str::from_utf8(rest)
                .ok()
                .map(|s| Value::Json(s.to_owned())),
            _ => None,
        },
        Some(1082) => be_i32(bytes).map(|days| match days {
            i32::MAX => Value::Text("infinity".to_owned()),
            i32::MIN => Value::Text("-infinity".to_owned()),
            days => {
                let text = fmt_date(i64::from(days));
                text.parse::<Date>()
                    .map_or_else(|_| Value::Text(text), Value::Date)
            }
        }),
        Some(1083) => be_i64(bytes).map(|micros| {
            if (0..MICROS_PER_DAY).contains(&micros) {
                let text = fmt_time(micros);
                text.parse::<Time>()
                    .map_or_else(|_| Value::Text(text), Value::Time)
            } else {
                Value::Text(fmt_time(micros))
            }
        }),
        Some(1114) => be_i64(bytes).map(|micros| match micros {
            i64::MAX => Value::Text("infinity".to_owned()),
            i64::MIN => Value::Text("-infinity".to_owned()),
            micros => {
                let text = fmt_timestamp(micros);
                text.parse::<Timestamp>()
                    .map_or_else(|_| Value::Text(text), Value::Timestamp)
            }
        }),
        Some(1184) => be_i64(bytes).map(|micros| match micros {
            i64::MAX => Value::Text("infinity".to_owned()),
            i64::MIN => Value::Text("-infinity".to_owned()),
            micros => {
                let text = format!("{}+00:00", fmt_timestamp(micros));
                text.parse()
                    .map_or_else(|_| Value::Text(text), Value::TimestampTz)
            }
        }),
        Some(2950 | 1266 | 1186 | 869 | 650) => {
            scalar_text(oid.unwrap_or(0), bytes).map(Value::Text)
        }
        _ => None,
    };
    typed.unwrap_or_else(|| fallback(bytes))
}

/// Anything else: valid UTF-8 is text (enums, `citext`), otherwise bytes.
fn fallback(bytes: &[u8]) -> Value {
    std::str::from_utf8(bytes).map_or_else(
        |_| Value::Bytes(bytes.to_vec()),
        |s| Value::Text(s.to_owned()),
    )
}

fn float4(v: f32) -> Value {
    // The shortest text of the f32, so 0.1 stays 0.1 and not 0.10000000149011612.
    if v.is_finite() {
        v.to_string()
            .parse()
            .map_or(Value::Float(f64::from(v)), Value::Float)
    } else {
        Value::Float(f64::from(v))
    }
}

/// Decodes the text format of one non-null cell, by the core `FromStr` where one exists.
fn decode_text(oid: Option<u32>, bytes: &[u8]) -> Value {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Value::Bytes(bytes.to_vec());
    };
    let owned = || Value::Text(text.to_owned());
    match oid {
        Some(16) => Value::Bool(text == "t"),
        Some(20 | 21 | 23 | 26) => text.parse().map_or_else(|_| owned(), Value::Int),
        Some(700 | 701) => text.parse().map_or_else(|_| owned(), Value::Float),
        Some(1700) => text.parse().map_or_else(|_| owned(), Value::Decimal),
        Some(1082) => text.parse().map_or_else(|_| owned(), Value::Date),
        Some(1083) => text.parse().map_or_else(|_| owned(), Value::Time),
        Some(1114) => text.parse().map_or_else(|_| owned(), Value::Timestamp),
        Some(1184) => text.parse().map_or_else(|_| owned(), Value::TimestampTz),
        Some(114 | 3802) => Value::Json(text.to_owned()),
        Some(17) => match text.strip_prefix("\\x").and_then(unhex) {
            Some(b) => Value::Bytes(b),
            None => Value::Bytes(bytes.to_vec()),
        },
        _ => owned(),
    }
}

fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn be_i16(b: &[u8]) -> Option<i16> {
    Some(i16::from_be_bytes(b.try_into().ok()?))
}
fn be_i32(b: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(b.try_into().ok()?))
}
fn be_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.try_into().ok()?))
}
fn be_i64(b: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(b.try_into().ok()?))
}
fn be_f32(b: &[u8]) -> Option<f32> {
    Some(f32::from_be_bytes(b.try_into().ok()?))
}
fn be_f64(b: &[u8]) -> Option<f64> {
    Some(f64::from_be_bytes(b.try_into().ok()?))
}

/// Decodes the `numeric` wire format to its decimal text. NaN and the infinities give their names.
fn decode_numeric(b: &[u8]) -> Option<String> {
    let ndigits = usize::from(u16::from_be_bytes(b.get(0..2)?.try_into().ok()?));
    let weight = i32::from(be_i16(b.get(2..4)?)?);
    let sign = u16::from_be_bytes(b.get(4..6)?.try_into().ok()?);
    let dscale = usize::from(u16::from_be_bytes(b.get(6..8)?.try_into().ok()?));
    match sign {
        0xC000 => return Some("NaN".to_owned()),
        0xD000 => return Some("Infinity".to_owned()),
        0xF000 => return Some("-Infinity".to_owned()),
        0x0000 | 0x4000 => {}
        _ => return None,
    }
    let raw = b.get(8..)?;
    if raw.len() != ndigits * 2 {
        return None;
    }
    let digits: Vec<i32> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i32::from(i16::from_be_bytes(*c)))
        .collect();
    // The base-10000 digit with decimal weight `w` (0 outside the stored range).
    let at = |w: i32| -> i32 {
        usize::try_from(weight - w)
            .ok()
            .and_then(|k| digits.get(k))
            .copied()
            .unwrap_or(0)
    };
    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        for w in (0..=weight).rev() {
            if w == weight {
                write!(out, "{}", at(w)).ok()?;
            } else {
                write!(out, "{:04}", at(w)).ok()?;
            }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut w = -1;
        while frac.len() < dscale {
            write!(frac, "{:04}", at(w)).ok()?;
            w -= 1;
        }
        frac.truncate(dscale);
        out.push('.');
        out.push_str(&frac);
    }
    Some(out)
}

/// `(year, month, day)` of a day count since 2000-01-01 (proleptic Gregorian, astronomical years).
fn civil_from_days(pg_days: i64) -> (i64, i64, i64) {
    let z = pg_days + PG_EPOCH_DAYS + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2024-01-02`, in the style of the core `Date` (`-0001-..` and `+12345-..` for far years).
fn fmt_date(pg_days: i64) -> String {
    let (y, m, d) = civil_from_days(pg_days);
    match y {
        0..=9999 => format!("{y:04}-{m:02}-{d:02}"),
        10_000.. => format!("+{y}-{m:02}-{d:02}"),
        _ => format!("-{:04}-{m:02}-{d:02}", -y),
    }
}

/// `HH:MM:SS` with the fraction when it is not zero (trailing zeros trimmed). Hours may pass 24.
fn fmt_time(micros: i64) -> String {
    let secs = micros.div_euclid(MICROS_PER_SEC);
    let frac = micros.rem_euclid(MICROS_PER_SEC);
    let mut out = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    push_fraction(&mut out, frac);
    out
}

fn push_fraction(out: &mut String, micros: i64) {
    if micros != 0 {
        let text = format!("{micros:06}");
        out.push('.');
        out.push_str(text.trim_end_matches('0'));
    }
}

fn fmt_timestamp(micros: i64) -> String {
    let days = micros.div_euclid(MICROS_PER_DAY);
    let rem = micros.rem_euclid(MICROS_PER_DAY);
    format!("{} {}", fmt_date(days), fmt_time(rem))
}

fn fmt_interval(b: &[u8]) -> Option<String> {
    let micros = be_i64(b.get(0..8)?)?;
    let days = i64::from(be_i32(b.get(8..12)?)?);
    let months = i64::from(be_i32(b.get(12..16)?)?);
    let mut out = String::new();
    let mut is_before = false;
    let mut part = |out: &mut String, value: i64, unit: &str| {
        if value == 0 {
            return;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        if is_before && value > 0 {
            out.push('+');
        }
        let plural = if value == 1 { "" } else { "s" };
        let _ = write!(out, "{value} {unit}{plural}");
        is_before = value < 0;
    };
    part(&mut out, months / 12, "year");
    part(&mut out, months % 12, "mon");
    part(&mut out, days, "day");
    if out.is_empty() || micros != 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        let total = micros.unsigned_abs();
        let secs = total / 1_000_000;
        let sign = if micros < 0 {
            "-"
        } else if is_before {
            "+"
        } else {
            ""
        };
        let _ = write!(
            out,
            "{sign}{:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        );
        push_fraction(&mut out, i64::try_from(total % 1_000_000).ok()?);
    }
    Some(out)
}

fn fmt_timetz(b: &[u8]) -> Option<String> {
    let micros = be_i64(b.get(0..8)?)?;
    // The zone is in seconds west of Greenwich.
    let east = -i64::from(be_i32(b.get(8..12)?)?);
    let mut out = fmt_time(micros);
    out.push(if east < 0 { '-' } else { '+' });
    let a = east.abs();
    let _ = write!(out, "{:02}", a / 3600);
    if a % 3600 != 0 {
        let _ = write!(out, ":{:02}", a / 60 % 60);
        if a % 60 != 0 {
            let _ = write!(out, ":{:02}", a % 60);
        }
    }
    Some(out)
}

fn fmt_inet(b: &[u8], cidr_type: bool) -> Option<String> {
    let family = *b.first()?;
    let bits = *b.get(1)?;
    let addr = b.get(4..)?;
    let (text, max) = match (family, addr.len()) {
        (2, 4) => (
            Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
            32,
        ),
        (3, 16) => {
            let octets: [u8; 16] = addr.try_into().ok()?;
            (Ipv6Addr::from(octets).to_string(), 128)
        }
        _ => return None,
    };
    Some(if cidr_type || bits != max {
        format!("{text}/{bits}")
    } else {
        text
    })
}

fn fmt_uuid(b: &[u8]) -> Option<String> {
    if b.len() != 16 {
        return None;
    }
    let mut out = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02x}");
    }
    Some(out)
}

fn fmt_mac(b: &[u8]) -> Option<String> {
    if !matches!(b.len(), 6 | 8) {
        return None;
    }
    Some(
        b.iter()
            .map(|x| format!("{x:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn hex_bytea(b: &[u8]) -> String {
    let mut out = String::from("\\x");
    for byte in b {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The `PostgreSQL` text output form of the types shown as text (and of array elements).
///
/// `None` for a type this module does not know or bytes of the wrong shape.
fn scalar_text(oid: u32, b: &[u8]) -> Option<String> {
    match oid {
        16 => b
            .first()
            .map(|v| if *v == 0 { "f" } else { "t" }.to_owned()),
        21 => be_i16(b).map(|v| v.to_string()),
        23 => be_i32(b).map(|v| v.to_string()),
        20 => be_i64(b).map(|v| v.to_string()),
        26 => be_u32(b).map(|v| v.to_string()),
        700 => be_f32(b).map(|v| float_text(f64::from(v), &v.to_string())),
        701 => be_f64(b).map(|v| float_text(v, &v.to_string())),
        1700 => decode_numeric(b),
        25 | 1042 | 1043 | 19 | 142 | 705 | 114 => std::str::from_utf8(b).ok().map(str::to_owned),
        3802 => match b.split_first() {
            Some((1, rest)) => std::str::from_utf8(rest).ok().map(str::to_owned),
            _ => None,
        },
        17 => Some(hex_bytea(b)),
        1082 => be_i32(b).map(|d| match d {
            i32::MAX => "infinity".to_owned(),
            i32::MIN => "-infinity".to_owned(),
            d => fmt_date(i64::from(d)),
        }),
        1083 => be_i64(b).map(fmt_time),
        1114 => be_i64(b).map(|m| match m {
            i64::MAX => "infinity".to_owned(),
            i64::MIN => "-infinity".to_owned(),
            m => fmt_timestamp(m),
        }),
        1184 => be_i64(b).map(|m| match m {
            i64::MAX => "infinity".to_owned(),
            i64::MIN => "-infinity".to_owned(),
            m => format!("{}+00", fmt_timestamp(m)),
        }),
        1266 => fmt_timetz(b),
        1186 => fmt_interval(b),
        869 => fmt_inet(b, false),
        650 => fmt_inet(b, true),
        2950 => fmt_uuid(b),
        829 | 774 => fmt_mac(b),
        _ => None,
    }
}

fn float_text(v: f64, shortest: &str) -> String {
    if v.is_nan() {
        "NaN".to_owned()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        shortest.to_owned()
    }
}

/// `PostgreSQL`'s `MAXDIM`: the most dimensions an array can have.
const MAX_ARRAY_DIMS: usize = 6;

/// Decodes a binary array into an array literal such as `{1,NULL,"a b"}`.
///
/// Multiple dimensions nest; a lower bound other than 1 adds the `[lb:ub]=` prefix.
fn decode_array(b: &[u8]) -> Option<String> {
    let ndim = usize::try_from(be_i32(b.get(0..4)?)?).ok()?;
    if ndim > MAX_ARRAY_DIMS {
        return None;
    }
    let elem_oid = be_u32(b.get(8..12)?)?;
    let mut pos = 12;
    let mut dims = Vec::with_capacity(ndim);
    for _ in 0..ndim {
        let len = usize::try_from(be_i32(b.get(pos..pos + 4)?)?).ok()?;
        let lower = be_i32(b.get(pos + 4..pos + 8)?)?;
        dims.push((len, lower));
        pos += 8;
    }
    // Every element takes at least a 4-byte length word, so the element count is bounded by the
    // rest of the buffer (checked before anything is written or recursed into).
    let mut total = 1_usize;
    for (len, _) in &dims {
        total = total.checked_mul(*len)?;
    }
    if !dims.is_empty() && total > b.len().saturating_sub(pos) / 4 && total != 0 {
        return None;
    }
    let mut out = String::new();
    if dims.iter().any(|(_, lower)| *lower != 1) {
        for (len, lower) in &dims {
            let upper = i64::from(*lower) + i64::try_from(*len).ok()? - 1;
            let _ = write!(out, "[{lower}:{upper}]");
        }
        out.push('=');
    }
    if dims.is_empty() || dims.iter().any(|(len, _)| *len == 0) {
        out.push_str("{}");
        return Some(out);
    }
    let lens: Vec<usize> = dims.iter().map(|(len, _)| *len).collect();
    write_level(&mut out, &lens, b, &mut pos, elem_oid)?;
    Some(out)
}

fn write_level(
    out: &mut String,
    lens: &[usize],
    b: &[u8],
    pos: &mut usize,
    elem_oid: u32,
) -> Option<()> {
    let (len, rest) = lens.split_first()?;
    out.push('{');
    for i in 0..*len {
        if i > 0 {
            out.push(',');
        }
        if rest.is_empty() {
            let n = be_i32(b.get(*pos..*pos + 4)?)?;
            *pos += 4;
            if n < 0 {
                out.push_str("NULL");
            } else {
                let n = usize::try_from(n).ok()?;
                let data = b.get(*pos..*pos + n)?;
                *pos += n;
                push_element(out, elem_oid, data);
            }
        } else {
            write_level(out, rest, b, pos, elem_oid)?;
        }
    }
    out.push('}');
    Some(())
}

fn push_element(out: &mut String, oid: u32, data: &[u8]) {
    let text = scalar_text(oid, data)
        .or_else(|| std::str::from_utf8(data).ok().map(str::to_owned))
        .unwrap_or_else(|| hex_bytea(data));
    let needs_quotes = text.is_empty()
        || text.eq_ignore_ascii_case("null")
        || text.chars().any(|c| {
            matches!(c, '"' | '\\' | '{' | '}' | ',') || c.is_ascii_whitespace() || c == '\u{b}'
        });
    if needs_quotes {
        out.push('"');
        for c in text.chars() {
            if matches!(c, '"' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    } else {
        out.push_str(&text);
    }
}

/// A `sqlx` query with the parameters bound in order.
pub(crate) type BoundQuery = Query<'static, Postgres, PgArguments>;

/// Builds a query for `sql`, binds `params` in order and returns the parameter types.
///
/// `Null` binds as a null `int8`. Decimal, date, time and timestamp values bind as text, so the
/// SQL needs a cast (`$1::numeric`).
///
/// # Errors
/// Returns [`DriverError::Unsupported`] for a value variant this driver cannot bind.
pub(crate) fn bind_params(
    sql: &str,
    params: &[Value],
) -> Result<(BoundQuery, Vec<PgTypeInfo>), DriverError> {
    let mut query = sqlx::query(AssertSqlSafe(sql.to_owned()));
    let mut types = Vec::with_capacity(params.len());
    let text = <String as sqlx::Type<Postgres>>::type_info;
    for param in params {
        match param {
            Value::Null => {
                query = query.bind(None::<i64>);
                types.push(<i64 as sqlx::Type<Postgres>>::type_info());
            }
            Value::Bool(b) => {
                query = query.bind(*b);
                types.push(<bool as sqlx::Type<Postgres>>::type_info());
            }
            Value::Int(i) => {
                query = query.bind(*i);
                types.push(<i64 as sqlx::Type<Postgres>>::type_info());
            }
            Value::Float(f) => {
                query = query.bind(*f);
                types.push(<f64 as sqlx::Type<Postgres>>::type_info());
            }
            Value::Bytes(b) => {
                query = query.bind(b.clone());
                types.push(<Vec<u8> as sqlx::Type<Postgres>>::type_info());
            }
            Value::Text(s) | Value::Json(s) => {
                query = query.bind(s.clone());
                types.push(text());
            }
            Value::Decimal(v) => {
                query = query.bind(v.to_string());
                types.push(text());
            }
            Value::Date(v) => {
                query = query.bind(v.to_string());
                types.push(text());
            }
            Value::Time(v) => {
                query = query.bind(v.to_string());
                types.push(text());
            }
            Value::Timestamp(v) => {
                query = query.bind(v.to_string());
                types.push(text());
            }
            Value::TimestampTz(v) => {
                query = query.bind(v.to_string());
                types.push(text());
            }
            other => {
                return Err(DriverError::Unsupported(format!(
                    "PostgreSQL parameter {other:?}"
                )));
            }
        }
    }
    Ok((query, types))
}

/// Turns SQL text into the `sqlx` string type.
pub(crate) fn sql_str(sql: &str) -> sqlx::SqlStr {
    AssertSqlSafe(sql.to_owned()).into_sql_str()
}

/// Maps a `sqlx` error raised while preparing, executing or fetching.
///
/// SQLSTATE `57014` is [`DriverError::Cancelled`] only when `cancelled` is set (the user asked);
/// otherwise (a `statement_timeout`) it is a query error.
pub(crate) fn query_error(err: sqlx::Error, cancelled: bool) -> DriverError {
    match err {
        sqlx::Error::Database(d) => {
            if cancelled && d.code().as_deref() == Some(QUERY_CANCELED) {
                DriverError::Cancelled
            } else {
                DriverError::Query(d.message().to_owned())
            }
        }
        other => DriverError::Backend(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_header(ndim: i32, dims: &[(i32, i32)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(ndim.to_be_bytes());
        b.extend(0_i32.to_be_bytes());
        b.extend(23_u32.to_be_bytes());
        for (len, lower) in dims {
            b.extend(len.to_be_bytes());
            b.extend(lower.to_be_bytes());
        }
        b
    }

    #[test]
    fn hostile_array_headers_are_rejected() {
        assert!(decode_array(&raw_header(i32::MAX, &[])).is_none());
        assert!(decode_array(&raw_header(7, &[(1, 1); 7])).is_none());
        assert!(decode_array(&raw_header(-1, &[])).is_none());
        assert!(decode_array(&raw_header(i32::MIN, &[])).is_none());
        // Deep nesting of length-1 dimensions stays rejected.
        assert!(decode_array(&raw_header(100_000, &vec![(1, 1); 100_000])).is_none());
    }

    #[test]
    fn hostile_array_counts_and_truncation_are_rejected() {
        // Claims 2^31-1 elements with no data.
        assert!(decode_array(&raw_header(1, &[(i32::MAX, 1)])).is_none());
        // Product of dimensions overflows the buffer.
        assert!(decode_array(&raw_header(2, &[(60_000, 1), (60_000, 1)])).is_none());
        // Negative length.
        assert!(decode_array(&raw_header(1, &[(-1, 1)])).is_none());
        // Truncated header and truncated element.
        assert!(decode_array(&[0, 0, 0, 1, 0, 0]).is_none());
        let mut b = raw_header(1, &[(1, 1)]);
        b.extend(100_i32.to_be_bytes());
        b.extend([1, 2]);
        assert!(decode_array(&b).is_none());
        // Still decodes a valid empty and a valid one-element array.
        assert_eq!(decode_array(&raw_header(0, &[])).as_deref(), Some("{}"));
        let mut ok = raw_header(1, &[(1, 1)]);
        ok.extend(4_i32.to_be_bytes());
        ok.extend(7_i32.to_be_bytes());
        assert_eq!(decode_array(&ok).as_deref(), Some("{7}"));
    }

    fn num(ndigits: u16, weight: i16, sign: u16, dscale: u16, digits: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(ndigits.to_be_bytes());
        b.extend(weight.to_be_bytes());
        b.extend(sign.to_be_bytes());
        b.extend(dscale.to_be_bytes());
        for d in digits {
            b.extend(d.to_be_bytes());
        }
        b
    }

    fn dec(b: &[u8]) -> Value {
        decode_binary(Some(1700), false, b)
    }

    fn decimal(text: &str) -> Value {
        Value::Decimal(text.parse().unwrap())
    }

    #[test]
    fn numeric_vectors() {
        assert_eq!(dec(&num(0, 0, 0, 0, &[])), decimal("0"));
        assert_eq!(dec(&num(2, 0, 0, 2, &[1, 5000])), decimal("1.50"));
        assert_eq!(
            dec(&num(3, 1, 0x4000, 3, &[1, 2345, 6780])),
            decimal("-12345.678")
        );
        assert_eq!(dec(&num(1, -1, 0, 4, &[1])), decimal("0.0001"));
        assert_eq!(dec(&num(1, 2, 0, 0, &[1])), decimal("100000000"));
        assert_eq!(dec(&num(1, 0, 0, 0, &[42])), decimal("42"));
        assert_eq!(dec(&num(0, 0, 0, 3, &[])), decimal("0.000"));
        assert_eq!(dec(&num(0, 0, 0xC000, 0, &[])), Value::Text("NaN".into()));
        assert_eq!(
            dec(&num(0, 0, 0xD000, 0, &[])),
            Value::Text("Infinity".into())
        );
        assert_eq!(
            dec(&num(0, 0, 0xF000, 0, &[])),
            Value::Text("-Infinity".into())
        );
        // A truncated buffer falls back instead of failing.
        assert!(matches!(dec(&[0xff, 1, 0]), Value::Bytes(_)));
    }

    #[test]
    fn scalars() {
        assert_eq!(decode_binary(Some(16), false, &[1]), Value::Bool(true));
        assert_eq!(
            decode_binary(Some(21), false, &(-2_i16).to_be_bytes()),
            Value::Int(-2)
        );
        assert_eq!(
            decode_binary(Some(23), false, &7_i32.to_be_bytes()),
            Value::Int(7)
        );
        assert_eq!(
            decode_binary(Some(20), false, &i64::MIN.to_be_bytes()),
            Value::Int(i64::MIN)
        );
        assert_eq!(
            decode_binary(Some(26), false, &u32::MAX.to_be_bytes()),
            Value::Int(i64::from(u32::MAX))
        );
        assert_eq!(
            decode_binary(Some(701), false, &1.5_f64.to_be_bytes()),
            Value::Float(1.5)
        );
        assert_eq!(
            decode_binary(Some(700), false, &0.1_f32.to_be_bytes()),
            Value::Float(0.1)
        );
        assert_eq!(
            decode_binary(Some(25), false, b"hi"),
            Value::Text("hi".into())
        );
        assert_eq!(
            decode_binary(Some(17), false, &[0, 255]),
            Value::Bytes(vec![0, 255])
        );
        assert_eq!(
            decode_binary(Some(114), false, b"{\"a\":1}"),
            Value::Json("{\"a\":1}".into())
        );
        let mut jsonb = vec![1];
        jsonb.extend(b"[1]");
        assert_eq!(
            decode_binary(Some(3802), false, &jsonb),
            Value::Json("[1]".into())
        );
        // Unknown types: UTF-8 is text, anything else bytes.
        assert_eq!(
            decode_binary(Some(99_999), false, b"red"),
            Value::Text("red".into())
        );
        assert_eq!(
            decode_binary(Some(99_999), false, &[0xff, 0xfe]),
            Value::Bytes(vec![0xff, 0xfe])
        );
        assert_eq!(decode_binary(None, false, b"x"), Value::Text("x".into()));
        // A wrong-sized integer falls back, never fails.
        assert!(matches!(
            decode_binary(Some(23), false, &[0xff]),
            Value::Bytes(_)
        ));
    }

    #[test]
    fn dates_and_times() {
        let date = |d: i32| decode_binary(Some(1082), false, &d.to_be_bytes());
        assert_eq!(date(0), Value::Date("2000-01-01".parse().unwrap()));
        assert_eq!(date(-1), Value::Date("1999-12-31".parse().unwrap()));
        assert_eq!(date(366), Value::Date("2001-01-01".parse().unwrap()));
        assert_eq!(date(i32::MAX), Value::Text("infinity".into()));
        assert_eq!(date(i32::MIN), Value::Text("-infinity".into()));
        let time = |m: i64| decode_binary(Some(1083), false, &m.to_be_bytes());
        assert_eq!(time(0), Value::Time("00:00:00".parse().unwrap()));
        assert_eq!(
            time(3_723_500_000),
            Value::Time("01:02:03.5".parse().unwrap())
        );
        assert_eq!(time(MICROS_PER_DAY), Value::Text("24:00:00".into()));
        let ts = |m: i64| decode_binary(Some(1114), false, &m.to_be_bytes());
        assert_eq!(
            ts(0),
            Value::Timestamp("2000-01-01 00:00:00".parse().unwrap())
        );
        assert_eq!(
            ts(-1),
            Value::Timestamp("1999-12-31 23:59:59.999999".parse().unwrap())
        );
        assert_eq!(ts(i64::MAX), Value::Text("infinity".into()));
        assert_eq!(ts(i64::MIN), Value::Text("-infinity".into()));
        let tz = |m: i64| decode_binary(Some(1184), false, &m.to_be_bytes());
        let Value::TimestampTz(v) = tz(86_400 * MICROS_PER_SEC) else {
            panic!("expected timestamptz");
        };
        assert_eq!(v.to_string(), "2000-01-02 00:00:00+00:00");
        assert_eq!(v.offset_seconds(), 0);
        assert_eq!(tz(i64::MAX), Value::Text("infinity".into()));
        assert_eq!(tz(i64::MIN), Value::Text("-infinity".into()));
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (2000, 1, 1));
        assert_eq!(civil_from_days(59), (2000, 2, 29));
        assert_eq!(civil_from_days(-10_958), (1969, 12, 31));
        assert_eq!(fmt_date(-10_957), "1970-01-01");
    }

    #[test]
    fn interval_text() {
        let iv = |micros: i64, days: i32, months: i32| {
            let mut b = Vec::new();
            b.extend(micros.to_be_bytes());
            b.extend(days.to_be_bytes());
            b.extend(months.to_be_bytes());
            decode_binary(Some(1186), false, &b)
        };
        let text = |s: &str| Value::Text(s.to_owned());
        assert_eq!(
            iv(14_706_000_000, 3, 14),
            text("1 year 2 mons 3 days 04:05:06")
        );
        assert_eq!(iv(0, 0, 0), text("00:00:00"));
        assert_eq!(iv(0, 1, 0), text("1 day"));
        assert_eq!(iv(0, -2, 0), text("-2 days"));
        assert_eq!(iv(-1_500_000, 0, 0), text("-00:00:01.5"));
        assert_eq!(
            iv(3_600_000_000, -1, -12),
            text("-1 years -1 days +01:00:00")
        );
    }

    #[test]
    fn network_uuid_timetz() {
        let inet = |fam: u8, bits: u8, addr: &[u8]| {
            let mut b = vec![fam, bits, 0, u8::try_from(addr.len()).unwrap()];
            b.extend(addr);
            b
        };
        let text = |s: &str| Value::Text(s.to_owned());
        assert_eq!(
            decode_binary(Some(869), false, &inet(2, 32, &[10, 0, 0, 1])),
            text("10.0.0.1")
        );
        assert_eq!(
            decode_binary(Some(869), false, &inet(2, 24, &[10, 0, 0, 1])),
            text("10.0.0.1/24")
        );
        assert_eq!(
            decode_binary(Some(650), false, &inet(2, 8, &[10, 0, 0, 0])),
            text("10.0.0.0/8")
        );
        let mut v6 = [0_u8; 16];
        v6[15] = 1;
        assert_eq!(
            decode_binary(Some(869), false, &inet(3, 128, &v6)),
            text("::1")
        );
        let uuid: Vec<u8> = (0..16).map(|i| i * 16 + 10).collect();
        assert_eq!(
            decode_binary(Some(2950), false, &uuid),
            text("0a1a2a3a-4a5a-6a7a-8a9a-aabacadaeafa")
        );
        let mut tz = Vec::new();
        tz.extend(3_723_000_000_i64.to_be_bytes());
        tz.extend((-7200_i32).to_be_bytes());
        assert_eq!(decode_binary(Some(1266), false, &tz), text("01:02:03+02"));
        let mut tz = Vec::new();
        tz.extend(0_i64.to_be_bytes());
        tz.extend(19_800_i32.to_be_bytes());
        assert_eq!(
            decode_binary(Some(1266), false, &tz),
            text("00:00:00-05:30")
        );
    }

    fn array_header(dims: &[(i32, i32)], elem: u32, has_null: bool) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(i32::try_from(dims.len()).unwrap().to_be_bytes());
        b.extend(i32::from(has_null).to_be_bytes());
        b.extend(elem.to_be_bytes());
        for (len, lower) in dims {
            b.extend(len.to_be_bytes());
            b.extend(lower.to_be_bytes());
        }
        b
    }

    fn elem(b: &mut Vec<u8>, data: Option<&[u8]>) {
        match data {
            None => b.extend((-1_i32).to_be_bytes()),
            Some(d) => {
                b.extend(i32::try_from(d.len()).unwrap().to_be_bytes());
                b.extend(d);
            }
        }
    }

    #[test]
    fn arrays() {
        let mut b = array_header(&[(3, 1)], 23, true);
        elem(&mut b, Some(&1_i32.to_be_bytes()));
        elem(&mut b, None);
        elem(&mut b, Some(&3_i32.to_be_bytes()));
        assert_eq!(
            decode_binary(None, true, &b),
            Value::Text("{1,NULL,3}".into())
        );

        let mut b = array_header(&[(4, 1)], 25, false);
        for s in ["a b", "", "NULL", "q\"u\\o"] {
            elem(&mut b, Some(s.as_bytes()));
        }
        assert_eq!(
            decode_binary(None, true, &b),
            Value::Text(r#"{"a b","","NULL","q\"u\\o"}"#.into())
        );

        let mut b = array_header(&[(2, 1), (2, 1)], 23, false);
        for v in 1..=4_i32 {
            elem(&mut b, Some(&v.to_be_bytes()));
        }
        assert_eq!(
            decode_binary(None, true, &b),
            Value::Text("{{1,2},{3,4}}".into())
        );

        let mut b = array_header(&[(2, 0)], 16, false);
        elem(&mut b, Some(&[1]));
        elem(&mut b, Some(&[0]));
        assert_eq!(
            decode_binary(None, true, &b),
            Value::Text("[0:1]={t,f}".into())
        );

        let b = array_header(&[], 23, false);
        assert_eq!(decode_binary(None, true, &b), Value::Text("{}".into()));

        // Truncated: falls back, never fails.
        let mut b = array_header(&[(2, 1)], 23, false);
        elem(&mut b, Some(&1_i32.to_be_bytes()));
        assert!(matches!(
            decode_binary(None, true, &b),
            Value::Bytes(_) | Value::Text(_)
        ));
    }

    #[test]
    fn text_format() {
        assert_eq!(decode_text(Some(16), b"t"), Value::Bool(true));
        assert_eq!(decode_text(Some(23), b"42"), Value::Int(42));
        assert_eq!(decode_text(Some(1700), b"1.50"), decimal("1.50"));
        assert_eq!(
            decode_text(Some(1082), b"2024-01-02"),
            Value::Date("2024-01-02".parse().unwrap())
        );
        assert_eq!(
            decode_text(Some(1082), b"infinity"),
            Value::Text("infinity".into())
        );
        assert_eq!(
            decode_text(Some(17), b"\\x00ff"),
            Value::Bytes(vec![0, 255])
        );
        assert_eq!(decode_text(None, b"x"), Value::Text("x".into()));
    }

    #[test]
    fn oid_map() {
        let cases = [
            (16, DataType::Boolean),
            (20, DataType::Integer),
            (21, DataType::Integer),
            (23, DataType::Integer),
            (26, DataType::Integer),
            (700, DataType::Float),
            (701, DataType::Float),
            (1700, DataType::Decimal),
            (25, DataType::Text),
            (1042, DataType::Text),
            (1043, DataType::Text),
            (19, DataType::Text),
            (17, DataType::Binary),
            (1082, DataType::Date),
            (1083, DataType::Time),
            (1114, DataType::Timestamp),
            (1184, DataType::TimestampTz),
            (114, DataType::Json),
            (3802, DataType::Json),
            (2950, DataType::Text),
            (1266, DataType::Text),
            (1186, DataType::Text),
            (869, DataType::Text),
            (650, DataType::Text),
            (600, DataType::Other),
        ];
        for (oid, expected) in cases {
            assert_eq!(data_type(Some(oid), false), expected, "{oid}");
        }
        assert_eq!(data_type(Some(1007), true), DataType::Text);
        assert_eq!(data_type(None, false), DataType::Other);
    }

    #[test]
    fn bind_all_variants() {
        let params = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(1),
            Value::Float(1.0),
            Value::Text("a".into()),
            Value::Bytes(vec![1]),
            Value::Json("{}".into()),
            Value::Decimal("1.5".parse().unwrap()),
            Value::Date("2024-01-02".parse().unwrap()),
        ];
        let (_, types) = bind_params("select 1", &params).unwrap();
        assert_eq!(types.len(), params.len());
    }

    #[test]
    fn non_database_errors_are_backend() {
        assert!(matches!(
            query_error(sqlx::Error::PoolClosed, true),
            DriverError::Backend(_)
        ));
    }
}
