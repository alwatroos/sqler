// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Turns what a call did into an [`AuditEntry`]. All sanitizing happens here, so the caller only
//! maps fields.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use sqler_config::redact_credentials;
use sqler_sql::{Dialect, StatementClass, TokenKind, tokenize};

use super::entry::{
    ApprovalOutcome, AuditConnection, AuditEntry, AuditFailure, AuditStatement, CallOutcome,
    Surface, new_id,
};
use super::{
    MAX_AUDIT_ARGUMENTS_BYTES, MAX_AUDIT_SQL_BYTES, MAX_AUDIT_STATEMENTS, MAX_AUDIT_TEXT_BYTES,
};
use crate::redact::{REDACTED, Redaction, RedactionSummary};

/// Everything the audit needs from one finished call.
pub(crate) struct AuditDraft<'a> {
    pub started: SystemTime,
    pub duration: Duration,
    pub surface: Surface,
    pub tool: &'a str,
    /// The connection id and display name.
    pub connection: Option<(&'a str, &'a str)>,
    pub raw_arguments: &'a str,
    pub statements: Vec<DraftStatement<'a>>,
    pub error: Option<DraftError<'a>>,
    pub result_bytes: usize,
    pub redaction: RedactionSummary,
    /// The configured redaction list (built-in plus user entries) the SQL text is checked with.
    pub rules: &'a Redaction,
    /// The dialect of the connection, if the driver has one.
    pub dialect: Option<Dialect>,
}

pub(crate) struct DraftStatement<'a> {
    pub sql: &'a str,
    pub class: StatementClass,
    pub needs_approval: bool,
    pub ran: Option<DraftRun>,
}

pub(crate) struct DraftRun {
    pub rows: u64,
    pub truncated: Option<&'static str>,
    pub failed: bool,
}

pub(crate) struct DraftError<'a> {
    pub code: &'a str,
    pub message: &'a str,
    pub denied_index: Option<usize>,
}

/// Cuts `text` to `max` bytes at a char boundary.
fn cut(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Replaces the token after each `Bearer ` / `Basic ` scheme word (any case) with the marker.
fn hide_auth_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let next = ["bearer ", "basic "]
            .iter()
            .filter_map(|w| lower[at..].find(w).map(|i| (at + i, w.len())))
            .min();
        let Some((pos, len)) = next else { break };
        let token_start = pos + len;
        let token_end = text[token_start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')'))
            .map_or(text.len(), |i| token_start + i);
        out.push_str(&text[at..token_start]);
        if token_end > token_start {
            out.push_str(REDACTED);
        }
        at = token_end;
    }
    out.push_str(&text[at..]);
    out
}

fn scrub(text: &str) -> String {
    hide_auth_tokens(&redact_credentials(text))
}

/// The redaction list and dialect the SQL text is checked with.
#[derive(Clone, Copy)]
struct SqlRules<'a> {
    rules: &'a Redaction,
    dialect: Option<Dialect>,
}

/// Replaces every literal and comment of `text` with `?` / nothing. The statement shape, the
/// identifiers and the placeholders stay, so the audit still says what was run.
fn mask_literals(dialect: Option<Dialect>, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for t in tokenize(dialect.unwrap_or(Dialect::Postgres), text) {
        let masked = match t.kind {
            TokenKind::Str { .. } | TokenKind::DollarStr { .. } | TokenKind::Number => Some("?"),
            TokenKind::LineComment
            | TokenKind::BlockComment { .. }
            | TokenKind::ExecComment { .. } => Some(""),
            _ => None,
        };
        if let Some(mark) = masked {
            out.push_str(&text[at..t.start]);
            out.push_str(mark);
            at = t.end;
        }
    }
    out.push_str(&text[at..]);
    out
}

/// Cut, then credential-redacted (the history order). A statement that mentions a listed word
/// (`ssn`, a user entry) keeps no literal at all, wherever the literal sits: the check runs on
/// the whole text, the masking on the part that is kept.
fn sql_text(text: &str, rules: SqlRules<'_>) -> (String, bool) {
    let (cut_text, truncated) = cut(text, MAX_AUDIT_SQL_BYTES);
    if rules.rules.statement_tainted(rules.dialect, text) {
        return (scrub(&mask_literals(rules.dialect, cut_text)), truncated);
    }
    (scrub(cut_text), truncated)
}

/// Credential-redacted, then cut.
fn plain_text(text: &str, max: usize) -> (String, bool) {
    let redacted = scrub(text);
    let (cut_text, truncated) = cut(&redacted, max);
    (cut_text.to_owned(), truncated)
}

struct ArgScrub<'a> {
    rules: SqlRules<'a>,
    truncated: bool,
}

impl ArgScrub<'_> {
    fn value(&mut self, key: Option<&str>, value: &Value) -> Value {
        // A value stored under a name like `password` or `api_key` is never kept.
        if key.is_some_and(|k| self.rules.rules.column_redacted(k)) && !value.is_null() {
            return Value::String(REDACTED.to_owned());
        }
        match value {
            Value::String(s) => {
                let (text, cut) = if key == Some("sql") {
                    sql_text(s, self.rules)
                } else {
                    plain_text(s, MAX_AUDIT_TEXT_BYTES)
                };
                self.truncated |= cut;
                Value::String(text)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.value(None, v)).collect())
            }
            Value::Object(map) => Value::Object(self.object(map)),
            other => other.clone(),
        }
    }

    fn object(&mut self, map: &Map<String, Value>) -> Map<String, Value> {
        map.iter()
            .map(|(k, v)| (k.clone(), self.value(Some(k), v)))
            .collect()
    }
}

fn arguments(raw: &str, rules: SqlRules<'_>) -> (Value, bool) {
    let mut scrub = ArgScrub {
        rules,
        truncated: false,
    };
    if raw.trim().is_empty() {
        return (Value::Object(Map::new()), false);
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) {
        let out = Value::Object(scrub.object(&map));
        let size = out.to_string().len();
        if size > MAX_AUDIT_ARGUMENTS_BYTES {
            let mut omitted = Map::new();
            omitted.insert("_omitted".to_owned(), Value::from(size as u64));
            return (Value::Object(omitted), true);
        }
        return (out, scrub.truncated);
    }
    let (text, truncated) = plain_text(raw, MAX_AUDIT_TEXT_BYTES);
    let mut map = Map::new();
    map.insert("_unparsed".to_owned(), Value::String(text));
    (Value::Object(map), truncated)
}

/// The approval outcome of each statement, by position.
fn approvals(
    statements: &[DraftStatement<'_>],
    error: Option<&DraftError<'_>>,
) -> Vec<ApprovalOutcome> {
    let code = error.map(|e| e.code);
    let needing: Vec<usize> = statements
        .iter()
        .enumerate()
        .filter(|(_, s)| s.needs_approval)
        .map(|(i, _)| i)
        .collect();
    let denied_at = match (code, error) {
        (Some("denied"), Some(e)) => Some(e.denied_index.or_else(|| needing.first().copied())),
        _ => None,
    };
    let refuses = matches!(code, Some("no_approver" | "read_only"));
    statements
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if !s.needs_approval {
                ApprovalOutcome::NotNeeded
            } else if let Some(Some(k)) = denied_at {
                match i.cmp(&k) {
                    std::cmp::Ordering::Less => ApprovalOutcome::Approved,
                    std::cmp::Ordering::Equal => ApprovalOutcome::Denied,
                    std::cmp::Ordering::Greater => ApprovalOutcome::NotAsked,
                }
            } else if refuses {
                if needing.first() == Some(&i) {
                    ApprovalOutcome::Refused
                } else {
                    ApprovalOutcome::NotAsked
                }
            } else {
                ApprovalOutcome::Approved
            }
        })
        .collect()
}

impl AuditEntry {
    /// Builds a sanitized entry from a finished call.
    pub(crate) fn from_draft(d: AuditDraft<'_>) -> Self {
        let ts_ms = d
            .started
            .duration_since(UNIX_EPOCH)
            .map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
        let rules = SqlRules {
            rules: d.rules,
            dialect: d.dialect,
        };
        let (arguments, arguments_truncated) = arguments(d.raw_arguments, rules);
        let outcomes = approvals(&d.statements, d.error.as_ref());
        let total = d.statements.len();
        let statements: Vec<AuditStatement> = d
            .statements
            .iter()
            .zip(&outcomes)
            .take(MAX_AUDIT_STATEMENTS)
            .map(|(s, &approval)| {
                let (sql, sql_truncated) = sql_text(s.sql, rules);
                AuditStatement {
                    sql,
                    sql_truncated,
                    class: s.class.as_str().to_owned(),
                    approval,
                    ran: s.ran.is_some(),
                    rows: s.ran.as_ref().map(|r| r.rows),
                    truncated: s.ran.as_ref().and_then(|r| r.truncated).map(str::to_owned),
                    failed: s.ran.as_ref().is_some_and(|r| r.failed),
                }
            })
            .collect();
        let has = |o| outcomes.contains(&o);
        let approval = if has(ApprovalOutcome::Denied) {
            ApprovalOutcome::Denied
        } else if has(ApprovalOutcome::Refused) {
            ApprovalOutcome::Refused
        } else if has(ApprovalOutcome::Approved) {
            ApprovalOutcome::Approved
        } else {
            ApprovalOutcome::NotNeeded
        };
        let rows = d
            .statements
            .iter()
            .filter_map(|s| s.ran.as_ref())
            .fold(0_u64, |acc, r| acc.saturating_add(r.rows));
        Self {
            id: new_id(ts_ms),
            ts_ms,
            surface: d.surface,
            tool: plain_text(d.tool, MAX_AUDIT_TEXT_BYTES).0,
            connection: d
                .connection
                .map(|(id, name)| AuditConnection::new(id, name)),
            arguments,
            arguments_truncated,
            statements_omitted: u32::try_from(total.saturating_sub(MAX_AUDIT_STATEMENTS))
                .unwrap_or(u32::MAX),
            statements,
            approval,
            outcome: if d.error.is_some() {
                CallOutcome::Error
            } else {
                CallOutcome::Ok
            },
            error: d
                .error
                .map(|e| AuditFailure::new(e.code, plain_text(e.message, MAX_AUDIT_TEXT_BYTES).0)),
            result_bytes: u64::try_from(d.result_bytes).unwrap_or(u64::MAX),
            rows,
            duration_ms: u64::try_from(d.duration.as_millis()).unwrap_or(u64::MAX),
            redaction: d.redaction,
        }
    }
}
