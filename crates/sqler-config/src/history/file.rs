// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `history.jsonl` wire format: one versioned JSON object per line.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::entry::{
    HISTORY_FORMAT_VERSION, HistoryEntry, HistoryIssueKind, HistoryOutcome, MAX_HISTORY_SQL_BYTES,
};
use crate::ConnectionId;

/// Only the version, so a newer layout is recognised before its fields are.
#[derive(Deserialize)]
struct Probe {
    v: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    started_ms: u64,
    connection: String,
    duration_ms: u64,
    outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rows: Option<u64>,
    sql: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
}

/// Whether `id` follows the connection id grammar (`[a-z0-9-]+`, keychain safe).
pub(crate) fn valid_connection_id(id: &str) -> bool {
    crate::secrets::validate_id(id).is_ok()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn malformed(column: Option<usize>) -> HistoryIssueKind {
    HistoryIssueKind::Malformed { column }
}

/// Parses one line (without its newline).
pub(crate) fn parse_line(line: &[u8]) -> Result<HistoryEntry, HistoryIssueKind> {
    let probe: Probe = serde_json::from_slice(line).map_err(|e| malformed(Some(e.column())))?;
    match probe.v.as_ref().and_then(serde_json::Value::as_u64) {
        Some(1) => {}
        Some(v) if v > u64::from(HISTORY_FORMAT_VERSION) => {
            return Err(HistoryIssueKind::NewerVersion { version: v });
        }
        _ => return Err(malformed(None)),
    }
    let w: Wire = serde_json::from_slice(line).map_err(|e| malformed(Some(e.column())))?;
    let outcome = HistoryOutcome::from_wire(&w.outcome).ok_or_else(|| malformed(None))?;
    if !valid_connection_id(&w.connection) {
        return Err(malformed(None));
    }
    let started_at = (w.started_ms <= MAX_STARTED_MS)
        .then(|| UNIX_EPOCH.checked_add(Duration::from_millis(w.started_ms)))
        .flatten()
        .ok_or_else(|| malformed(None))?;
    Ok(HistoryEntry {
        sql: w.sql,
        connection: ConnectionId::from_valid(w.connection),
        started_at,
        duration: Duration::from_millis(w.duration_ms),
        outcome,
        rows: w.rows,
        truncated: w.truncated,
    })
}

/// Latest accepted `started_ms` (end of year 9999). Keeps the result inside the
/// range of `SystemTime` on every platform.
const MAX_STARTED_MS: u64 = 253_402_300_799_999;

/// Serialises an entry as one line including the trailing `\n`.
pub(crate) fn serialize_line(entry: &HistoryEntry) -> Vec<u8> {
    let wire = Wire {
        v: HISTORY_FORMAT_VERSION,
        started_ms: to_ms(entry.started_at),
        connection: entry.connection.as_str().to_owned(),
        duration_ms: u64::try_from(entry.duration.as_millis()).unwrap_or(u64::MAX),
        outcome: entry.outcome.as_wire().to_owned(),
        rows: entry.rows,
        sql: entry.sql.clone(),
        truncated: entry.truncated,
    };
    // Serialising plain strings and integers cannot fail; fall back to an empty
    // line, which the loader skips, rather than panic.
    let mut line = serde_json::to_vec(&wire).unwrap_or_default();
    line.push(b'\n');
    line
}

/// Unix-epoch milliseconds; a time before 1970 is 0.
pub(crate) fn to_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Cuts `sql` to [`MAX_HISTORY_SQL_BYTES`] at a char boundary.
pub(crate) fn truncate_sql(mut sql: String) -> (String, bool) {
    if sql.len() <= MAX_HISTORY_SQL_BYTES {
        return (sql, false);
    }
    let mut cut = MAX_HISTORY_SQL_BYTES;
    while !sql.is_char_boundary(cut) {
        cut -= 1;
    }
    sql.truncate(cut);
    (sql, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"v":1,"started_ms":1760950000123,"connection":"billing","duration_ms":42,"outcome":"ok","rows":10,"sql":"select 1"}"#;

    #[test]
    fn parses_a_valid_line_and_round_trips_byte_for_byte() {
        let e = parse_line(GOOD.as_bytes()).unwrap();
        assert_eq!(e.sql(), "select 1");
        assert_eq!(e.connection().as_str(), "billing");
        assert_eq!(to_ms(e.started_at()), 1_760_950_000_123);
        assert_eq!(e.duration(), Duration::from_millis(42));
        assert_eq!(e.outcome(), HistoryOutcome::Ok);
        assert_eq!(e.rows(), Some(10));
        assert!(!e.truncated());
        assert_eq!(serialize_line(&e), format!("{GOOD}\n").into_bytes());
    }

    #[test]
    fn rows_omitted_and_truncated_flag() {
        let line = r#"{"v":1,"started_ms":0,"connection":"a","duration_ms":0,"outcome":"cancelled","sql":"x","truncated":true}"#;
        let e = parse_line(line.as_bytes()).unwrap();
        assert_eq!(e.rows(), None);
        assert!(e.truncated());
        assert_eq!(serialize_line(&e), format!("{line}\n").into_bytes());
    }

    #[test]
    fn rejects_extra_field_bad_outcome_bad_id() {
        for bad in [
            GOOD.replace("\"sql\"", "\"extra\":1,\"sql\""),
            GOOD.replace("\"ok\"", "\"maybe\""),
            GOOD.replace("billing", "Bill ing"),
            GOOD.replace("billing", ""),
            GOOD.replace("\"started_ms\":1760950000123", "\"started_ms\":-1"),
        ] {
            assert!(
                matches!(
                    parse_line(bad.as_bytes()),
                    Err(HistoryIssueKind::Malformed { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn version_probe() {
        let with_v = |v: &str| GOOD.replace("\"v\":1", &format!("\"v\":{v}"));
        assert!(matches!(
            parse_line(with_v("0").as_bytes()),
            Err(HistoryIssueKind::Malformed { .. })
        ));
        assert!(matches!(
            parse_line(with_v("\"1\"").as_bytes()),
            Err(HistoryIssueKind::Malformed { .. })
        ));
        assert_eq!(
            parse_line(with_v("2").as_bytes()).unwrap_err(),
            HistoryIssueKind::NewerVersion { version: 2 }
        );
        // A newer layout is recognised even when its fields differ.
        assert_eq!(
            parse_line(br#"{"v":7,"other":true}"#).unwrap_err(),
            HistoryIssueKind::NewerVersion { version: 7 }
        );
        let missing = GOOD.replace("\"v\":1,", "");
        assert!(matches!(
            parse_line(missing.as_bytes()),
            Err(HistoryIssueKind::Malformed { .. })
        ));
    }

    #[test]
    fn invalid_utf8_and_non_objects_are_malformed() {
        for bad in [&b"\xff\xfe"[..], b"[]", b"{", b"null", b"123"] {
            assert!(matches!(
                parse_line(bad),
                Err(HistoryIssueKind::Malformed { .. })
            ));
        }
    }

    #[test]
    fn error_messages_do_not_echo_the_line() {
        let secret = r#"{"v":1,"sql":"select 'topsecret'","oops""#;
        let kind = parse_line(secret.as_bytes()).unwrap_err();
        let msg = kind.to_string();
        assert!(
            !msg.contains("topsecret") && !msg.contains("select"),
            "{msg}"
        );
    }

    #[test]
    fn times_before_1970_are_zero() {
        assert_eq!(to_ms(UNIX_EPOCH - Duration::from_secs(5)), 0);
    }

    #[test]
    fn truncation_cuts_on_a_char_boundary() {
        let (s, t) = truncate_sql("a".repeat(MAX_HISTORY_SQL_BYTES));
        assert!(!t);
        assert_eq!(s.len(), MAX_HISTORY_SQL_BYTES);
        // A 3-byte char straddling the limit is dropped whole.
        let mut sql = "a".repeat(MAX_HISTORY_SQL_BYTES - 1);
        sql.push('€');
        let (s, t) = truncate_sql(sql);
        assert!(t);
        assert_eq!(s.len(), MAX_HISTORY_SQL_BYTES - 1);
    }

    #[test]
    fn sql_with_newlines_stays_on_one_line() {
        let mut e = parse_line(GOOD.as_bytes()).unwrap();
        e.sql = "a\nb\r\n\"c\"".into();
        let line = serialize_line(&e);
        assert_eq!(line.split(|&b| b == b'\n').count() - 1, 1);
        assert_eq!(parse_line(&line[..line.len() - 1]).unwrap().sql(), e.sql());
    }

    #[test]
    fn started_ms_beyond_system_time_range_is_malformed() {
        let line = GOOD.replace("1760950000123", "18446744073709551615");
        assert!(matches!(
            parse_line(line.as_bytes()),
            Err(HistoryIssueKind::Malformed { .. })
        ));
    }
}
