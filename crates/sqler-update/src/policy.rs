// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! When to check: pure functions, the clock is passed in.

use std::time::{Duration, SystemTime};

/// The timing of background update checks.
///
/// The caller owns the opt-in setting and must not check while it is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckSchedule {
    /// Minimum wait after the first frame before the first check.
    pub startup_delay: Duration,
    /// Wait between successful checks.
    pub interval: Duration,
    /// Wait after the first failure; doubled for each further failure.
    pub first_retry: Duration,
    /// Upper bound of the wait after failures.
    pub max_backoff: Duration,
}

impl Default for CheckSchedule {
    fn default() -> Self {
        Self {
            startup_delay: Duration::from_secs(30),
            interval: Duration::from_hours(24),
            first_retry: Duration::from_hours(1),
            max_backoff: Duration::from_hours(24),
        }
    }
}

impl CheckSchedule {
    /// The wait before the next attempt: `interval` after a success, else
    /// `min(first_retry * 2^(n-1), max_backoff)` for `n` failures in a row.
    #[must_use]
    pub fn delay_after(&self, consecutive_failures: u32) -> Duration {
        if consecutive_failures == 0 {
            return self.interval;
        }
        let shift = (consecutive_failures - 1).min(32);
        let factor = 1u32 << shift.min(31);
        self.first_retry
            .saturating_mul(factor)
            .min(self.max_backoff)
    }

    /// True when a check should run now. Always true without a previous
    /// attempt or when the clock moved back before it.
    #[must_use]
    pub fn is_due(
        &self,
        now: SystemTime,
        last_attempt: Option<SystemTime>,
        consecutive_failures: u32,
    ) -> bool {
        let Some(last) = last_attempt else {
            return true;
        };
        match now.duration_since(last) {
            Err(_) => true,
            Ok(elapsed) => elapsed >= self.delay_after(consecutive_failures),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn delays() {
        let s = CheckSchedule::default();
        assert_eq!(s.delay_after(0), 24 * HOUR);
        assert_eq!(s.delay_after(1), HOUR);
        assert_eq!(s.delay_after(2), 2 * HOUR);
        assert_eq!(s.delay_after(3), 4 * HOUR);
        assert_eq!(s.delay_after(6), 24 * HOUR);
        assert_eq!(s.delay_after(u32::MAX), 24 * HOUR);
    }

    #[test]
    fn saturates_without_a_cap() {
        let s = CheckSchedule {
            max_backoff: Duration::MAX,
            first_retry: Duration::MAX,
            ..CheckSchedule::default()
        };
        assert_eq!(s.delay_after(u32::MAX), Duration::MAX);
    }

    #[test]
    fn due_rules() {
        let s = CheckSchedule::default();
        let last = SystemTime::UNIX_EPOCH + 1000 * HOUR;
        assert!(s.is_due(last, None, 0));
        assert!(!s.is_due(last, Some(last), 0));
        assert!(!s.is_due(last + 24 * HOUR - Duration::from_secs(1), Some(last), 0));
        assert!(s.is_due(last + 24 * HOUR, Some(last), 0));
        assert!(!s.is_due(last + HOUR - Duration::from_secs(1), Some(last), 1));
        assert!(s.is_due(last + HOUR, Some(last), 1));
        // The clock moved back: the last attempt is in the future.
        assert!(s.is_due(last - HOUR, Some(last), 0));
    }
}
