// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `ai-audit.jsonl` wire format, version 1: one JSON object per line.

use serde::{Deserialize, Serialize};

use super::AUDIT_FORMAT_VERSION;
use super::entry::{
    ApprovalOutcome, AuditConnection, AuditEntry, AuditFailure, AuditStatement, CallOutcome,
    Surface,
};
use crate::redact::RedactionSummary;

/// Why a line did not load. Never carries the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineIssue {
    /// Not a valid version 1 entry.
    Malformed,
    /// Written by a newer version.
    Newer,
}

/// Only the version, so a newer layout is recognised before its fields are.
#[derive(Deserialize)]
struct Probe {
    v: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireConnection {
    id: String,
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFailure {
    code: String,
    message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStatement {
    sql: String,
    sql_truncated: bool,
    class: String,
    approval: String,
    ran: bool,
    rows: Option<u64>,
    truncated: Option<String>,
    failed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRedaction {
    columns: Vec<String>,
    tainted_statements: u32,
    rows_withheld: bool,
    errors_withheld: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    v: u32,
    id: String,
    ts_ms: u64,
    surface: String,
    tool: String,
    connection: Option<WireConnection>,
    arguments: serde_json::Value,
    arguments_truncated: bool,
    statements: Vec<WireStatement>,
    statements_omitted: u32,
    approval: String,
    outcome: String,
    error: Option<WireFailure>,
    result_bytes: u64,
    rows: u64,
    duration_ms: u64,
    redaction: WireRedaction,
}

/// Parses one line (without its newline).
pub(crate) fn parse_line(line: &[u8]) -> Result<AuditEntry, LineIssue> {
    let probe: Probe = serde_json::from_slice(line).map_err(|_| LineIssue::Malformed)?;
    match probe.v.as_ref().and_then(serde_json::Value::as_u64) {
        Some(1) => {}
        Some(v) if v > u64::from(AUDIT_FORMAT_VERSION) => return Err(LineIssue::Newer),
        _ => return Err(LineIssue::Malformed),
    }
    let w: Wire = serde_json::from_slice(line).map_err(|_| LineIssue::Malformed)?;
    let bad = || LineIssue::Malformed;
    let statements = w
        .statements
        .into_iter()
        .map(|s| {
            Ok(AuditStatement {
                sql: s.sql,
                sql_truncated: s.sql_truncated,
                class: s.class,
                approval: ApprovalOutcome::from_wire(&s.approval).ok_or_else(bad)?,
                ran: s.ran,
                rows: s.rows,
                truncated: s.truncated,
                failed: s.failed,
            })
        })
        .collect::<Result<Vec<_>, LineIssue>>()?;
    Ok(AuditEntry {
        id: w.id,
        ts_ms: w.ts_ms,
        surface: Surface::from_wire(&w.surface).ok_or_else(bad)?,
        tool: w.tool,
        connection: w.connection.map(|c| AuditConnection::new(c.id, c.name)),
        arguments: w.arguments,
        arguments_truncated: w.arguments_truncated,
        statements,
        statements_omitted: w.statements_omitted,
        approval: ApprovalOutcome::from_wire(&w.approval).ok_or_else(bad)?,
        outcome: CallOutcome::from_wire(&w.outcome).ok_or_else(bad)?,
        error: w.error.map(|e| AuditFailure::new(e.code, e.message)),
        result_bytes: w.result_bytes,
        rows: w.rows,
        duration_ms: w.duration_ms,
        redaction: RedactionSummary {
            columns: w.redaction.columns,
            tainted_statements: w.redaction.tainted_statements,
            rows_withheld: w.redaction.rows_withheld,
            errors_withheld: w.redaction.errors_withheld,
        },
    })
}

/// Serialises an entry as one line including the trailing `\n`.
pub(crate) fn serialize_line(entry: &AuditEntry) -> Vec<u8> {
    let wire = Wire {
        v: AUDIT_FORMAT_VERSION,
        id: entry.id.clone(),
        ts_ms: entry.ts_ms,
        surface: entry.surface.as_str().to_owned(),
        tool: entry.tool.clone(),
        connection: entry.connection.as_ref().map(|c| WireConnection {
            id: c.id.clone(),
            name: c.name.clone(),
        }),
        arguments: entry.arguments.clone(),
        arguments_truncated: entry.arguments_truncated,
        statements: entry
            .statements
            .iter()
            .map(|s| WireStatement {
                sql: s.sql.clone(),
                sql_truncated: s.sql_truncated,
                class: s.class.clone(),
                approval: s.approval.as_str().to_owned(),
                ran: s.ran,
                rows: s.rows,
                truncated: s.truncated.clone(),
                failed: s.failed,
            })
            .collect(),
        statements_omitted: entry.statements_omitted,
        approval: entry.approval.as_str().to_owned(),
        outcome: entry.outcome.as_str().to_owned(),
        error: entry.error.as_ref().map(|e| WireFailure {
            code: e.code.clone(),
            message: e.message.clone(),
        }),
        result_bytes: entry.result_bytes,
        rows: entry.rows,
        duration_ms: entry.duration_ms,
        redaction: WireRedaction {
            columns: entry.redaction.columns.clone(),
            tainted_statements: entry.redaction.tainted_statements,
            rows_withheld: entry.redaction.rows_withheld,
            errors_withheld: entry.redaction.errors_withheld,
        },
    };
    // Serialising plain strings, numbers and a JSON value cannot fail; an empty line (skipped
    // by the reader) is better than a panic.
    let mut line = serde_json::to_vec(&wire).unwrap_or_default();
    line.push(b'\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"v":1,"id":"a-b-c","ts_ms":1760950000123,"surface":"mcp","tool":"run_query","connection":{"id":"db","name":"Test DB"},"arguments":{"sql":"select 1"},"arguments_truncated":false,"statements":[{"sql":"select 1","sql_truncated":false,"class":"read","approval":"not_needed","ran":true,"rows":3,"truncated":null,"failed":false}],"statements_omitted":0,"approval":"not_needed","outcome":"ok","error":null,"result_bytes":512,"rows":3,"duration_ms":12,"redaction":{"columns":[],"tainted_statements":0,"rows_withheld":false,"errors_withheld":0}}"#;

    #[test]
    fn parses_the_spec_example_and_round_trips_byte_for_byte() {
        let e = parse_line(GOOD.as_bytes()).unwrap();
        assert_eq!(e.surface, Surface::Mcp);
        assert_eq!(e.connection.as_ref().unwrap().name, "Test DB");
        assert_eq!(e.statements[0].rows, Some(3));
        assert_eq!(serialize_line(&e), format!("{GOOD}\n").into_bytes());
    }

    #[test]
    fn rejects_extra_fields_bad_enums_and_non_entries() {
        for bad in [
            GOOD.replace("\"tool\"", "\"extra\":1,\"tool\""),
            GOOD.replace("\"mcp\"", "\"cli\""),
            GOOD.replace("\"not_needed\"", "\"maybe\""),
            GOOD.replace("\"ok\"", "\"fine\""),
            GOOD.replace("\"ts_ms\":1760950000123", "\"ts_ms\":-1"),
            GOOD.replace("\"v\":1,", ""),
            GOOD.replace("\"v\":1", "\"v\":\"1\""),
            GOOD.replace("\"v\":1", "\"v\":0"),
            "[]".to_owned(),
            "{".to_owned(),
        ] {
            assert_eq!(
                parse_line(bad.as_bytes()).unwrap_err(),
                LineIssue::Malformed,
                "{bad}"
            );
        }
        assert_eq!(parse_line(b"\xff\xfe").unwrap_err(), LineIssue::Malformed);
    }

    #[test]
    fn newer_versions_are_recognised_by_the_probe_alone() {
        assert_eq!(
            parse_line(GOOD.replace("\"v\":1", "\"v\":2").as_bytes()).unwrap_err(),
            LineIssue::Newer
        );
        assert_eq!(
            parse_line(br#"{"v":7,"other":true}"#).unwrap_err(),
            LineIssue::Newer
        );
    }

    #[test]
    fn multi_line_text_stays_on_one_line() {
        let mut e = parse_line(GOOD.as_bytes()).unwrap();
        e.statements[0].sql = "a\nb\r\n\"c\"".into();
        let line = serialize_line(&e);
        assert_eq!(line.split(|&b| b == b'\n').count(), 2);
        let back = parse_line(&line[..line.len() - 1]).unwrap();
        assert_eq!(back.statements[0].sql, e.statements[0].sql);
    }
}
