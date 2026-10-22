// SPDX-License-Identifier: Apache-2.0
//! `SPIKE key=value` output and the small statistics helpers.

// The base variant has no engine and so no user of the timing helpers.
#![cfg_attr(not(feature = "engine"), allow(dead_code))]

use std::fmt::Display;
use std::time::Duration;

/// Formats one `SPIKE key=value` line (without the newline).
pub fn line(key: &str, value: impl Display) -> String {
    format!("SPIKE {key}={value}")
}

/// Prints one `SPIKE key=value` line to stdout (LF only).
pub fn emit(key: &str, value: impl Display) {
    println!("{}", line(key, value));
}

/// Nearest-rank percentile of an ascending-sorted slice; `None` when empty.
pub fn percentile(sorted: &[u64], pct: u32) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = (n * pct as usize).div_ceil(100).clamp(1, n);
    sorted.get(rank - 1).copied()
}

/// Milliseconds with three decimals.
pub fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

/// Overhead of `with` over `base` in percent, one decimal.
#[allow(clippy::cast_precision_loss)]
pub fn overhead_pct(base: Duration, with: Duration) -> String {
    let b = base.as_secs_f64();
    if b <= 0.0 {
        return "n/a".to_owned();
    }
    format!("{:.1}", (with.as_secs_f64() - b) / b * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        assert_eq!(line("fixture_add", 5), "SPIKE fixture_add=5");
        assert_eq!(line("variant", "base"), "SPIKE variant=base");
    }

    #[test]
    fn percentile_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50), Some(50));
        assert_eq!(percentile(&v, 95), Some(95));
        assert_eq!(percentile(&v, 100), Some(100));
        assert_eq!(percentile(&[7], 50), Some(7));
        assert_eq!(percentile(&[7], 95), Some(7));
        assert_eq!(percentile(&[1, 2, 3, 4], 50), Some(2));
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn ms_and_overhead() {
        assert_eq!(ms(Duration::from_micros(1500)), "1.500");
        assert_eq!(
            overhead_pct(Duration::from_millis(100), Duration::from_millis(125)),
            "25.0"
        );
        assert_eq!(
            overhead_pct(Duration::ZERO, Duration::from_millis(1)),
            "n/a"
        );
    }
}
