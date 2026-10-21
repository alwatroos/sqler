// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Randomised checks of the grid sort and filter engine against naive reference implementations
//! written independently of `compare_cells`: the total-order axioms, sort equals a stable
//! reference sort, filters equal a reference, rows arriving in chunks equal a one-shot sort,
//! superseded jobs publish nothing, and the selection follows the model rows. Seeded, so a failure
//! reproduces. `SQLER_PROP_ROWS` raises the row count (default 20,000).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::cast_precision_loss,
    clippy::similar_names
)]

use std::cmp::Ordering;
use std::time::Duration;

use sqler_driver_api::sqler_core::{
    Column, DataType, Date, Decimal, Row, Time, Timestamp, TimestampTz, Value,
};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};
use sqler_ui::grid::{
    CellPos, Collation, ColumnLayout, Condition, DisplayMap, FilterOp, FilterSpec, GridLimits,
    GridModel, GridOrder, OrderProgress, QuickFilter, RowOrder, Selection, SortDirection, SortKey,
    SortSpec, compare_cells, grid_channel_with, ingest,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const WORDS: &[&str] = &[
    "alpha",
    "Alpha",
    "ALPHA",
    "beta",
    "Äpfel",
    "äpfel",
    "ÄPFEL",
    "",
    "z",
    "zz",
    "ß",
    "SS",
    "ss",
    "İstanbul",
    "istanbul",
    "ǅ",
    "ǆ",
    "日本",
    "日本語",
    "😀",
    "a\u{301}",
    "\u{e1}",
    "10",
    "9",
    "-1",
    "1e5",
    "  pad",
    "NULL",
];

const DECIMALS: &[&str] = &[
    "0",
    "-0",
    "0.000",
    "1",
    "1.0",
    "1.00",
    "+1.5",
    "-1.5",
    "5.",
    ".5",
    "-.5",
    "00012",
    "12",
    "123456789012345678901234567890.123456789012345678901234567890",
    "123456789012345678901234567890.123456789012345678901234567891",
    "-99999999999999999999999.9",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "0.1",
    "0.10000000000000000555",
    "9007199254740993",
];

fn float(r: &mut Rng) -> f64 {
    match r.below(14) {
        0 => f64::NAN,
        1 => f64::INFINITY,
        2 => f64::NEG_INFINITY,
        3 => -0.0,
        4 => 0.0,
        5 => 1e300,
        6 => -1e300,
        7 => 1e-300,
        8 => 0.1,
        9 => 9_007_199_254_740_992.0,
        10 => f64::MAX,
        11 => f64::MIN_POSITIVE,
        _ => (r.below(2000) as f64 - 1000.0) / 8.0,
    }
}

fn date(r: &mut Rng) -> Date {
    Date::new(
        1000 + r.below(9000) as i32,
        1 + r.below(12) as u8,
        1 + r.below(28) as u8,
    )
    .unwrap()
}

fn time(r: &mut Rng) -> Time {
    Time::new(
        r.below(24) as u8,
        r.below(60) as u8,
        r.below(60) as u8,
        r.below(3) as u32 * 500,
    )
    .unwrap()
}

fn ts(r: &mut Rng) -> Timestamp {
    Timestamp::new(date(r), time(r))
}

fn tz(r: &mut Rng) -> TimestampTz {
    const OFFSETS: [i32; 6] = [0, 3600, -3600, 19800, -43200, 50400];
    TimestampTz::new(ts(r), *r.pick(&OFFSETS)).unwrap()
}

/// A random cell of any type (NULL included).
fn mixed(r: &mut Rng) -> Value {
    match r.below(16) {
        0 | 1 => Value::Null,
        2 => Value::Bool(r.below(2) == 0),
        3 => Value::Int(*r.pick(&[i64::MIN, i64::MAX, 0, -1, 1, 42, 9_007_199_254_740_993])),
        4 => Value::Int(r.below(100) as i64 - 50),
        5 | 6 => Value::Float(float(r)),
        7 | 8 => Value::Decimal(r.pick(DECIMALS).parse::<Decimal>().unwrap()),
        9 => Value::Date(date(r)),
        10 => Value::Time(time(r)),
        11 => Value::Timestamp(ts(r)),
        12 => Value::TimestampTz(tz(r)),
        13 => Value::Text(format!("{}{}", r.pick(WORDS), r.below(3))),
        14 => Value::Json(format!("{{\"k\":{}}}", r.below(5))),
        _ => Value::Bytes((0..r.below(4)).map(|_| r.below(3) as u8).collect()),
    }
}

fn nullable(r: &mut Rng, v: Value) -> Value {
    if r.below(8) == 0 { Value::Null } else { v }
}

const NCOLS: usize = 8;

/// Columns: mixed, int, text, float, decimal, date, tz, bool.
fn row(r: &mut Rng) -> Vec<Value> {
    let m = mixed(r);
    let i = Value::Int(r.below(40) as i64 - 20);
    let t = Value::Text(format!("{}{}", r.pick(WORDS), r.below(3)));
    let f = Value::Float(float(r));
    let d = Value::Decimal(r.pick(DECIMALS).parse().unwrap());
    let dt = Value::Date(date(r));
    let z = Value::TimestampTz(tz(r));
    let b = Value::Bool(r.below(2) == 0);
    vec![
        m,
        nullable(r, i),
        nullable(r, t),
        nullable(r, f),
        nullable(r, d),
        nullable(r, dt),
        nullable(r, z),
        nullable(r, b),
    ]
}

fn rows_of(n: usize, seed: u64) -> Vec<Vec<Value>> {
    let mut r = Rng(seed);
    (0..n).map(|_| row(&mut r)).collect()
}

struct Source {
    events: std::vec::IntoIter<QueryEvent>,
}

impl QueryStream for Source {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let ev = self.events.next().map(Ok);
        Box::pin(std::future::ready(ev))
    }
}

/// Ingests `rows` in batches of `batch` into an unbounded model; the model is polled `polled`
/// messages deep (all when `None`): columns first, then one message per batch.
fn load(rows: &[Vec<Value>], batch: usize, polled: Option<usize>) -> GridModel {
    let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
    let mut events = vec![QueryEvent::Columns(
        (0..NCOLS)
            .map(|i| Column::new(format!("c{i}"), DataType::Text))
            .collect(),
    )];
    for chunk in rows.chunks(batch.max(1)) {
        events.push(QueryEvent::Rows(chunk.iter().cloned().map(Row).collect()));
    }
    events.push(QueryEvent::Done {
        rows_affected: None,
    });
    let mut src = Source {
        events: events.into_iter(),
    };
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(ingest(&mut src, sink))
        .unwrap();
    model.poll(polled.unwrap_or(usize::MAX));
    model
}

fn n_rows() -> usize {
    std::env::var("SQLER_PROP_ROWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000)
}

// ---------------------------------------------------------------- the reference order

/// A decimal number as (negative, integer digits, fraction digits), normalised; `None` for NaN.
#[derive(Debug, PartialEq, Eq, Clone)]
enum Num {
    NegInf,
    Fin(bool, String, String),
    PosInf,
    Nan,
}

fn num_of_text(s: &str) -> Num {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (i, f) = body.split_once('.').unwrap_or((body, ""));
    let i = i.trim_start_matches('0').to_owned();
    let f = f.trim_end_matches('0').to_owned();
    let zero = i.is_empty() && f.is_empty();
    Num::Fin(neg && !zero, i, f)
}

fn num_of(v: &Value) -> Num {
    match v {
        Value::Int(i) => num_of_text(&i.to_string()),
        Value::Decimal(d) => num_of_text(d.as_str()),
        Value::Float(x) if x.is_nan() => Num::Nan,
        Value::Float(x) if x.is_infinite() => {
            if *x > 0.0 {
                Num::PosInf
            } else {
                Num::NegInf
            }
        }
        Value::Float(x) => num_of_text(&format!("{x}")),
        _ => unreachable!(),
    }
}

fn mag(a: &(String, String), b: &(String, String)) -> Ordering {
    a.0.len()
        .cmp(&b.0.len())
        .then_with(|| a.0.cmp(&b.0))
        .then_with(|| a.1.cmp(&b.1))
}

fn cmp_num(a: &Num, b: &Num) -> Ordering {
    let rank = |n: &Num| match n {
        Num::NegInf => 0,
        Num::Fin(..) => 1,
        Num::PosInf => 2,
        Num::Nan => 3,
    };
    match (a, b) {
        (Num::Fin(an, ai, af), Num::Fin(bn, bi, bf)) => {
            let m = mag(&(ai.clone(), af.clone()), &(bi.clone(), bf.clone()));
            match (an, bn) {
                (false, true) => Ordering::Greater,
                (true, false) => Ordering::Less,
                (false, false) => m,
                (true, true) => m.reverse(),
            }
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn tod(t: Time) -> i128 {
    ((i128::from(t.hour()) * 60 + i128::from(t.minute())) * 60 + i128::from(t.second()))
        * 1_000_000_000
        + i128::from(t.nanosecond())
}

fn dnanos(d: Date) -> i128 {
    i128::from(days(
        i64::from(d.year()),
        i64::from(d.month()),
        i64::from(d.day()),
    )) * 86_400
        * 1_000_000_000
}

fn tsnanos(t: Timestamp) -> i128 {
    dnanos(t.date()) + tod(t.time())
}

fn text_cmp(a: &str, b: &str, c: Collation) -> Ordering {
    match c {
        Collation::CodePoint => a.cmp(b),
        Collation::CaseInsensitive => a
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(b.chars().flat_map(char::to_lowercase))
            .then_with(|| a.cmp(b)),
    }
}

fn class(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => 0,
        Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 1,
        Value::Date(_) | Value::Timestamp(_) => 2,
        Value::TimestampTz(_) => 3,
        Value::Time(_) => 4,
        Value::Text(_) | Value::Json(_) => 5,
        Value::Bytes(_) => 6,
        _ => 7,
    }
}

fn txt(v: &Value) -> &str {
    match v {
        Value::Text(s) | Value::Json(s) => s,
        _ => unreachable!(),
    }
}

/// Ascending order of two non-NULL values.
fn ocmp(a: &Value, b: &Value, c: Collation) -> Ordering {
    class(a).cmp(&class(b)).then_with(|| match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Date(_) | Value::Timestamp(_), Value::Date(_) | Value::Timestamp(_)) => {
            let n = |v: &Value| match v {
                Value::Date(d) => (dnanos(*d), 0),
                Value::Timestamp(t) => (tsnanos(*t), 1),
                _ => unreachable!(),
            };
            n(a).cmp(&n(b))
        }
        (Value::TimestampTz(x), Value::TimestampTz(y)) => {
            let n = |t: &TimestampTz| {
                (
                    tsnanos(t.local()) - i128::from(t.offset_seconds()) * 1_000_000_000,
                    t.offset_seconds(),
                )
            };
            n(x).cmp(&n(y))
        }
        (Value::Time(x), Value::Time(y)) => tod(*x).cmp(&tod(*y)),
        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Text(_) | Value::Json(_), _) => text_cmp(txt(a), txt(b), c),
        _ => cmp_num(&num_of(a), &num_of(b)),
    })
}

/// Stable reference sort of the rows (NULL last in both directions).
fn ref_sort(rows: &[Vec<Value>], idx: Vec<usize>, keys: &[SortKey], c: Collation) -> Vec<usize> {
    let mut idx = idx;
    idx.sort_by(|&a, &b| {
        for k in keys {
            let (x, y) = (&rows[a][k.column], &rows[b][k.column]);
            let o = match (x.is_null(), y.is_null()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                _ => {
                    let o = ocmp(x, y, c);
                    if k.direction == SortDirection::Descending {
                        o.reverse()
                    } else {
                        o
                    }
                }
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    idx
}

fn shown(order: &GridOrder, model: &GridModel) -> Vec<usize> {
    let rows = order.rows(model.row_count());
    (0..rows.len())
        .map(|d| rows.model_row(d).unwrap())
        .collect()
}

fn finish(order: &mut GridOrder, model: &GridModel) {
    // Idle is right when the spec did not change (set_* of an equal spec starts no job).
    let p = order.step(model, None);
    assert!(
        matches!(p, OrderProgress::Published | OrderProgress::Idle),
        "{p:?}"
    );
    assert!(!order.is_working());
}

fn random_keys(r: &mut Rng) -> Vec<SortKey> {
    let n = 1 + r.below(4);
    let mut keys: Vec<SortKey> = Vec::new();
    while keys.len() < n {
        let column = r.below(NCOLS);
        if keys.iter().any(|k| k.column == column) {
            continue;
        }
        let direction = if r.below(2) == 0 {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };
        keys.push(SortKey { column, direction });
    }
    keys
}

fn coll(r: &mut Rng) -> Collation {
    if r.below(2) == 0 {
        Collation::CaseInsensitive
    } else {
        Collation::CodePoint
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn compare_cells_is_a_total_order_and_matches_the_reference() {
    let mut r = Rng(0x9e37_79b9);
    let n = 150;
    let rows: Vec<Vec<Value>> = (0..n).map(|_| vec![mixed(&mut r); NCOLS]).collect();
    let model = load(&rows, 64, None);
    for c in [Collation::CaseInsensitive, Collation::CodePoint] {
        let cells: Vec<_> = (0..n)
            .map(|i| model.row(i).unwrap().get(0).unwrap())
            .collect();
        for (i, &a) in cells.iter().enumerate() {
            assert_eq!(
                compare_cells(a, a, c),
                Ordering::Equal,
                "reflexive {:?}",
                rows[i][0]
            );
            for (j, &b) in cells.iter().enumerate() {
                let ab = compare_cells(a, b, c);
                assert_eq!(
                    ab,
                    compare_cells(b, a, c).reverse(),
                    "antisymmetric {:?} {:?}",
                    rows[i][0],
                    rows[j][0]
                );
                if !rows[i][0].is_null() && !rows[j][0].is_null() {
                    assert_eq!(
                        ab,
                        ocmp(&rows[i][0], &rows[j][0], c),
                        "vs reference {:?} {:?}",
                        rows[i][0],
                        rows[j][0]
                    );
                }
                for &z in &cells {
                    let (bz, az) = (compare_cells(b, z, c), compare_cells(a, z, c));
                    if ab != Ordering::Greater && bz != Ordering::Greater {
                        assert_ne!(
                            az,
                            Ordering::Greater,
                            "transitive {:?} {:?}",
                            rows[i][0],
                            rows[j][0]
                        );
                    }
                    if ab == Ordering::Equal {
                        assert_eq!(az, bz, "congruent");
                    }
                }
            }
        }
    }
}

#[test]
fn sorted_rows_equal_the_reference_for_one_to_four_keys() {
    let n = n_rows();
    let rows = rows_of(n, 7);
    let model = load(&rows, 1000, None);
    let mut r = Rng(11);
    for round in 0..12 {
        let keys = random_keys(&mut r);
        let c = coll(&mut r);
        let mut order = GridOrder::default();
        order.set_collation(c);
        order.set_sort(SortSpec { keys: keys.clone() });
        if round % 3 == 0 {
            // Sliced to the smallest budget: same result.
            let mut guard = 0;
            while order.step(&model, Some(Duration::ZERO)) != OrderProgress::Published {
                guard += 1;
                assert!(guard < 5_000_000);
            }
        } else {
            finish(&mut order, &model);
        }
        let want = ref_sort(&rows, (0..n).collect(), &keys, c);
        assert_eq!(shown(&order, &model), want, "keys {keys:?} {c:?}");
    }
}

#[test]
fn equal_keys_keep_model_order_in_both_directions() {
    let rows = rows_of(5000, 3);
    let model = load(&rows, 700, None);
    for d in [SortDirection::Ascending, SortDirection::Descending] {
        let mut order = GridOrder::default();
        order.set_sort(SortSpec {
            keys: vec![SortKey {
                column: 1,
                direction: d,
            }],
        });
        finish(&mut order, &model);
        let got = shown(&order, &model);
        for w in got.windows(2) {
            if rows[w[0]][1] == rows[w[1]][1] {
                assert!(w[0] < w[1], "unstable {w:?}");
            }
        }
    }
}

fn random_filter(r: &mut Rng) -> FilterSpec {
    let mut spec = FilterSpec::default();
    if r.below(2) == 0 {
        spec.quick = Some(QuickFilter {
            text: r
                .pick(&[
                    "a", "A", "Ä", "ss", "1", "-", "20", "", "😀", "日", "i̇", "NaN", "e",
                ])
                .to_string(),
            column: if r.below(2) == 0 {
                None
            } else {
                Some(r.below(NCOLS))
            },
        });
    }
    for _ in 0..r.below(3) {
        let (column, op, operand) = match r.below(6) {
            0 => (
                1,
                *r.pick(&[
                    FilterOp::Equals,
                    FilterOp::NotEquals,
                    FilterOp::Greater,
                    FilterOp::Less,
                ]),
                r.pick(&["0", "-5", " 7 ", "7.0", "x", "", "99999999999999999999"])
                    .to_string(),
            ),
            1 => (
                3,
                *r.pick(&[
                    FilterOp::Equals,
                    FilterOp::NotEquals,
                    FilterOp::Greater,
                    FilterOp::Less,
                ]),
                r.pick(&[
                    "0",
                    "0.0",
                    "-0",
                    "1e2",
                    "125",
                    "-7.5",
                    "NaN",
                    "0.1",
                    "1000000000000000000000",
                ])
                .to_string(),
            ),
            2 => (
                4,
                *r.pick(&[
                    FilterOp::Equals,
                    FilterOp::NotEquals,
                    FilterOp::Greater,
                    FilterOp::Less,
                ]),
                r.pick(DECIMALS).to_string(),
            ),
            3 => (
                2,
                *r.pick(&[
                    FilterOp::Equals,
                    FilterOp::NotEquals,
                    FilterOp::Greater,
                    FilterOp::Less,
                ]),
                r.pick(WORDS).to_string(),
            ),
            4 => (
                r.below(NCOLS),
                *r.pick(&[FilterOp::IsNull, FilterOp::NotNull]),
                String::new(),
            ),
            _ => (
                r.below(NCOLS),
                FilterOp::Contains,
                r.pick(&["a", "ä", "1", ".", "0", "T", ":", "+"])
                    .to_string(),
            ),
        };
        spec.conditions.push(Condition {
            column,
            op,
            operand,
        });
    }
    spec
}

fn display(v: &Value) -> String {
    v.to_string()
}

fn ref_matches(row: &[Value], f: &FilterSpec, c: Collation) -> bool {
    let fold = |s: &str| -> String {
        if c == Collation::CaseInsensitive {
            s.chars().flat_map(char::to_lowercase).collect()
        } else {
            s.to_owned()
        }
    };
    let contains =
        |v: &Value, needle: &str| !v.is_null() && fold(&display(v)).contains(&fold(needle));
    if let Some(q) = f.quick.as_ref().filter(|q| !q.text.is_empty()) {
        {
            let hit = match q.column {
                Some(col) => contains(&row[col], &q.text),
                None => row.iter().any(|v| contains(v, &q.text)),
            };
            if !hit {
                return false;
            }
        }
    }
    f.conditions.iter().all(|cond| {
        let v = &row[cond.column];
        match cond.op {
            FilterOp::IsNull => return v.is_null(),
            FilterOp::NotNull => return !v.is_null(),
            _ => {}
        }
        if v.is_null() {
            return false;
        }
        if cond.op == FilterOp::Contains {
            return contains(v, cond.operand.trim());
        }
        let operand = cond.operand.trim();
        let ord = match v {
            Value::Int(_) | Value::Float(_) | Value::Decimal(_) => {
                if operand.parse::<Decimal>().is_ok() {
                    cmp_num(&num_of(v), &num_of_text(operand))
                } else {
                    text_cmp_primary(&display(v), operand, c)
                }
            }
            _ => text_cmp_primary(&display(v), operand, c),
        };
        match cond.op {
            FilterOp::Equals => ord == Ordering::Equal,
            FilterOp::NotEquals => ord != Ordering::Equal,
            FilterOp::Greater => ord == Ordering::Greater,
            _ => ord == Ordering::Less,
        }
    })
}

fn text_cmp_primary(a: &str, b: &str, c: Collation) -> Ordering {
    match c {
        Collation::CodePoint => a.cmp(b),
        Collation::CaseInsensitive => a
            .chars()
            .flat_map(char::to_lowercase)
            .cmp(b.chars().flat_map(char::to_lowercase)),
    }
}

#[test]
fn filters_equal_the_reference_for_every_operator() {
    let n = n_rows().min(20_000);
    let rows = rows_of(n, 21);
    let model = load(&rows, 512, None);
    let mut r = Rng(5);
    let mut kept_some = 0;
    for round in 0..80 {
        let spec = random_filter(&mut r);
        let c = coll(&mut r);
        let mut order = GridOrder::default();
        order.set_collation(c);
        order.set_filter(spec.clone());
        if spec.is_empty() {
            assert!(!order.is_active());
            continue;
        }
        finish(&mut order, &model);
        let want: Vec<usize> = (0..n)
            .filter(|&i| ref_matches(&rows[i], &spec, c))
            .collect();
        kept_some += usize::from(!want.is_empty() && want.len() < n);
        assert_eq!(shown(&order, &model), want, "round {round} {spec:?} {c:?}");
        // Filter and sort together.
        let keys = random_keys(&mut r);
        order.set_sort(SortSpec { keys: keys.clone() });
        finish(&mut order, &model);
        assert_eq!(
            shown(&order, &model),
            ref_sort(&rows, want, &keys, c),
            "{spec:?} {keys:?}"
        );
    }
    assert!(
        kept_some > 10,
        "the generated filters were too trivial: {kept_some}"
    );
}

#[test]
fn rows_arriving_in_chunks_merge_into_the_one_shot_result() {
    let n = 12_000;
    let rows = rows_of(n, 99);
    let mut r = Rng(13);
    for round in 0..6 {
        let keys = random_keys(&mut r);
        let c = coll(&mut r);
        let filter = if round % 2 == 0 {
            random_filter(&mut r)
        } else {
            FilterSpec::default()
        };
        let batch = [1, 7, 500, 3000][round % 4];
        let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        let mut events = vec![QueryEvent::Columns(
            (0..NCOLS)
                .map(|i| Column::new(format!("c{i}"), DataType::Text))
                .collect(),
        )];
        for chunk in rows.chunks(batch) {
            events.push(QueryEvent::Rows(chunk.iter().cloned().map(Row).collect()));
        }
        events.push(QueryEvent::Done {
            rows_affected: None,
        });
        let mut src = Source {
            events: events.into_iter(),
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(ingest(&mut src, sink))
            .unwrap();
        let mut order = GridOrder::default();
        order.set_collation(c);
        order.set_sort(SortSpec { keys: keys.clone() });
        order.set_filter(filter.clone());
        // Deliver a few messages at a time and step in small slices between them.
        let mut guard = 0;
        while !matches!(model.state(), sqler_ui::grid::IngestState::Done { .. }) {
            model.poll(1 + guard % 3);
            order.step(&model, Some(Duration::from_micros(50)));
            guard += 1;
            assert!(guard < 1_000_000);
        }
        let mut guard = 0;
        while order.is_working() || order.covered() < model.row_count() {
            order.step(&model, Some(Duration::from_millis(1)));
            guard += 1;
            assert!(guard < 1_000_000);
        }
        let want = ref_sort(
            &rows,
            (0..n)
                .filter(|&i| ref_matches(&rows[i], &filter, c))
                .collect(),
            &keys,
            c,
        );
        assert_eq!(
            shown(&order, &model),
            want,
            "batch {batch} {keys:?} {filter:?}"
        );
        let mut one = GridOrder::default();
        one.set_collation(c);
        one.set_sort(SortSpec { keys });
        one.set_filter(filter);
        finish(&mut one, &model);
        assert_eq!(shown(&one, &model), shown(&order, &model));
    }
}

#[test]
fn a_superseded_job_never_publishes_and_the_old_order_stays_until_the_new_one() {
    let n = 30_000;
    let rows = rows_of(n, 5);
    let model = load(&rows, 4000, None);
    let asc = |c| SortSpec {
        keys: vec![SortKey {
            column: c,
            direction: SortDirection::Ascending,
        }],
    };
    let mut order = GridOrder::default();
    order.set_sort(asc(1));
    finish(&mut order, &model);
    let first = shown(&order, &model);
    // Start a job, run part of it, supersede it repeatedly.
    for col in [2, 3, 4, 0] {
        order.set_sort(asc(col));
        for _ in 0..3 {
            assert!(matches!(
                order.step(&model, Some(Duration::ZERO)),
                OrderProgress::Working { .. }
            ));
        }
        assert_eq!(shown(&order, &model), first, "stale partial publish");
    }
    // Supersede with a spec that equals the published one is a different job again; clear mid-job.
    order.set_sort(asc(5));
    order.step(&model, Some(Duration::ZERO));
    order.set_sort(SortSpec::default());
    assert!(!order.is_working() && !order.is_active());
    assert_eq!(shown(&order, &model), (0..n).collect::<Vec<_>>());
    assert_eq!(order.step(&model, None), OrderProgress::Idle);
    // Finally one job to the end is exactly the last spec.
    order.set_sort(asc(5));
    order.step(&model, Some(Duration::ZERO));
    order.set_sort(asc(2));
    finish(&mut order, &model);
    assert_eq!(
        shown(&order, &model),
        ref_sort(
            &rows,
            (0..n).collect(),
            &asc(2).keys,
            Collation::CaseInsensitive
        )
    );
}

#[test]
fn the_selection_keeps_pointing_at_the_same_model_row() {
    let n = 3000;
    let rows = rows_of(n, 77);
    let model = load(&rows, 1000, None);
    let layout = ColumnLayout::new(vec![80.0; NCOLS]);
    let mut r = Rng(31);
    let mut order = GridOrder::default();
    let mut sel = Selection::default();
    let target = CellPos { row: 1234, col: 2 };
    sel.click(target, false);
    for _ in 0..20 {
        match r.below(4) {
            0 => order.set_sort(SortSpec {
                keys: random_keys(&mut r),
            }),
            1 => order.set_filter(random_filter(&mut r)),
            2 => order.set_sort(SortSpec::default()),
            _ => order.set_filter(FilterSpec::default()),
        }
        finish(&mut order, &model);
        assert_eq!(sel.active(), Some(target));
        let rows_view = order.rows(model.row_count());
        let map = DisplayMap {
            rows: &rows_view,
            columns: &layout,
        };
        match rows_view.display_row(target.row) {
            Some(d) => {
                assert_eq!(rows_view.model_row(d), Some(target.row));
                assert_eq!(map.cell_at(d, 2), Some(target));
                assert!(sel.contains(d, 2, &map));
                // No other display row is selected.
                for other in [
                    0,
                    d.saturating_sub(1),
                    (d + 1).min(rows_view.len().saturating_sub(1)),
                ] {
                    if other != d {
                        assert!(!sel.contains(other, 2, &map));
                    }
                }
            }
            None => assert!(
                !(0..rows_view.len()).any(|d| sel.contains(d, 2, &map)),
                "hidden row selected"
            ),
        }
        // Round trip for every shown row.
        for d in (0..rows_view.len()).step_by(37) {
            assert_eq!(
                rows_view.display_row(rows_view.model_row(d).unwrap()),
                Some(d)
            );
        }
    }
    // Clearing both returns to the identity and the selected cell shows at its model position.
    order.reset();
    let v = order.rows(model.row_count());
    assert_eq!(v.display_row(target.row), Some(target.row));
}
