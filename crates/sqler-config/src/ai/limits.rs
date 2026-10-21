// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Row and time caps for AI-issued queries.

use std::ops::RangeInclusive;
use std::time::Duration;

use sqler_core::Environment;

use super::error::AiConfigProblem;

/// Default row cap for AI-issued queries.
pub const DEFAULT_AI_MAX_ROWS: u32 = 1_000;
/// Allowed values of `max_rows`.
pub const AI_MAX_ROWS_RANGE: RangeInclusive<u32> = 1..=1_000_000;
/// Default time cap, in seconds, for AI-issued queries.
pub const DEFAULT_AI_TIMEOUT_SECS: u32 = 30;
/// Allowed values of `timeout_secs`.
pub const AI_TIMEOUT_SECS_RANGE: RangeInclusive<u32> = 1..=3_600;

/// Checks `value` against `range`.
pub(crate) fn check_limit(
    key: &'static str,
    value: i64,
    range: &RangeInclusive<u32>,
) -> Result<u32, AiConfigProblem> {
    match u32::try_from(value) {
        Ok(v) if range.contains(&v) => Ok(v),
        _ => Err(AiConfigProblem::LimitOutOfRange {
            key,
            value,
            min: *range.start(),
            max: *range.end(),
        }),
    }
}

/// The `[limits]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AiLimits {
    /// Row cap, within [`AI_MAX_ROWS_RANGE`].
    pub max_rows: u32,
    /// Time cap in seconds, within [`AI_TIMEOUT_SECS_RANGE`].
    pub timeout_secs: u32,
    /// Lift both caps. Ignored for `prod` connections.
    pub unlimited: bool,
}

impl Default for AiLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_AI_MAX_ROWS,
            timeout_secs: DEFAULT_AI_TIMEOUT_SECS,
            unlimited: false,
        }
    }
}

impl AiLimits {
    /// Checks both values against their ranges.
    ///
    /// # Errors
    /// [`AiConfigProblem::LimitOutOfRange`].
    pub fn validate(&self) -> Result<(), AiConfigProblem> {
        check_limit("max_rows", i64::from(self.max_rows), &AI_MAX_ROWS_RANGE)?;
        check_limit(
            "timeout_secs",
            i64::from(self.timeout_secs),
            &AI_TIMEOUT_SECS_RANGE,
        )?;
        Ok(())
    }

    /// The caps that apply on a connection of environment `env`. On `prod` they always apply.
    #[must_use]
    pub fn effective(&self, env: Option<Environment>) -> QueryCaps {
        if self.unlimited && env != Some(Environment::Prod) {
            QueryCaps {
                max_rows: None,
                timeout: None,
            }
        } else {
            QueryCaps {
                max_rows: Some(self.max_rows),
                timeout: Some(Duration::from_secs(u64::from(self.timeout_secs))),
            }
        }
    }
}

/// The caps for one connection; `None` means no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct QueryCaps {
    /// Most rows an AI-issued query may return.
    pub max_rows: Option<u32>,
    /// Longest an AI-issued query may run.
    pub timeout: Option<Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cap_everything() {
        let caps = AiLimits::default().effective(None);
        assert_eq!(caps.max_rows, Some(1_000));
        assert_eq!(caps.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn unlimited_lifts_caps_except_on_prod() {
        let l = AiLimits {
            unlimited: true,
            ..AiLimits::default()
        };
        for env in [None, Some(Environment::Dev), Some(Environment::Staging)] {
            let caps = l.effective(env);
            assert_eq!((caps.max_rows, caps.timeout), (None, None));
        }
        let caps = l.effective(Some(Environment::Prod));
        assert_eq!(caps.max_rows, Some(1_000));
        assert_eq!(caps.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn range_check() {
        assert!(AiLimits::default().validate().is_ok());
        for (rows, secs, key) in [
            (0, 30, "max_rows"),
            (1_000_001, 30, "max_rows"),
            (10, 0, "timeout_secs"),
            (10, 3_601, "timeout_secs"),
        ] {
            let l = AiLimits {
                max_rows: rows,
                timeout_secs: secs,
                unlimited: false,
            };
            assert!(
                matches!(l.validate(), Err(AiConfigProblem::LimitOutOfRange { key: k, .. }) if k == key)
            );
        }
        assert!(check_limit("max_rows", -5, &AI_MAX_ROWS_RANGE).is_err());
        assert!(check_limit("max_rows", i64::MAX, &AI_MAX_ROWS_RANGE).is_err());
        assert_eq!(
            check_limit("max_rows", 1_000_000, &AI_MAX_ROWS_RANGE),
            Ok(1_000_000)
        );
    }
}
