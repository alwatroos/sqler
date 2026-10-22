// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Plugin log lines: a rate limiter and the sanitising `tracing` bridge.

use std::time::Instant;

use sqler_plugin_api::PluginId;

/// Severity of a plugin log line (WIT `log.level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Trace.
    Trace,
    /// Debug.
    Debug,
    /// Info.
    Info,
    /// Warn.
    Warn,
    /// Error.
    Error,
}

/// Bucket capacity in lines.
pub(crate) const LOG_CAPACITY: u64 = 100;
/// Refill rate in lines per second.
pub(crate) const LOG_REFILL_PER_SEC: u64 = 20;
/// Longest log message in bytes, after escaping.
pub(crate) const LOG_MAX_BYTES: usize = 4096;

/// Milli-lines, so refill needs no floating point.
const MILLI: u64 = 1000;

/// What the limiter decided for one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Write the line; `suppressed` lines were dropped since the last written one.
    Pass {
        /// Lines dropped since the last pass.
        suppressed: u64,
    },
    /// Drop the line.
    Drop,
}

/// Token bucket: 100 lines, refilled at 20 lines/s. The clock is an argument so tests use synthetic instants.
#[derive(Debug)]
pub(crate) struct LogLimiter {
    milli_tokens: u64,
    last: Option<Instant>,
    suppressed: u64,
}

impl LogLimiter {
    /// A full bucket.
    pub(crate) fn new() -> Self {
        Self {
            milli_tokens: LOG_CAPACITY * MILLI,
            last: None,
            suppressed: 0,
        }
    }

    /// Decides whether a line at `now` is written.
    pub(crate) fn admit(&mut self, now: Instant) -> Admit {
        if let Some(last) = self.last {
            let ms =
                u64::try_from(now.saturating_duration_since(last).as_millis()).unwrap_or(u64::MAX);
            // 20 lines per second is 20 milli-lines per millisecond.
            let gained = ms.saturating_mul(LOG_REFILL_PER_SEC);
            self.milli_tokens = self
                .milli_tokens
                .saturating_add(gained)
                .min(LOG_CAPACITY * MILLI);
        }
        if self.last.is_none_or(|l| now > l) {
            self.last = Some(now);
        }
        if self.milli_tokens >= MILLI {
            self.milli_tokens -= MILLI;
            Admit::Pass {
                suppressed: std::mem::take(&mut self.suppressed),
            }
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            Admit::Drop
        }
    }
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Escapes control characters (a guest cannot forge a second log line or terminal escape) and cuts at 4096 bytes on
/// a char boundary. The result never exceeds [`LOG_MAX_BYTES`].
pub(crate) fn sanitize(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(LOG_MAX_BYTES));
    for c in message.chars() {
        let piece = match c {
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            c if is_unsafe_char(c) => format!("\\u{{{:x}}}", u32::from(c)),
            c => c.to_string(),
        };
        if out.len() + piece.len() > LOG_MAX_BYTES {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// Writes one plugin line as a `tracing` event: target `sqler_plugin`, field `plugin`.
pub(crate) fn emit(plugin: &PluginId, level: LogLevel, message: &str) {
    let message = sanitize(message);
    match level {
        LogLevel::Trace => {
            tracing::trace!(target: "sqler_plugin", plugin = %plugin, "{message}");
        }
        LogLevel::Debug => {
            tracing::debug!(target: "sqler_plugin", plugin = %plugin, "{message}");
        }
        LogLevel::Info => {
            tracing::info!(target: "sqler_plugin", plugin = %plugin, "{message}");
        }
        LogLevel::Warn => {
            tracing::warn!(target: "sqler_plugin", plugin = %plugin, "{message}");
        }
        LogLevel::Error => {
            tracing::error!(target: "sqler_plugin", plugin = %plugin, "{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    #[test]
    fn burst_then_drop_then_refill_reports_suppressed() {
        let t0 = Instant::now();
        let mut l = LogLimiter::new();
        for _ in 0..LOG_CAPACITY {
            assert_eq!(l.admit(t0), Admit::Pass { suppressed: 0 });
        }
        for _ in 0..7 {
            assert_eq!(l.admit(t0), Admit::Drop);
        }
        // 100 ms refills two lines.
        let t1 = t0 + Duration::from_millis(100);
        assert_eq!(l.admit(t1), Admit::Pass { suppressed: 7 });
        assert_eq!(l.admit(t1), Admit::Pass { suppressed: 0 });
        assert_eq!(l.admit(t1), Admit::Drop);
    }

    #[test]
    fn refill_is_capped_at_capacity_and_time_going_back_is_harmless() {
        let t0 = Instant::now();
        let mut l = LogLimiter::new();
        let later = t0 + Duration::from_secs(3600);
        let mut passed = 0;
        for _ in 0..(LOG_CAPACITY + 50) {
            if matches!(l.admit(later), Admit::Pass { .. }) {
                passed += 1;
            }
        }
        assert_eq!(passed, LOG_CAPACITY);
        assert_eq!(l.admit(t0), Admit::Drop);
    }

    #[test]
    fn newlines_and_controls_are_escaped() {
        let s = sanitize("ok\nINFO forged\r\n\x1b[31mred\x07\u{2028}");
        assert!(!s.contains('\n') && !s.contains('\r') && !s.contains('\x1b'));
        assert_eq!(s, "ok\\nINFO forged\\r\\n\\u{1b}[31mred\\u{7}\\u{2028}");
    }

    #[test]
    fn cut_is_at_4096_bytes_on_a_char_boundary() {
        let s = sanitize(&"a".repeat(10_000));
        assert_eq!(s.len(), LOG_MAX_BYTES);
        // 3-byte chars: 4096 is not a multiple of 3, so the cut must fall back to a boundary.
        let s = sanitize(&"\u{20ac}".repeat(3000));
        assert_eq!(s.len(), 4095);
        assert!(s.chars().all(|c| c == '\u{20ac}'));
        // Escapes are not split either.
        let s = sanitize(&format!("{}\n", "a".repeat(4095)));
        assert_eq!(s.len(), 4095);
        assert_eq!(sanitize("short"), "short");
    }

    #[derive(Default)]
    struct Seen(Mutex<Vec<(String, String, String)>>);

    struct Collect(Arc<Seen>);

    struct Fields(String, String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            match field.name() {
                "plugin" => self.0 = format!("{value:?}"),
                "message" => self.1 = format!("{value:?}"),
                _ => {}
            }
        }
    }

    impl Subscriber for Collect {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut f = Fields(String::new(), String::new());
            event.record(&mut f);
            self.0
                .0
                .lock()
                .unwrap()
                .push((event.metadata().target().to_owned(), f.0, f.1));
        }
    }

    #[test]
    fn emit_uses_target_and_plugin_field_and_sanitises() {
        let seen = Arc::new(Seen::default());
        let id: PluginId = "demo".parse().unwrap();
        tracing::subscriber::with_default(Collect(Arc::clone(&seen)), || {
            emit(&id, LogLevel::Warn, "a\nb");
        });
        let lines = seen.0.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "sqler_plugin");
        assert_eq!(lines[0].1, "demo");
        assert_eq!(lines[0].2, "a\\nb");
    }
}
