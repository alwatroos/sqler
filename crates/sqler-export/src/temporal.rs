// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Civil-day and epoch arithmetic shared by the `Parquet` and `XLSX` writers.

use sqler_core::{Date, Time, Timestamp, TimestampTz};

const NANOS_PER_SECOND: i64 = 1_000_000_000;
const MICROS_PER_DAY: i64 = 86_400 * 1_000_000;

/// Days since 1970-01-01 in the proleptic Gregorian calendar (negative before).
pub(crate) fn days_from_civil(d: Date) -> i64 {
    let y = i64::from(d.year()) - i64::from(d.month() <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(d.month());
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d.day()) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Nanoseconds since midnight.
pub(crate) fn nanos_of_day(t: Time) -> i64 {
    (i64::from(t.hour()) * 3600 + i64::from(t.minute()) * 60 + i64::from(t.second()))
        * NANOS_PER_SECOND
        + i64::from(t.nanosecond())
}

/// Microseconds since the epoch of the wall clock (as if UTC); `None` on `i64` overflow.
/// Digits below a microsecond are dropped.
pub(crate) fn local_micros(ts: Timestamp) -> Option<i64> {
    days_from_civil(ts.date())
        .checked_mul(MICROS_PER_DAY)?
        .checked_add(nanos_of_day(ts.time()) / 1000)
}

/// Microseconds since the epoch of the instant: the wall clock minus the offset.
pub(crate) fn utc_micros(ts: TimestampTz) -> Option<i64> {
    local_micros(ts.local())?.checked_sub(i64::from(ts.offset_seconds()).checked_mul(1_000_000)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::new(y, m, d).unwrap()
    }

    #[test]
    fn known_days() {
        assert_eq!(days_from_civil(date(1970, 1, 1)), 0);
        assert_eq!(days_from_civil(date(1970, 1, 2)), 1);
        assert_eq!(days_from_civil(date(1969, 12, 31)), -1);
        assert_eq!(days_from_civil(date(2000, 3, 1)), 11_017);
        assert_eq!(days_from_civil(date(2024, 2, 29)), 19_782);
        assert_eq!(days_from_civil(date(2024, 3, 1)), 19_783);
        assert_eq!(days_from_civil(date(1900, 3, 1)), -25_508);
        assert_eq!(days_from_civil(date(9999, 12, 31)), 2_932_896);
    }

    #[test]
    fn leap_years() {
        // 1900 is not a leap year, 2000 is.
        assert_eq!(
            days_from_civil(date(1900, 3, 1)) - days_from_civil(date(1900, 2, 28)),
            1
        );
        assert_eq!(
            days_from_civil(date(2000, 3, 1)) - days_from_civil(date(2000, 2, 28)),
            2
        );
    }

    #[test]
    fn year_one_and_negative_years() {
        assert_eq!(days_from_civil(date(1, 1, 1)), -719_162);
        assert_eq!(days_from_civil(date(0, 1, 1)), -719_528);
        assert_eq!(days_from_civil(date(-1, 1, 1)), -719_893);
        assert_eq!(
            days_from_civil(date(-4, 2, 29)) + 1,
            days_from_civil(date(-4, 3, 1))
        );
    }

    #[test]
    fn nanos_and_micros() {
        let t = Time::new(1, 2, 3, 456_789_012).unwrap();
        assert_eq!(nanos_of_day(t), 3_723_456_789_012);
        let ts = Timestamp::new(date(1970, 1, 2), t);
        assert_eq!(local_micros(ts), Some(86_400_000_000 + 3_723_456_789));
        let before = Timestamp::new(date(1969, 12, 31), Time::new(23, 59, 59, 0).unwrap());
        assert_eq!(local_micros(before), Some(-1_000_000));
    }

    #[test]
    fn utc_subtracts_the_offset() {
        let ts = Timestamp::new(date(2024, 1, 1), Time::new(5, 30, 0, 0).unwrap());
        let tz = TimestampTz::new(ts, 5 * 3600 + 30 * 60).unwrap();
        let utc = Timestamp::new(date(2024, 1, 1), Time::new(0, 0, 0, 0).unwrap());
        assert_eq!(utc_micros(tz), local_micros(utc));
    }

    #[test]
    fn overflow_is_none() {
        let far = Timestamp::new(date(i32::MAX, 12, 31), Time::new(0, 0, 0, 0).unwrap());
        assert_eq!(local_micros(far), None);
        let near = Timestamp::new(date(-290_000, 1, 1), Time::new(0, 0, 0, 0).unwrap());
        assert!(local_micros(near).is_some());
    }
}
