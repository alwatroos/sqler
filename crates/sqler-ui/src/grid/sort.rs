// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Sorting: the sort specification and the total order over cells.
//!
//! [`compare_cells`] is a documented total order over [`CellRef`] (ADR-0031). NULL is greater than
//! every value; the direction of a sort key never moves NULLs, they stay last. Values of
//! different classes are ordered by class: boolean, number, date and timestamp, timestamp with
//! zone, time, text, binary, anything else. Numbers compare by their displayed decimal value,
//! exactly, so `1`, `1.0` and `1.00` tie. A 9-byte key per cell (`encode`) decides most
//! comparisons without touching the cell again.

use std::cmp::Ordering;

use sqler_driver_api::sqler_core::{Date, Time, Timestamp, TimestampTz, Value};

use super::store::CellRef;

/// How text is compared (sort and filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Collation {
    /// Case-insensitive: the lowercased code points first, then the raw code points.
    #[default]
    CaseInsensitive,
    /// Unicode code point order.
    CodePoint,
}

/// The direction of one sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Smallest first.
    Ascending,
    /// Largest first.
    Descending,
}

/// One sort key: a model column and a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKey {
    /// The model column.
    pub column: usize,
    /// The direction.
    pub direction: SortDirection,
}

/// The sort keys of a grid, most significant first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortSpec {
    /// The keys.
    pub keys: Vec<SortKey>,
}

impl SortSpec {
    /// Whether there is no key.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// A click on the sort indicator of `column`. Without `add`: ascending, descending, none,
    /// ascending again; any other spec becomes ascending on `column`. With `add` (Shift): an
    /// existing key goes ascending, descending, removed; a new column is appended ascending.
    pub fn click(&mut self, column: usize, add: bool) {
        use SortDirection::{Ascending, Descending};
        let pos = self.keys.iter().position(|k| k.column == column);
        if add {
            match pos {
                Some(i) if self.keys[i].direction == Ascending => {
                    self.keys[i].direction = Descending;
                }
                Some(i) => {
                    self.keys.remove(i);
                }
                None => self.keys.push(SortKey {
                    column,
                    direction: Ascending,
                }),
            }
            return;
        }
        let only = self.keys.len() == 1 && pos == Some(0);
        if only && self.keys[0].direction == Ascending {
            self.keys[0].direction = Descending;
        } else if only {
            self.keys.clear();
        } else {
            self.set_only(column, Ascending);
        }
    }

    /// Replaces the spec with one key.
    pub fn set_only(&mut self, column: usize, d: SortDirection) {
        self.keys.clear();
        self.keys.push(SortKey {
            column,
            direction: d,
        });
    }

    /// The header indicator of `column`: `↑` or `↓`, followed by its position (`↑1`, `↓2`) when
    /// there are two or more keys; `None` when the column is not a key.
    #[must_use]
    pub fn indicator(&self, column: usize) -> Option<String> {
        let i = self.keys.iter().position(|k| k.column == column)?;
        let arrow = match self.keys[i].direction {
            SortDirection::Ascending => "↑",
            SortDirection::Descending => "↓",
        };
        Some(if self.keys.len() > 1 {
            format!("{arrow}{}", i + 1)
        } else {
            arrow.to_owned()
        })
    }
}

// ---- classification ------------------------------------------------------------------------

/// A cell reduced to what the order looks at.
#[derive(Clone, Copy)]
enum Norm<'a> {
    Null,
    Bool(bool),
    Num(NumSrc<'a>),
    Date(Date),
    Ts(Timestamp),
    Tz(TimestampTz),
    Time(Time),
    Text(&'a str),
    Bytes(&'a [u8]),
    Other(&'a Value),
}

/// A number as it is stored.
#[derive(Clone, Copy)]
pub(crate) enum NumSrc<'a> {
    Int(i64),
    Float(f64),
    /// Decimal text (`[+-]?digits[.digits]`).
    Dec(&'a str),
}

const CLASS_BOOL: u8 = 0;
const CLASS_NUM: u8 = 1;
const CLASS_TS: u8 = 2;
const CLASS_TZ: u8 = 3;
const CLASS_TIME: u8 = 4;
const CLASS_TEXT: u8 = 5;
const CLASS_BYTES: u8 = 6;
const CLASS_OTHER: u8 = 7;

fn norm(c: CellRef<'_>) -> Norm<'_> {
    match c {
        CellRef::Null => Norm::Null,
        CellRef::Bool(b) => Norm::Bool(b),
        CellRef::Int(i) => Norm::Num(NumSrc::Int(i)),
        CellRef::Float(x) => Norm::Num(NumSrc::Float(x)),
        CellRef::Text(s) | CellRef::Json(s) => Norm::Text(s),
        CellRef::Bytes(b) => Norm::Bytes(b),
        CellRef::Value(v) => match v {
            Value::Null => Norm::Null,
            Value::Bool(b) => Norm::Bool(*b),
            Value::Int(i) => Norm::Num(NumSrc::Int(*i)),
            Value::Float(x) => Norm::Num(NumSrc::Float(*x)),
            Value::Decimal(d) => Norm::Num(NumSrc::Dec(d.as_str())),
            Value::Text(s) | Value::Json(s) => Norm::Text(s),
            Value::Bytes(b) => Norm::Bytes(b),
            Value::Date(d) => Norm::Date(*d),
            Value::Timestamp(t) => Norm::Ts(*t),
            Value::TimestampTz(t) => Norm::Tz(*t),
            Value::Time(t) => Norm::Time(*t),
            _ => Norm::Other(v),
        },
    }
}

fn class(n: &Norm<'_>) -> u8 {
    match n {
        Norm::Null => u8::MAX,
        Norm::Bool(_) => CLASS_BOOL,
        Norm::Num(_) => CLASS_NUM,
        Norm::Date(_) | Norm::Ts(_) => CLASS_TS,
        Norm::Tz(_) => CLASS_TZ,
        Norm::Time(_) => CLASS_TIME,
        Norm::Text(_) => CLASS_TEXT,
        Norm::Bytes(_) => CLASS_BYTES,
        Norm::Other(_) => CLASS_OTHER,
    }
}

// ---- temporal ------------------------------------------------------------------------------

const NANOS_PER_SEC: i128 = 1_000_000_000;

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(date: Date) -> i64 {
    let (m, d) = (i64::from(date.month()), i64::from(date.day()));
    let y = i64::from(date.year()) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn time_nanos(t: Time) -> i128 {
    (i128::from(t.hour()) * 3600 + i128::from(t.minute()) * 60 + i128::from(t.second()))
        * NANOS_PER_SEC
        + i128::from(t.nanosecond())
}

/// Nanoseconds since 1970 of a date at 00:00.
pub(crate) fn date_instant(d: Date) -> i128 {
    i128::from(days_from_civil(d)) * 86_400 * NANOS_PER_SEC
}

/// Nanoseconds since 1970 of a timestamp.
pub(crate) fn ts_instant(t: Timestamp) -> i128 {
    date_instant(t.date()) + time_nanos(t.time())
}

/// The UTC instant of a timestamp with zone, in nanoseconds since 1970.
pub(crate) fn tz_instant(t: TimestampTz) -> i128 {
    ts_instant(t.local()) - i128::from(t.offset_seconds()) * NANOS_PER_SEC
}

/// Nanoseconds since midnight.
pub(crate) fn time_of_day(t: Time) -> i128 {
    time_nanos(t)
}

// ---- numbers -------------------------------------------------------------------------------

/// A number in sortable form.
enum Num<'a> {
    NegInf,
    Fin(Dec<'a>),
    PosInf,
    Nan,
}

impl Num<'_> {
    fn rank(&self) -> u8 {
        match self {
            Num::NegInf => 0,
            Num::Fin(_) => 1,
            Num::PosInf => 2,
            Num::Nan => 3,
        }
    }
}

/// A finite decimal without redundant zeros; zero is never negative.
struct Dec<'a> {
    neg: bool,
    int: &'a str,
    frac: &'a str,
}

fn parse_num(s: &str) -> Num<'_> {
    match s {
        "inf" => return Num::PosInf,
        "-inf" => return Num::NegInf,
        "NaN" => return Num::Nan,
        _ => {}
    }
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    let zero = int.is_empty() && frac.is_empty();
    Num::Fin(Dec {
        neg: neg && !zero,
        int,
        frac,
    })
}

fn cmp_dec(a: &Dec<'_>, b: &Dec<'_>) -> Ordering {
    if a.neg != b.neg {
        return if a.neg {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    let mag = a
        .int
        .len()
        .cmp(&b.int.len())
        .then_with(|| a.int.cmp(b.int))
        .then_with(|| a.frac.cmp(b.frac));
    if a.neg { mag.reverse() } else { mag }
}

fn cmp_num_text(left: &str, right: &str) -> Ordering {
    let (nl, nr) = (parse_num(left), parse_num(right));
    nl.rank().cmp(&nr.rank()).then_with(|| match (&nl, &nr) {
        (Num::Fin(dl), Num::Fin(dr)) => cmp_dec(dl, dr),
        _ => Ordering::Equal,
    })
}

fn cmp_f64(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

/// A stack buffer for the displayed text of a number (the longest `f64` text is about 330 bytes).
struct TextBuf {
    bytes: [u8; 352],
    len: usize,
}

impl std::fmt::Write for TextBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let end = self.len + s.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl NumSrc<'_> {
    /// Runs `f` on the displayed text of the number, without allocating.
    fn with_text<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        use std::fmt::Write;
        let mut buf = TextBuf {
            bytes: [0; 352],
            len: 0,
        };
        let written = match self {
            NumSrc::Dec(s) => return f(s),
            NumSrc::Int(i) => write!(buf, "{i}"),
            NumSrc::Float(x) => write!(buf, "{x}"),
        };
        match written
            .ok()
            .and_then(|()| std::str::from_utf8(&buf.bytes[..buf.len]).ok())
        {
            Some(text) => f(text),
            None => f(&match self {
                NumSrc::Int(i) => i.to_string(),
                NumSrc::Float(x) => x.to_string(),
                NumSrc::Dec(s) => (*s).to_owned(),
            }),
        }
    }
}

/// Compares two numbers by their exact displayed decimal value: -inf, finite, +inf, NaN.
pub(crate) fn cmp_num(a: NumSrc<'_>, b: NumSrc<'_>) -> Ordering {
    match (a, b) {
        (NumSrc::Int(x), NumSrc::Int(y)) => x.cmp(&y),
        (NumSrc::Float(x), NumSrc::Float(y)) => cmp_f64(x, y),
        (NumSrc::Dec(x), NumSrc::Dec(y)) => cmp_num_text(x, y),
        (x, y) => x.with_text(|l| y.with_text(|r| cmp_num_text(l, r))),
    }
}

// ---- text ----------------------------------------------------------------------------------

/// Compares two texts under `c` by the primary key only: the lowercased code points for
/// `CaseInsensitive` (so `a` equals `A`), the code points for `CodePoint`.
pub(crate) fn cmp_text_primary(a: &str, b: &str, c: Collation) -> Ordering {
    match c {
        Collation::CodePoint => a.cmp(b),
        Collation::CaseInsensitive => {
            if a.is_ascii() && b.is_ascii() {
                a.bytes()
                    .map(|x| x.to_ascii_lowercase())
                    .cmp(b.bytes().map(|x| x.to_ascii_lowercase()))
            } else {
                a.chars()
                    .flat_map(char::to_lowercase)
                    .cmp(b.chars().flat_map(char::to_lowercase))
            }
        }
    }
}

fn cmp_text(a: &str, b: &str, c: Collation) -> Ordering {
    match c {
        Collation::CodePoint => a.cmp(b),
        Collation::CaseInsensitive => cmp_text_primary(a, b, c).then_with(|| a.cmp(b)),
    }
}

// ---- the order -----------------------------------------------------------------------------

/// The total order over cells, ascending. NULL is greater than every value and equal to NULL.
///
/// Classes: boolean < number < date/timestamp < timestamp with zone < time < text (and JSON) <
/// binary < anything else (by its display text). Numbers compare exactly by displayed value
/// (-inf, finite, +inf, NaN last; -0 equals 0). A date is the timestamp at 00:00 and sorts
/// before an equal timestamp; a timestamp with zone compares by its UTC instant, then offset.
#[must_use]
pub fn compare_cells(left: CellRef<'_>, right: CellRef<'_>, coll: Collation) -> Ordering {
    let (nl, nr) = (norm(left), norm(right));
    class(&nl).cmp(&class(&nr)).then_with(|| match (nl, nr) {
        (Norm::Bool(pl), Norm::Bool(pr)) => pl.cmp(&pr),
        (Norm::Num(pl), Norm::Num(pr)) => cmp_num(pl, pr),
        (Norm::Date(pl), Norm::Date(pr)) => pl.cmp(&pr),
        (Norm::Date(pl), Norm::Ts(pr)) => {
            date_instant(pl).cmp(&ts_instant(pr)).then(Ordering::Less)
        }
        (Norm::Ts(pl), Norm::Date(pr)) => ts_instant(pl)
            .cmp(&date_instant(pr))
            .then(Ordering::Greater),
        (Norm::Ts(pl), Norm::Ts(pr)) => pl.cmp(&pr),
        (Norm::Tz(pl), Norm::Tz(pr)) => tz_instant(pl)
            .cmp(&tz_instant(pr))
            .then_with(|| pl.offset_seconds().cmp(&pr.offset_seconds())),
        (Norm::Time(pl), Norm::Time(pr)) => pl.cmp(&pr),
        (Norm::Text(pl), Norm::Text(pr)) => cmp_text(pl, pr, coll),
        (Norm::Bytes(pl), Norm::Bytes(pr)) => pl.cmp(pr),
        (Norm::Other(pl), Norm::Other(pr)) => pl.to_string().cmp(&pr.to_string()),
        _ => Ordering::Equal,
    })
}

// ---- key encoding --------------------------------------------------------------------------

/// The tag of a NULL key: greater than every other tag.
pub(crate) const NULL_TAG: u8 = u8::MAX;

const KIND_INT: u8 = 0;
const KIND_FLOAT: u8 = 1;
const KIND_BOOL: u8 = 2;
const KIND_PREFIX: u8 = 3;
const KIND_INSTANT: u8 = 4;
const KIND_SLOW: u8 = 5;
/// A key that is monotone but not exact: rounded decimals, instants clamped to the `i64` range.
const KIND_APPROX: u8 = 6;

/// Ints up to this size are exact in an `f64`, so they share the float key space.
const EXACT_INT: u64 = 1 << 53;

const SIGN: u64 = 1 << 63;

const fn tag(class: u8, kind: u8) -> u8 {
    class << 3 | kind
}

/// The first 8 bytes of `bytes`, zero padded, big-endian.
fn pack(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut out = [0u8; 8];
    for (slot, b) in out.iter_mut().zip(bytes) {
        *slot = b;
    }
    u64::from_be_bytes(out)
}

#[allow(clippy::cast_sign_loss)] // reinterpreting a signed value with the sign bit flipped
fn flip(i: i64) -> u64 {
    (i as u64) ^ SIGN
}

fn instant_key(class: u8, v: i128) -> (u8, u64) {
    match i64::try_from(v) {
        Ok(n) => (tag(class, KIND_INSTANT), flip(n)),
        // Outside the i64 nanosecond range (before 1677 or after 2262): clamped, ties fall back.
        Err(_) => (tag(class, KIND_APPROX), if v < 0 { 0 } else { u64::MAX }),
    }
}

/// The key of a float: the bits ordered as numbers, NaN last, -0 equal to 0.
fn float_key(x: f64) -> u64 {
    let x = if x.is_nan() {
        f64::NAN
    } else if x == 0.0 {
        0.0
    } else {
        x
    };
    let bits = x.to_bits();
    if bits & SIGN != 0 { !bits } else { bits | SIGN }
}

/// The key of a cell: a tag (`class << 3 | kind`) and 64 bits. Two keys with the same tag and
/// different bits are ordered by the bits; see [`cmp_keys`].
pub(crate) fn encode(c: CellRef<'_>, coll: Collation) -> (u8, u64) {
    match norm(c) {
        Norm::Null => (NULL_TAG, 0),
        Norm::Bool(b) => (tag(CLASS_BOOL, KIND_BOOL), u64::from(b)),
        // Small ints and floats share one key space, so mixed columns need no cell reads.
        #[allow(clippy::cast_precision_loss)] // exact below 2^53
        Norm::Num(NumSrc::Int(i)) if i.unsigned_abs() <= EXACT_INT => {
            (tag(CLASS_NUM, KIND_FLOAT), float_key(i as f64))
        }
        Norm::Num(NumSrc::Int(i)) => (tag(CLASS_NUM, KIND_INT), flip(i)),
        Norm::Num(NumSrc::Float(x)) => (tag(CLASS_NUM, KIND_FLOAT), float_key(x)),
        // The nearest float is monotone in the decimal value; equal keys fall back to the text.
        Norm::Num(NumSrc::Dec(d)) => d
            .parse::<f64>()
            .map_or((tag(CLASS_NUM, KIND_SLOW), 0), |x| {
                (tag(CLASS_NUM, KIND_APPROX), float_key(x))
            }),
        Norm::Date(d) => instant_key(CLASS_TS, date_instant(d)),
        Norm::Ts(t) => instant_key(CLASS_TS, ts_instant(t)),
        Norm::Tz(t) => instant_key(CLASS_TZ, tz_instant(t)),
        Norm::Time(t) => instant_key(CLASS_TIME, time_nanos(t)),
        Norm::Text(s) => {
            let bits = match coll {
                Collation::CodePoint => pack(s.bytes()),
                Collation::CaseInsensitive => pack(folded_bytes(s)),
            };
            (tag(CLASS_TEXT, KIND_PREFIX), bits)
        }
        Norm::Bytes(b) => (tag(CLASS_BYTES, KIND_PREFIX), pack(b.iter().copied())),
        Norm::Other(_) => (tag(CLASS_OTHER, KIND_SLOW), 0),
    }
}

/// The UTF-8 bytes of the lowercased text, lazily.
fn folded_bytes(s: &str) -> impl Iterator<Item = u8> + '_ {
    s.chars().flat_map(char::to_lowercase).flat_map(|c| {
        let mut buf = [0u8; 4];
        let n = c.encode_utf8(&mut buf).len();
        buf.into_iter().take(n)
    })
}

/// What two keys decide: `Some(order)` when the keys are enough, `None` when the cells have to
/// be compared with [`compare_cells`].
pub(crate) fn cmp_keys(ta: u8, ba: u64, tb: u8, bb: u64) -> Option<Ordering> {
    match (ta == NULL_TAG, tb == NULL_TAG) {
        (true, true) => return Some(Ordering::Equal),
        (true, false) => return Some(Ordering::Greater),
        (false, true) => return Some(Ordering::Less),
        (false, false) => {}
    }
    if ta >> 3 != tb >> 3 {
        return Some((ta >> 3).cmp(&(tb >> 3)));
    }
    if ta != tb {
        // Different kinds of one class: keys that are monotone (exact or rounded) still decide
        // when their bits differ.
        let monotone = |t: u8| matches!(t & 7, KIND_FLOAT | KIND_INSTANT | KIND_APPROX);
        return (ta >> 3 == tb >> 3 && monotone(ta) && monotone(tb) && ba != bb)
            .then(|| ba.cmp(&bb));
    }
    if ba != bb {
        return Some(ba.cmp(&bb));
    }
    matches!(ta & 7, KIND_INT | KIND_FLOAT | KIND_BOOL).then_some(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::Decimal;

    use super::*;

    fn cell(v: &Value) -> CellRef<'_> {
        match v {
            Value::Null => CellRef::Null,
            Value::Bool(b) => CellRef::Bool(*b),
            Value::Int(i) => CellRef::Int(*i),
            Value::Float(x) => CellRef::Float(*x),
            Value::Text(s) => CellRef::Text(s),
            Value::Json(s) => CellRef::Json(s),
            Value::Bytes(b) => CellRef::Bytes(b),
            other => CellRef::Value(other),
        }
    }

    fn dec(s: &str) -> Value {
        Value::Decimal(s.parse::<Decimal>().unwrap())
    }

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    fn ts(y: i32, m: u8, d: u8, h: u8, mi: u8) -> Timestamp {
        Timestamp::new(date(y, m, d), Time::new(h, mi, 0, 0).unwrap())
    }

    fn tz(t: Timestamp, off: i32) -> Value {
        Value::TimestampTz(TimestampTz::new(t, off).unwrap())
    }

    fn t(s: &str) -> Value {
        Value::Text(s.to_owned())
    }

    /// About 50 cells covering every variant and the edge cases.
    fn samples() -> Vec<Value> {
        vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Int(i64::MIN),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(1),
            Value::Int(i64::MAX),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(-1.5),
            Value::Float(-0.0),
            Value::Float(0.0),
            Value::Float(1.0),
            Value::Float(1.5),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NAN),
            dec("1"),
            dec("1.0"),
            dec("1.00"),
            dec("-0.00"),
            dec("-12.5"),
            dec("12345678901234567890.123"),
            dec(".5"),
            Value::Date(date(1969, 12, 31)),
            Value::Date(date(2024, 2, 29)),
            Value::Timestamp(ts(2024, 2, 29, 0, 0)),
            Value::Timestamp(ts(2024, 2, 29, 12, 30)),
            Value::Timestamp(ts(1900, 1, 1, 0, 0)),
            tz(ts(2024, 2, 29, 12, 0), 0),
            tz(ts(2024, 2, 29, 14, 0), 7200),
            tz(ts(2024, 2, 29, 10, 0), -7200),
            Value::Time(Time::new(0, 0, 0, 0).unwrap()),
            Value::Time(Time::new(23, 59, 59, 999).unwrap()),
            t(""),
            t("a"),
            t("A"),
            t("ab"),
            t("B"),
            t("ä"),
            t("Ä"),
            t("z"),
            t("a\0"),
            t("abcdefghi1"),
            t("ABCDEFGHI2"),
            Value::Json("{\"a\":1}".into()),
            Value::Bytes(vec![]),
            Value::Bytes(vec![0]),
            Value::Bytes(vec![0, 1]),
            Value::Bytes(vec![255]),
        ]
    }

    const COLS: [Collation; 2] = [Collation::CaseInsensitive, Collation::CodePoint];

    #[test]
    fn total_order_over_all_pairs_and_triples() {
        let vs = samples();
        for c in COLS {
            for a in &vs {
                for b in &vs {
                    let (x, y) = (cell(a), cell(b));
                    let o = compare_cells(x, y, c);
                    assert_eq!(compare_cells(y, x, c), o.reverse(), "{a:?} {b:?}");
                    if a.is_null() != b.is_null() {
                        assert_eq!(
                            o,
                            if a.is_null() {
                                Ordering::Greater
                            } else {
                                Ordering::Less
                            }
                        );
                    }
                    if a == b {
                        assert_eq!(o, Ordering::Equal, "{a:?}");
                    }
                    for d in &vs {
                        let z = cell(d);
                        let (yz, xz) = (compare_cells(y, z, c), compare_cells(x, z, c));
                        if o != Ordering::Greater && yz != Ordering::Greater {
                            assert_ne!(xz, Ordering::Greater, "{a:?} {b:?} {d:?}");
                        }
                        if o == Ordering::Equal {
                            assert_eq!(xz, yz, "{a:?} {b:?} {d:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn classes_are_ordered() {
        let rank = [
            Value::Bool(true),
            Value::Int(-5),
            Value::Timestamp(ts(1900, 1, 1, 0, 0)),
            tz(ts(1900, 1, 1, 0, 0), 0),
            Value::Time(Time::new(0, 0, 0, 0).unwrap()),
            t(""),
            Value::Bytes(vec![]),
            Value::Null,
        ];
        for c in COLS {
            for w in rank.windows(2) {
                assert_eq!(compare_cells(cell(&w[0]), cell(&w[1]), c), Ordering::Less);
            }
        }
    }

    fn cmp(a: &Value, b: &Value) -> Ordering {
        compare_cells(cell(a), cell(b), Collation::CaseInsensitive)
    }

    #[test]
    fn numbers_compare_exactly_by_displayed_value() {
        assert_eq!(cmp(&Value::Int(1), &dec("1.00")), Ordering::Equal);
        assert_eq!(cmp(&dec("1"), &dec("1.0")), Ordering::Equal);
        assert_eq!(cmp(&Value::Float(1.0), &dec("1.00")), Ordering::Equal);
        assert_eq!(cmp(&Value::Float(-0.0), &Value::Int(0)), Ordering::Equal);
        assert_eq!(cmp(&dec("-0.00"), &Value::Int(0)), Ordering::Equal);
        assert_eq!(
            cmp(&Value::Int(i64::MAX), &dec("9223372036854775808")),
            Ordering::Less
        );
        assert_eq!(cmp(&dec("-12.5"), &dec("-2")), Ordering::Less);
        assert_eq!(cmp(&dec("0.5"), &dec(".50")), Ordering::Equal);
        assert_eq!(cmp(&dec("10"), &dec("9.99")), Ordering::Greater);
        assert_eq!(
            cmp(&Value::Float(f64::NEG_INFINITY), &Value::Int(i64::MIN)),
            Ordering::Less
        );
        assert_eq!(
            cmp(&Value::Float(f64::INFINITY), &Value::Int(i64::MAX)),
            Ordering::Greater
        );
        assert_eq!(
            cmp(&Value::Float(f64::NAN), &Value::Float(f64::INFINITY)),
            Ordering::Greater
        );
        assert_eq!(
            cmp(&Value::Float(f64::NAN), &Value::Float(-f64::NAN)),
            Ordering::Equal
        );
        assert_eq!(cmp(&Value::Float(1.5), &dec("1.5")), Ordering::Equal);
    }

    #[test]
    fn temporal_rules() {
        let d = Value::Date(date(2024, 2, 29));
        let midnight = Value::Timestamp(ts(2024, 2, 29, 0, 0));
        assert_eq!(cmp(&d, &midnight), Ordering::Less);
        assert_eq!(cmp(&midnight, &d), Ordering::Greater);
        assert_eq!(
            cmp(&d, &Value::Timestamp(ts(2024, 2, 28, 23, 59))),
            Ordering::Greater
        );
        // Same instant, different offsets: the offset breaks the tie.
        let a = tz(ts(2024, 2, 29, 12, 0), 0);
        let b = tz(ts(2024, 2, 29, 14, 0), 7200);
        let c = tz(ts(2024, 2, 29, 10, 0), -7200);
        assert_eq!(cmp(&c, &a), Ordering::Less);
        assert_eq!(cmp(&a, &b), Ordering::Less);
        assert_eq!(
            cmp(&tz(ts(2024, 2, 29, 12, 1), -7200), &b),
            Ordering::Greater
        );
        // Dates before 1970 and far apart keep their order.
        assert_eq!(
            cmp(
                &Value::Date(date(1969, 12, 31)),
                &Value::Date(date(1970, 1, 1))
            ),
            Ordering::Less
        );
        assert_eq!(days_from_civil(date(1970, 1, 1)), 0);
        assert_eq!(days_from_civil(date(2000, 3, 1)), 11_017);
        assert_eq!(days_from_civil(date(1969, 12, 31)), -1);
    }

    #[test]
    fn text_collation() {
        let ci = Collation::CaseInsensitive;
        let cp = Collation::CodePoint;
        let ord = |a: &str, b: &str, c| compare_cells(CellRef::Text(a), CellRef::Text(b), c);
        // Case-insensitive: folded first, raw as the tie-break.
        assert_eq!(ord("a", "B", ci), Ordering::Less);
        assert_eq!(ord("B", "a", cp), Ordering::Less);
        assert_eq!(ord("a", "A", ci), Ordering::Greater);
        assert_eq!(ord("A", "a", cp), Ordering::Less);
        assert_eq!(ord("ä", "Ä", ci), Ordering::Greater);
        assert_eq!(ord("Ä", "ä", ci), Ordering::Less);
        assert_eq!(ord("z", "ä", ci), Ordering::Less);
        assert_eq!(ord("", "a", ci), Ordering::Less);
        assert_eq!(ord("a", "ab", cp), Ordering::Less);
        assert_eq!(cmp_text_primary("a", "A", ci), Ordering::Equal);
        assert_eq!(cmp_text_primary("ä", "Ä", ci), Ordering::Equal);
        assert_eq!(cmp_text_primary("a", "A", cp), Ordering::Greater);
        // JSON sorts with text.
        assert_eq!(
            compare_cells(CellRef::Json("b"), CellRef::Text("a"), ci),
            Ordering::Greater
        );
    }

    #[test]
    fn bytes_and_other_values() {
        let ci = Collation::CaseInsensitive;
        assert_eq!(
            compare_cells(CellRef::Bytes(&[1]), CellRef::Bytes(&[1, 0]), ci),
            Ordering::Less
        );
        // A text held as a `Value` (stored out of line) sorts with text.
        let v = Value::Text("b".into());
        assert_eq!(
            compare_cells(CellRef::Value(&v), CellRef::Text("a"), ci),
            Ordering::Greater
        );
    }

    #[test]
    fn keys_agree_with_compare_cells() {
        let vs = samples();
        let mut decided = 0usize;
        for c in COLS {
            let keys: Vec<_> = vs.iter().map(|v| encode(cell(v), c)).collect();
            for (i, a) in vs.iter().enumerate() {
                for (j, b) in vs.iter().enumerate() {
                    let (ta, ba) = keys[i];
                    let (tb, bb) = keys[j];
                    if let Some(o) = cmp_keys(ta, ba, tb, bb) {
                        decided += 1;
                        assert_eq!(o, compare_cells(cell(a), cell(b), c), "{a:?} {b:?}");
                    }
                }
            }
        }
        assert!(decided > 2000, "keys decide most pairs: {decided}");
    }

    /// Keys must never contradict the cell order, on numbers that sit next to each other in
    /// different representations: ints around 2^53, floats, long decimals, huge and tiny ones.
    #[test]
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)] // test data
    fn number_keys_agree_with_compare_cells_on_close_values() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut vs: Vec<Value> = Vec::new();
        for _ in 0..120 {
            let r = next();
            let base = ((r >> 3) as i64) >> (r % 11) as u32;
            let i = [
                base,
                (1i64 << 53) + (r % 5) as i64 - 2,
                i64::MAX - (r % 3) as i64,
            ][(r % 3) as usize];
            vs.push(Value::Int(i));
            #[allow(clippy::cast_precision_loss)]
            let f = i as f64;
            vs.push(Value::Float(f));
            vs.push(Value::Float(f64::from_bits(
                f.to_bits().wrapping_add(r % 3),
            )));
            vs.push(dec(&i.to_string()));
            vs.push(dec(&format!("{i}.{}", r % 7)));
            vs.push(dec(&format!("{}.{}", (r % 1000) as i64 - 500, r >> 40)));
            vs.push(dec(&format!("0.{}1", "0".repeat((r % 400) as usize))));
            vs.push(dec(&format!("-{}", "9".repeat((r % 330) as usize + 1))));
        }
        vs.extend([
            Value::Float(f64::MAX),
            Value::Float(f64::MIN_POSITIVE),
            Value::Float(5e-324),
        ]);
        vs.push(dec(&"9".repeat(320)));
        let c = Collation::CaseInsensitive;
        let keys: Vec<_> = vs.iter().map(|v| encode(cell(v), c)).collect();
        let mut decided = 0usize;
        for (i, a) in vs.iter().enumerate() {
            for (j, b) in vs.iter().enumerate() {
                if let Some(o) = cmp_keys(keys[i].0, keys[i].1, keys[j].0, keys[j].1) {
                    decided += 1;
                    assert_eq!(o, compare_cells(cell(a), cell(b), c), "{a:?} {b:?}");
                }
            }
        }
        assert!(decided > vs.len() * vs.len() / 2, "{decided}");
    }

    #[test]
    fn key_corner_cases() {
        let ci = Collation::CaseInsensitive;
        let k = |c: CellRef<'_>| encode(c, ci);
        assert_eq!(k(CellRef::Null).0, NULL_TAG);
        // -0 and NaN are normalised.
        assert_eq!(k(CellRef::Float(-0.0)), k(CellRef::Float(0.0)));
        assert_eq!(k(CellRef::Float(f64::NAN)), k(CellRef::Float(-f64::NAN)));
        // Equal prefixes are not decided.
        let (ta, ba) = k(CellRef::Text("abcdefghX"));
        let (tb, bb) = k(CellRef::Text("ABCDEFGHY"));
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        // A text and its NUL-extended twin share the prefix: not decided either.
        let (ta, ba) = k(CellRef::Text("a"));
        let (tb, bb) = k(CellRef::Text("a\0"));
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        // Small ints and floats share one exact key space.
        let (ta, ba) = k(CellRef::Int(1));
        let (tb, bb) = k(CellRef::Float(2.0));
        assert_eq!(cmp_keys(ta, ba, tb, bb), Some(Ordering::Less));
        let (tb, bb) = k(CellRef::Float(1.0));
        assert_eq!(cmp_keys(ta, ba, tb, bb), Some(Ordering::Equal));
        // Big ints keep their own exact key; against a float the cells decide.
        let (ta, ba) = k(CellRef::Int(i64::MAX));
        let (tb, bb) = k(CellRef::Float(1.0));
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        // A decimal has a rounded key: different keys decide, equal keys do not.
        let d = |s: &str| dec(s);
        let key = |v: &Value| encode(cell(v), ci);
        let ((ta, ba), (tb, bb)) = (key(&d("1.5")), key(&d("12.25")));
        assert_eq!(cmp_keys(ta, ba, tb, bb), Some(Ordering::Less));
        let ((ta, ba), (tb, bb)) = (key(&d("1.5")), key(&Value::Int(2)));
        assert_eq!(cmp_keys(ta, ba, tb, bb), Some(Ordering::Less));
        let ((ta, ba), (tb, bb)) = (
            key(&d("0.10000000000000000000001")),
            key(&d("0.10000000000000000000002")),
        );
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        let ((ta, ba), (tb, bb)) = (key(&d("1.0")), key(&Value::Float(1.0)));
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        // Multi-byte folding fills the prefix without splitting a character.
        let (_, bits) = k(CellRef::Text("ÄÄÄÄÄ"));
        assert_eq!(
            bits.to_be_bytes(),
            [0xc3, 0xa4, 0xc3, 0xa4, 0xc3, 0xa4, 0xc3, 0xa4]
        );
        // Far-away instants do not fit 64 bits of nanoseconds: clamped, equal clamps fall back.
        let far = Value::Date(date(9999, 12, 31));
        let far2 = Value::Date(date(9999, 12, 30));
        assert_eq!(encode(cell(&far), ci).0 & 7, KIND_APPROX);
        let ((ta, ba), (tb, bb)) = (encode(cell(&far), ci), encode(cell(&far2), ci));
        assert_eq!(cmp_keys(ta, ba, tb, bb), None);
        let near2 = Value::Date(date(2025, 1, 1));
        let ((ta, ba), (tb, bb)) = (encode(cell(&near2), ci), encode(cell(&far), ci));
        assert_eq!(cmp_keys(ta, ba, tb, bb), Some(Ordering::Less));
        let near = Value::Date(date(2024, 1, 1));
        assert_eq!(encode(cell(&near), ci).0 & 7, KIND_INSTANT);
    }

    #[test]
    fn click_cycles_the_single_key() {
        let mut s = SortSpec::default();
        assert!(s.is_empty());
        s.click(2, false);
        assert_eq!(s.indicator(2).as_deref(), Some("↑"));
        s.click(2, false);
        assert_eq!(s.indicator(2).as_deref(), Some("↓"));
        s.click(2, false);
        assert!(s.is_empty());
        s.click(2, false);
        assert_eq!(s.keys.len(), 1);
        // Another column replaces the spec.
        s.click(3, false);
        assert_eq!(s.indicator(2), None);
        assert_eq!(s.indicator(3).as_deref(), Some("↑"));
    }

    #[test]
    fn shift_click_builds_a_multi_key_sort() {
        let mut s = SortSpec::default();
        s.click(1, true);
        s.click(4, true);
        assert_eq!(s.indicator(1).as_deref(), Some("↑1"));
        assert_eq!(s.indicator(4).as_deref(), Some("↑2"));
        s.click(1, true);
        assert_eq!(s.indicator(1).as_deref(), Some("↓1"));
        s.click(1, true);
        assert_eq!(s.indicator(1), None);
        assert_eq!(
            s.indicator(4).as_deref(),
            Some("↑"),
            "one key left: no number"
        );
        // A plain click on a column of a multi-key spec resets to that column.
        s.click(7, true);
        s.click(7, false);
        assert_eq!(
            s.keys,
            [SortKey {
                column: 7,
                direction: SortDirection::Ascending
            }]
        );
        // A plain click on a lone key that is not the first of the spec.
        s.click(4, true);
        s.click(4, false);
        assert_eq!(s.keys.len(), 1);
        assert_eq!(s.keys[0].column, 4);
    }

    #[test]
    fn set_only_replaces() {
        let mut s = SortSpec::default();
        s.click(1, true);
        s.click(2, true);
        s.set_only(5, SortDirection::Descending);
        assert_eq!(s.indicator(5).as_deref(), Some("↓"));
        assert_eq!(s.keys.len(), 1);
    }
}
