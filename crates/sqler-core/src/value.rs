// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fmt;
use std::str::FromStr;

use crate::CoreError;

/// The logical type of a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DataType {
    /// Boolean.
    Boolean,
    /// Integer of any width.
    Integer,
    /// Floating point number.
    Float,
    /// Exact decimal number.
    Decimal,
    /// Text.
    Text,
    /// Binary data.
    Binary,
    /// Calendar date.
    Date,
    /// Time of day.
    Time,
    /// Date and time without a zone.
    Timestamp,
    /// Date and time with a UTC offset.
    TimestampTz,
    /// JSON document.
    Json,
    /// Any type without a closer match.
    Other,
}

/// A single cell value.
///
/// Drivers return the typed variants where they can. A value that does not fit one
/// (`infinity`, `NaN`, a leap second, time with zone) is returned as [`Value::Text`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Text.
    Text(String),
    /// Binary data.
    Bytes(Vec<u8>),
    /// Exact decimal number.
    Decimal(Decimal),
    /// Calendar date.
    Date(Date),
    /// Time of day.
    Time(Time),
    /// Date and time without a zone.
    Timestamp(Timestamp),
    /// Date and time with a UTC offset.
    TimestampTz(TimestampTz),
    /// JSON document as raw server text; not validated. [`Display`](fmt::Display) prints it verbatim.
    Json(String),
}

impl Value {
    /// The data type of this value, or `None` for [`Value::Null`].
    #[must_use]
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Self::Null => None,
            Self::Bool(_) => Some(DataType::Boolean),
            Self::Int(_) => Some(DataType::Integer),
            Self::Float(_) => Some(DataType::Float),
            Self::Text(_) => Some(DataType::Text),
            Self::Bytes(_) => Some(DataType::Binary),
            Self::Decimal(_) => Some(DataType::Decimal),
            Self::Date(_) => Some(DataType::Date),
            Self::Time(_) => Some(DataType::Time),
            Self::Timestamp(_) => Some(DataType::Timestamp),
            Self::TimestampTz(_) => Some(DataType::TimestampTz),
            Self::Json(_) => Some(DataType::Json),
        }
    }

    /// Whether this value is [`Value::Null`].
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::Text(s) | Self::Json(s) => f.write_str(s),
            Self::Decimal(d) => d.fmt(f),
            Self::Date(d) => d.fmt(f),
            Self::Time(t) => t.fmt(f),
            Self::Timestamp(t) => t.fmt(f),
            Self::TimestampTz(t) => t.fmt(f),
            Self::Bytes(bytes) => {
                f.write_str("0x")?;
                for b in bytes {
                    write!(f, "{b:02x}")?;
                }
                Ok(())
            }
        }
    }
}

fn invalid(kind: &'static str, input: impl Into<String>) -> CoreError {
    CoreError::InvalidValue {
        kind,
        input: input.into(),
    }
}

const fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(year) => 29,
        _ => 28,
    }
}

/// Parses an all-digit string of exactly `min..=max` characters.
fn digits(s: &str, min: usize, max: usize) -> Option<u32> {
    if s.len() < min || s.len() > max || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A calendar date in the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// Validates and builds a date.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidValue`] if the month is not 1..=12 or the day does not
    /// exist in that month (leap years are honoured).
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self, CoreError> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return Err(invalid("date", format!("{year}-{month}-{day}")));
        }
        Ok(Self { year, month, day })
    }

    /// The year.
    #[must_use]
    pub fn year(&self) -> i32 {
        self.year
    }

    /// The month, 1..=12.
    #[must_use]
    pub fn month(&self) -> u8 {
        self.month
    }

    /// The day of the month, starting at 1.
    #[must_use]
    pub fn day(&self) -> u8 {
        self.day
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if (0..=9999).contains(&self.year) {
            write!(f, "{:04}", self.year)?;
        } else {
            write!(f, "{:+05}", self.year)?;
        }
        write!(f, "-{:02}-{:02}", self.month, self.day)
    }
}

impl FromStr for Date {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || invalid("date", s);
        let (signed, body) = match s.as_bytes().first() {
            Some(b'+' | b'-') => (true, &s[1..]),
            _ => (false, s),
        };
        let mut parts = body.split('-');
        let (y, m, d) = (parts.next(), parts.next(), parts.next());
        let (Some(y), Some(m), Some(d), None) = (y, m, d, parts.next()) else {
            return Err(bad());
        };
        let year = if signed {
            digits(y, 4, 10)
        } else {
            digits(y, 4, 4)
        }
        .map(i64::from)
        .map(|v| if s.starts_with('-') { -v } else { v })
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(bad)?;
        let month = digits(m, 2, 2)
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(bad)?;
        let day = digits(d, 2, 2)
            .and_then(|v| u8::try_from(v).ok())
            .ok_or_else(bad)?;
        Self::new(year, month, day).map_err(|_| bad())
    }
}

/// A time of day with nanosecond precision and no zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Time {
    hour: u8,
    minute: u8,
    second: u8,
    nanosecond: u32,
}

impl Time {
    /// Validates and builds a time of day.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidValue`] unless hour < 24, minute < 60, second < 60 and
    /// nanosecond < 1 000 000 000.
    pub fn new(hour: u8, minute: u8, second: u8, nanosecond: u32) -> Result<Self, CoreError> {
        if hour >= 24 || minute >= 60 || second >= 60 || nanosecond >= 1_000_000_000 {
            return Err(invalid(
                "time",
                format!("{hour}:{minute}:{second}.{nanosecond}"),
            ));
        }
        Ok(Self {
            hour,
            minute,
            second,
            nanosecond,
        })
    }

    /// The hour, 0..24.
    #[must_use]
    pub fn hour(&self) -> u8 {
        self.hour
    }

    /// The minute, 0..60.
    #[must_use]
    pub fn minute(&self) -> u8 {
        self.minute
    }

    /// The second, 0..60.
    #[must_use]
    pub fn second(&self) -> u8 {
        self.second
    }

    /// The nanosecond within the second.
    #[must_use]
    pub fn nanosecond(&self) -> u32 {
        self.nanosecond
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)?;
        if self.nanosecond != 0 {
            let frac = format!("{:09}", self.nanosecond);
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        Ok(())
    }
}

impl FromStr for Time {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_time(s).ok_or_else(|| invalid("time", s))
    }
}

/// Parses `HH:MM[:SS[.f]]` with a 1-9 digit fraction.
fn parse_time(s: &str) -> Option<Time> {
    let mut parts = s.split(':');
    let hour = u8::try_from(digits(parts.next()?, 2, 2)?).ok()?;
    let minute = u8::try_from(digits(parts.next()?, 2, 2)?).ok()?;
    let (mut second, mut nanosecond) = (0, 0);
    if let Some(sec) = parts.next() {
        let (whole, frac) = match sec.split_once('.') {
            Some((w, f)) => (w, Some(f)),
            None => (sec, None),
        };
        second = u8::try_from(digits(whole, 2, 2)?).ok()?;
        if let Some(frac) = frac {
            let n = digits(frac, 1, 9)?;
            // Scale the fraction to nine digits.
            let missing = 9 - u32::try_from(frac.len()).ok()?;
            nanosecond = n * 10u32.pow(missing);
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Time::new(hour, minute, second, nanosecond).ok()
}

/// A date and time of day without a zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    date: Date,
    time: Time,
}

impl Timestamp {
    /// Combines a date and a time of day.
    #[must_use]
    pub fn new(date: Date, time: Time) -> Self {
        Self { date, time }
    }

    /// The date part.
    #[must_use]
    pub fn date(&self) -> Date {
        self.date
    }

    /// The time part.
    #[must_use]
    pub fn time(&self) -> Time {
        self.time
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.date, self.time)
    }
}

/// Splits `date<sep>time` at the first space or `T`.
fn split_timestamp(s: &str) -> Option<(&str, &str)> {
    let i = s.find([' ', 'T'])?;
    Some((&s[..i], &s[i + 1..]))
}

impl FromStr for Timestamp {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed = split_timestamp(s)
            .and_then(|(d, t)| Some(Timestamp::new(d.parse().ok()?, parse_time(t)?)));
        parsed.ok_or_else(|| invalid("timestamp", s))
    }
}

/// A date and time with a UTC offset.
///
/// `local` is the wall clock at that offset, not UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimestampTz {
    local: Timestamp,
    offset_seconds: i32,
}

impl TimestampTz {
    /// Builds a timestamp from the wall clock at `offset_seconds` east of UTC.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidValue`] unless |offset| < 86 400.
    pub fn new(local: Timestamp, offset_seconds: i32) -> Result<Self, CoreError> {
        if offset_seconds.unsigned_abs() >= 86_400 {
            return Err(invalid("timestamptz", format!("offset {offset_seconds}")));
        }
        Ok(Self {
            local,
            offset_seconds,
        })
    }

    /// The wall clock at the offset.
    #[must_use]
    pub fn local(&self) -> Timestamp {
        self.local
    }

    /// The offset east of UTC, in seconds.
    #[must_use]
    pub fn offset_seconds(&self) -> i32 {
        self.offset_seconds
    }
}

impl fmt::Display for TimestampTz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = self.offset_seconds.unsigned_abs();
        let sign = if self.offset_seconds < 0 { '-' } else { '+' };
        write!(f, "{}{sign}{:02}:{:02}", self.local, o / 3600, o / 60 % 60)?;
        if !o.is_multiple_of(60) {
            write!(f, ":{:02}", o % 60)?;
        }
        Ok(())
    }
}

/// Parses `Z`, `z`, `±HH`, `±HHMM`, `±HH:MM` or `±HH:MM:SS` into seconds.
fn parse_zone(zone: &str) -> Option<i32> {
    if matches!(zone, "Z" | "z") {
        return Some(0);
    }
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &zone[1..];
    let (hours, minutes, seconds) = if body.contains(':') {
        let mut parts = body.split(':');
        let hours = digits(parts.next()?, 2, 2)?;
        let minutes = digits(parts.next()?, 2, 2)?;
        let seconds = match parts.next() {
            Some(text) => digits(text, 2, 2)?,
            None => 0,
        };
        if parts.next().is_some() {
            return None;
        }
        (hours, minutes, seconds)
    } else {
        match body.len() {
            2 => (digits(body, 2, 2)?, 0, 0),
            4 => (
                digits(body.get(..2)?, 2, 2)?,
                digits(body.get(2..)?, 2, 2)?,
                0,
            ),
            _ => return None,
        }
    };
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    i32::try_from(hours * 3600 + minutes * 60 + seconds)
        .ok()
        .map(|total| total * sign)
}

impl FromStr for TimestampTz {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed = (|| {
            let (d, t) = split_timestamp(s)?;
            let zone_at = t.find(['Z', 'z', '+', '-'])?;
            let local = Timestamp::new(d.parse().ok()?, parse_time(&t[..zone_at])?);
            Self::new(local, parse_zone(&t[zone_at..])?).ok()
        })();
        parsed.ok_or_else(|| invalid("timestamptz", s))
    }
}

/// An exact decimal number kept as validated text, so the scale survives.
///
/// Matches `[+-]?(\d+(\.\d*)?|\.\d+)`: no exponent, no whitespace. A leading `+` is dropped;
/// `"1.50"` and `"1.5"` are different values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Decimal(String);

impl Decimal {
    /// The normalised text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Decimal {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s.strip_prefix(['+', '-']).unwrap_or(s);
        let (int, frac) = match body.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (body, None),
        };
        let all_digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
        let ok = all_digits(int)
            && frac.is_none_or(all_digits)
            && (!int.is_empty() || frac.is_some_and(|f| !f.is_empty()));
        if !ok {
            return Err(invalid("decimal", s));
        }
        Ok(Self(s.strip_prefix('+').unwrap_or(s).to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_types() {
        assert_eq!(Value::Null.data_type(), None);
        assert_eq!(Value::Bool(true).data_type(), Some(DataType::Boolean));
        assert_eq!(Value::Int(1).data_type(), Some(DataType::Integer));
        assert_eq!(Value::Float(1.5).data_type(), Some(DataType::Float));
        assert_eq!(Value::Text(String::new()).data_type(), Some(DataType::Text));
        assert_eq!(Value::Bytes(vec![]).data_type(), Some(DataType::Binary));
        let ts = ts("2024-01-02 03:04:05");
        let cases = [
            (Value::Decimal("1.5".parse().unwrap()), DataType::Decimal),
            (Value::Date(date(2024, 1, 2)), DataType::Date),
            (Value::Time(Time::new(1, 2, 3, 0).unwrap()), DataType::Time),
            (Value::Timestamp(ts), DataType::Timestamp),
            (
                Value::TimestampTz(TimestampTz::new(ts, 0).unwrap()),
                DataType::TimestampTz,
            ),
            (Value::Json("{}".into()), DataType::Json),
        ];
        for (v, t) in cases {
            assert_eq!(v.data_type(), Some(t));
        }
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn date_validation() {
        for (y, ok) in [(2024, true), (1900, false), (2000, true), (2023, false)] {
            assert_eq!(Date::new(y, 2, 29).is_ok(), ok, "{y}");
        }
        assert!(Date::new(2024, 2, 30).is_err());
        assert!(Date::new(2024, 4, 31).is_err());
        assert!(Date::new(2024, 12, 31).is_ok());
        assert!(Date::new(2024, 0, 1).is_err());
        assert!(Date::new(2024, 13, 1).is_err());
        assert!(Date::new(2024, 1, 0).is_err());
        assert!(Date::new(-4, 2, 29).is_ok());
        let d = date(2024, 2, 29);
        assert_eq!((d.year(), d.month(), d.day()), (2024, 2, 29));
        assert_eq!(
            Date::new(2024, 2, 30),
            Err(CoreError::InvalidValue {
                kind: "date",
                input: "2024-2-30".into()
            })
        );
    }

    #[test]
    fn date_display_and_parse() {
        for (d, text) in [
            (date(2024, 1, 2), "2024-01-02"),
            (date(5, 1, 2), "0005-01-02"),
            (date(0, 1, 1), "0000-01-01"),
            (date(9999, 12, 31), "9999-12-31"),
            (date(12345, 6, 7), "+12345-06-07"),
            (date(-1, 1, 1), "-0001-01-01"),
            (date(-12345, 1, 1), "-12345-01-01"),
        ] {
            assert_eq!(d.to_string(), text);
            assert_eq!(text.parse::<Date>().unwrap(), d);
        }
        assert_eq!("+0001-01-01".parse::<Date>().unwrap(), date(1, 1, 1));
        for bad in [
            "",
            "2024",
            "2024-01",
            "2024-1-02",
            "2024-01-2",
            "2024-02-30",
            "24-01-02",
            "12345-01-01",
            "2024-01-02-03",
            "2024-01-02 ",
            "x024-01-02",
            "+999-01-01",
            "--001-01-01",
            "20é4-01-02",
            "2024-0é-02",
            "２０２４-01-02",
            "+é234-01-02",
            "2024-01-02\u{0}",
        ] {
            assert!(bad.parse::<Date>().is_err(), "{bad:?}");
        }
        for year in [i32::MAX, i32::MIN, 1_000_000_000, -1_000_000_000] {
            let d = Date::new(year, 1, 1).unwrap();
            assert_eq!(d.to_string().parse::<Date>().unwrap(), d, "{year}");
        }
        assert!(date(2024, 1, 1) < date(2024, 1, 2));
    }

    #[test]
    fn time_validation_display_parse() {
        assert!(Time::new(23, 59, 59, 999_999_999).is_ok());
        assert!(Time::new(24, 0, 0, 0).is_err());
        assert!(Time::new(0, 60, 0, 0).is_err());
        assert!(Time::new(0, 0, 60, 0).is_err());
        assert!(Time::new(0, 0, 0, 1_000_000_000).is_err());
        let t = Time::new(1, 2, 3, 450_000_000).unwrap();
        assert_eq!(
            (t.hour(), t.minute(), t.second(), t.nanosecond()),
            (1, 2, 3, 450_000_000)
        );
        for (t, text) in [
            (Time::new(0, 0, 0, 0).unwrap(), "00:00:00"),
            (Time::new(1, 2, 3, 450_000_000).unwrap(), "01:02:03.45"),
            (Time::new(1, 2, 3, 1).unwrap(), "01:02:03.000000001"),
            (
                Time::new(23, 59, 59, 999_999_999).unwrap(),
                "23:59:59.999999999",
            ),
        ] {
            assert_eq!(t.to_string(), text);
            assert_eq!(text.parse::<Time>().unwrap(), t);
        }
        assert_eq!(
            "10:20".parse::<Time>().unwrap(),
            Time::new(10, 20, 0, 0).unwrap()
        );
        assert_eq!(
            "10:20:30.5".parse::<Time>().unwrap().nanosecond(),
            500_000_000
        );
        assert_eq!("10:20:30.000".parse::<Time>().unwrap().nanosecond(), 0);
        for bad in [
            "",
            "10",
            "24:00:00",
            "10:60",
            "10:20:60",
            "10:20:30.",
            "10:20:30.1234567890",
            "1:02:03",
            "10:20:30:40",
            "10:20:30 ",
            "10:20:30Z",
            "10:20:3x",
            "10:20:30.-1",
            "1é:20:30",
            "10:2é:30",
            "10:20:30.é",
            "１０:20:30",
        ] {
            assert!(bad.parse::<Time>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn timestamp_display_and_parse() {
        let t = ts("2024-01-02 03:04:05.5");
        assert_eq!(t.to_string(), "2024-01-02 03:04:05.5");
        assert_eq!((t.date(), t.time().second()), (date(2024, 1, 2), 5));
        assert_eq!(ts("2024-01-02T03:04:05.5"), t);
        assert_eq!(ts("2024-01-02 03:04").to_string(), "2024-01-02 03:04:00");
        assert_eq!(ts("-0001-01-01 00:00:00").date(), date(-1, 1, 1));
        for bad in [
            "",
            "2024-01-02",
            "2024-01-02 ",
            "2024-01-02 03:04:05Z",
            "2024-01-02 03:04:05+01:00",
            "2024-01-02 03:04:05-01",
            "2024-01-02  03:04:05",
            "2024-13-02 03:04:05",
            "03:04:05",
        ] {
            assert!(bad.parse::<Timestamp>().is_err(), "{bad:?}");
        }
        assert!(ts("2024-01-02 03:04:05") < ts("2024-01-02 03:04:06"));
    }

    #[test]
    fn timestamptz_validation_display_parse() {
        let local = ts("2024-01-02 03:04:05");
        assert!(TimestampTz::new(local, 86_399).is_ok());
        assert!(TimestampTz::new(local, -86_399).is_ok());
        assert!(TimestampTz::new(local, 86_400).is_err());
        assert!(TimestampTz::new(local, -86_400).is_err());
        assert!(TimestampTz::new(local, i32::MIN).is_err());
        let z = TimestampTz::new(local, 0).unwrap();
        assert_eq!((z.local(), z.offset_seconds()), (local, 0));
        assert_eq!(z.to_string(), "2024-01-02 03:04:05+00:00");
        for (offset, text) in [
            (3600, "2024-01-02 03:04:05+01:00"),
            (-19_800, "2024-01-02 03:04:05-05:30"),
            (3661, "2024-01-02 03:04:05+01:01:01"),
            (-1, "2024-01-02 03:04:05-00:00:01"),
        ] {
            let v = TimestampTz::new(local, offset).unwrap();
            assert_eq!(v.to_string(), text);
            assert_eq!(text.parse::<TimestampTz>().unwrap(), v);
        }
        for ok in [
            "2024-01-02 03:04:05Z",
            "2024-01-02T03:04:05z",
            "2024-01-02 03:04:05+00",
            "2024-01-02 03:04:05+0000",
            "2024-01-02T03:04:05.000+00:00",
        ] {
            assert_eq!(ok.parse::<TimestampTz>().unwrap(), z, "{ok}");
        }
        assert_eq!(
            "2024-01-02 03:04:05-0530"
                .parse::<TimestampTz>()
                .unwrap()
                .offset_seconds(),
            -19_800
        );
        assert_eq!(
            "2024-01-02 03:04:05+05"
                .parse::<TimestampTz>()
                .unwrap()
                .offset_seconds(),
            18_000
        );
        for bad in [
            "",
            "2024-01-02 03:04:05",
            "2024-01-02 03:04:05 Z",
            "2024-01-02 03:04:05 +01:00",
            "2024-01-02 03:04:05+24:00",
            "2024-01-02 03:04:05+1",
            "2024-01-02 03:04:05+01:60",
            "2024-01-02 03:04:05+123",
            "2024-01-02 03:04:05+01:00:00:00",
            "2024-01-02 03:04:05ZZ",
            "2024-01-02 03:04:05+aa",
            "2024-01-02 03:04:05+aéb",
            "2024-01-02 03:04:05+é",
            "2024-01-02 03:04:05+éé",
            "2024-01-02 03:04:05-éa",
            "2024-01-02 03:04:05+01:é",
            "2024-01-02 03:04:05+０１",
            "2024-01-02 03:04:05+\u{1F600}",
            "2024-01-02 03:04:05é",
            "é024-01-02 03:04:05Z",
        ] {
            assert!(bad.parse::<TimestampTz>().is_err(), "{bad:?}");
        }
        assert_eq!(
            "x".parse::<TimestampTz>(),
            Err(CoreError::InvalidValue {
                kind: "timestamptz",
                input: "x".into()
            })
        );
    }

    #[test]
    fn decimal_parse_and_display() {
        for (input, text) in [
            ("0", "0"),
            ("1.50", "1.50"),
            ("+1.5", "1.5"),
            ("-1.5", "-1.5"),
            ("5.", "5."),
            (".5", ".5"),
            ("-.5", "-.5"),
            ("00012", "00012"),
            (
                "123456789012345678901234567890.123456789012345678901234567890",
                "123456789012345678901234567890.123456789012345678901234567890",
            ),
        ] {
            let d: Decimal = input.parse().unwrap();
            assert_eq!(d.as_str(), text);
            assert_eq!(d.to_string(), text);
        }
        assert_ne!(
            "1.50".parse::<Decimal>().unwrap(),
            "1.5".parse::<Decimal>().unwrap()
        );
        assert_eq!(
            "+1".parse::<Decimal>().unwrap(),
            "1".parse::<Decimal>().unwrap()
        );
        for bad in [
            "", "+", "-", ".", "-.", "1e5", "1E5", " 1", "1 ", "1 2", "1.2.3", "--1", "+-1", "a",
            "NaN", "inf", "1,5", "1_0", "1é", "é", "1.é", "１２", "+é",
        ] {
            assert_eq!(
                bad.parse::<Decimal>(),
                Err(CoreError::InvalidValue {
                    kind: "decimal",
                    input: bad.into()
                }),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn null_check() {
        assert!(Value::Null.is_null());
        assert!(!Value::Int(0).is_null());
    }

    #[test]
    fn display() {
        assert_eq!(Value::Null.to_string(), "NULL");
        assert_eq!(Value::Bool(true).to_string(), "true");
        assert_eq!(Value::Bool(false).to_string(), "false");
        assert_eq!(Value::Int(-7).to_string(), "-7");
        assert_eq!(Value::Float(1.5).to_string(), "1.5");
        assert_eq!(Value::Text("a b".into()).to_string(), "a b");
        assert_eq!(Value::Bytes(vec![0, 0xab, 0xFF]).to_string(), "0x00abff");
        assert_eq!(Value::Bytes(vec![]).to_string(), "0x");
        assert_eq!(Value::Decimal("+2.50".parse().unwrap()).to_string(), "2.50");
        assert_eq!(Value::Date(date(2024, 1, 2)).to_string(), "2024-01-02");
        assert_eq!(
            Value::Time(Time::new(1, 2, 3, 0).unwrap()).to_string(),
            "01:02:03"
        );
        assert_eq!(
            Value::Timestamp(ts("2024-01-02T03:04:05")).to_string(),
            "2024-01-02 03:04:05"
        );
        let tz = TimestampTz::new(ts("2024-01-02 03:04:05"), 3600).unwrap();
        assert_eq!(
            Value::TimestampTz(tz).to_string(),
            "2024-01-02 03:04:05+01:00"
        );
        assert_eq!(Value::Json("{\"a\":\n1}".into()).to_string(), "{\"a\":\n1}");
    }
}
