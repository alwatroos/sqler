// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Row and time limits.

use std::time::Duration;

use sqler_config::AiLimits;
use sqler_driver_api::sqler_core::Environment;

/// The limits a host configured. `unlimited` removes them, except on `prod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The most rows a statement returns.
    pub max_rows: u32,
    /// The most time one call may run.
    pub timeout: Duration,
    /// Whether the limits are off. Ignored on `prod`.
    pub unlimited: bool,
}

impl Limits {
    /// Creates limits.
    #[must_use]
    pub fn new(max_rows: u32, timeout: Duration, unlimited: bool) -> Self {
        Self {
            max_rows,
            timeout,
            unlimited,
        }
    }

    /// The caps for a connection in `env`. `prod` always gets both caps.
    #[must_use]
    pub fn effective(&self, env: Option<Environment>) -> Caps {
        if self.unlimited && env != Some(Environment::Prod) {
            Caps {
                max_rows: None,
                timeout: None,
            }
        } else {
            Caps {
                max_rows: Some(self.max_rows),
                timeout: Some(self.timeout),
            }
        }
    }
}

impl From<&AiLimits> for Limits {
    fn from(limits: &AiLimits) -> Self {
        Self {
            max_rows: limits.max_rows,
            timeout: Duration::from_secs(u64::from(limits.timeout_secs)),
            unlimited: limits.unlimited,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::from(&AiLimits::default())
    }
}

/// The caps in force for one call; `None` means no cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caps {
    /// The row cap per statement.
    pub max_rows: Option<u32>,
    /// The deadline of the call.
    pub timeout: Option<Duration>,
}

impl Caps {
    /// The row cap when the caller asked for `requested` rows: the smaller of the two.
    #[must_use]
    pub fn rows_for(&self, requested: Option<u32>) -> Option<u32> {
        match (requested, self.max_rows) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_config() {
        let l = Limits::default();
        assert_eq!(l.max_rows, 1000);
        assert_eq!(l.timeout, Duration::from_secs(30));
        assert!(!l.unlimited);
    }

    #[test]
    fn unlimited_is_ignored_on_prod() {
        let l = Limits::new(10, Duration::from_secs(2), true);
        assert_eq!(l.effective(None).max_rows, None);
        assert_eq!(l.effective(Some(Environment::Dev)).timeout, None);
        assert_eq!(l.effective(Some(Environment::Staging)).max_rows, None);
        let prod = l.effective(Some(Environment::Prod));
        assert_eq!(prod.max_rows, Some(10));
        assert_eq!(prod.timeout, Some(Duration::from_secs(2)));
        let limited = Limits::new(10, Duration::from_secs(2), false);
        assert_eq!(limited.effective(None).max_rows, Some(10));
    }

    #[test]
    fn rows_for_takes_the_smaller() {
        let c = Caps {
            max_rows: Some(10),
            timeout: None,
        };
        assert_eq!(c.rows_for(None), Some(10));
        assert_eq!(c.rows_for(Some(3)), Some(3));
        assert_eq!(c.rows_for(Some(30)), Some(10));
        let free = Caps {
            max_rows: None,
            timeout: None,
        };
        assert_eq!(free.rows_for(None), None);
        assert_eq!(free.rows_for(Some(5)), Some(5));
    }
}
