// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Per-plugin rate limits for the `ui` import and the sanitising of the text a plugin pushes to the window.

use std::time::Instant;

/// Status text cap in bytes.
pub(crate) const STATUS_MAX_BYTES: usize = 256;
/// Notification text cap in bytes.
pub(crate) const NOTIFY_MAX_BYTES: usize = 512;

/// Micro-tokens in one token, so refill needs no floating point.
const MICRO: u64 = 1_000_000;

/// A token bucket. The clock is an argument so tests use synthetic instants.
#[derive(Debug, Clone)]
pub(crate) struct Bucket {
    capacity: u64,
    /// Micro-tokens gained per millisecond (`tokens per second * 1000`).
    refill_per_ms: u64,
    micro_tokens: u64,
    last: Option<Instant>,
}

impl Bucket {
    /// A full bucket of `burst` tokens refilled at `per_second_milli / 1000` tokens per second.
    pub(crate) fn new(burst: u64, per_second_milli: u64) -> Self {
        Self {
            capacity: burst * MICRO,
            refill_per_ms: per_second_milli,
            micro_tokens: burst * MICRO,
            last: None,
        }
    }

    /// Takes one token at `now`; `false` means the call is over its rate.
    pub(crate) fn take(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last {
            let ms =
                u64::try_from(now.saturating_duration_since(last).as_millis()).unwrap_or(u64::MAX);
            self.micro_tokens = self
                .micro_tokens
                .saturating_add(ms.saturating_mul(self.refill_per_ms))
                .min(self.capacity);
        }
        if self.last.is_none_or(|l| now > l) {
            self.last = Some(now);
        }
        if self.micro_tokens >= MICRO {
            self.micro_tokens -= MICRO;
            true
        } else {
            false
        }
    }
}

/// The three buckets of one plugin.
#[derive(Debug, Clone)]
pub(crate) struct UiLimits {
    pub refresh: Bucket,
    pub status: Bucket,
    pub notify: Bucket,
}

impl UiLimits {
    /// `refresh-panel` 20 burst / 10 per s, `set-status` 20 / 10 per s, `notify` 3 / 1 per 10 s.
    pub(crate) fn new() -> Self {
        Self {
            refresh: Bucket::new(20, 10_000),
            status: Bucket::new(20, 10_000),
            notify: Bucket::new(3, 100),
        }
    }
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// One line of text with no control characters (line breaks and tabs become spaces, other controls and bidi
/// overrides are dropped), trimmed and cut to `max_bytes` on a char boundary.
pub(crate) fn one_line(text: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_bytes));
    for c in text.chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if is_unsafe_char(c) => continue,
            c => c,
        };
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn burst_then_refill_with_an_injected_clock() {
        let t0 = Instant::now();
        let mut b = Bucket::new(20, 10_000);
        for _ in 0..20 {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0));
        // 10 per second: 100 ms gives one token.
        assert!(!b.take(t0 + Duration::from_millis(50)));
        assert!(b.take(t0 + Duration::from_millis(100)));
        assert!(!b.take(t0 + Duration::from_millis(100)));
        // A long pause refills to the burst and no further.
        let later = t0 + Duration::from_secs(60);
        for _ in 0..20 {
            assert!(b.take(later));
        }
        assert!(!b.take(later));
    }

    #[test]
    fn notify_allows_three_then_one_per_ten_seconds() {
        let t0 = Instant::now();
        let mut b = UiLimits::new().notify;
        assert!(b.take(t0) && b.take(t0) && b.take(t0));
        assert!(!b.take(t0));
        assert!(!b.take(t0 + Duration::from_secs(9)));
        assert!(b.take(t0 + Duration::from_secs(10)));
        assert!(!b.take(t0 + Duration::from_secs(10)));
    }

    #[test]
    fn a_clock_that_goes_back_does_not_refill() {
        let earlier = Instant::now();
        let later = earlier + Duration::from_secs(5);
        let mut b = Bucket::new(1, 1_000);
        assert!(b.take(later));
        assert!(!b.take(earlier));
    }

    #[test]
    fn text_is_one_line_without_control_characters() {
        assert_eq!(one_line("a\nb\r\nc\td", 100), "a b  c d");
        assert_eq!(one_line("x\u{1b}[31my\u{202e}z\0", 100), "x[31myz");
        assert_eq!(one_line("  padded  ", 100), "padded");
        assert_eq!(one_line("\n\n", 100), "");
    }

    #[test]
    fn text_is_cut_at_a_char_boundary() {
        let text = "é".repeat(200);
        let cut = one_line(&text, STATUS_MAX_BYTES);
        assert_eq!(cut.len(), 256);
        let odd = one_line(&text, 255);
        assert_eq!(odd.len(), 254);
        assert!(odd.chars().all(|c| c == 'é'));
    }
}
