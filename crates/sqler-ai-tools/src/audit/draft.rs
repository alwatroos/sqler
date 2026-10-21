// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Turns what a call did into an [`AuditEntry`]. All sanitizing happens here, so the caller only
//! maps fields.
//!
//! # Every string of an entry, and its scrub
//!
//! Every text that ends up in an [`AuditEntry`] passes through [`scrub_text`], the one function
//! that cuts, masks and redacts. A field not in this table does not hold caller text.
//!
//! | Field                              | Scrub                                   | Cap            |
//! | ---------------------------------- | --------------------------------------- | -------------- |
//! | `tool`                             | `scrub_text`                            | text           |
//! | `connection.id`, `connection.name` | `scrub_text`                            | text           |
//! | `arguments` object keys (any depth)| plain identifier as is, else `scrub_text`| text          |
//! | `arguments` string values          | `scrub_text` (key `sql`: the SQL cap)   | text / SQL     |
//! | `arguments` under a listed key     | replaced by the marker, whole subtree   | none           |
//! | `arguments._unparsed`              | `scrub_text` on the raw text            | text           |
//! | `statements[].sql`                 | `scrub_text`                            | SQL            |
//! | `error.code`, `error.message`      | `scrub_text`                            | text           |
//! | `id`, `surface`, `approval`, ...   | generated or enums, no caller text      | n/a            |
//! | `statements[].class`, `truncated`  | fixed names from the guard              | n/a            |
//!
//! There are no approval notes: the approval outcome is an enum. The `scrub_text` steps are:
//! cut to the cap; if the whole text mentions a listed word (as SQL for the dialect, or as a
//! plain word anywhere), SQL (`statements[].sql`, the `sql` argument) has every literal, quoted
//! identifier, number and comment masked, and any other text is replaced by the marker, as is
//! SQL whose quotes do not close (a truncated call); credential redaction; cut again to the
//! cap.

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

/// The redaction list and dialect the text is checked with.
#[derive(Clone, Copy)]
struct SqlRules<'a> {
    rules: &'a Redaction,
    dialect: Option<Dialect>,
}

/// Which cap a text gets.
#[derive(Clone, Copy)]
enum Field {
    Sql,
    Text,
}

impl Field {
    fn max(self) -> usize {
        match self {
            Self::Sql => MAX_AUDIT_SQL_BYTES,
            Self::Text => MAX_AUDIT_TEXT_BYTES,
        }
    }
}

/// Replaces every literal, quoted identifier and comment of `text` with `?` / nothing. The
/// statement shape, the bare identifiers and the placeholders stay, so the audit still says what
/// was run. Returns `None` if a quote or comment is not closed: where the literals end is then
/// a guess, and a guess can leave a value outside the marks.
fn mask_literals(dialect: Dialect, text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for t in tokenize(dialect, text) {
        let (mark, closed) = match t.kind {
            TokenKind::Str { terminated }
            | TokenKind::DollarStr { terminated }
            | TokenKind::QuotedIdent { terminated } => ("?", terminated),
            TokenKind::Number => ("?", true),
            TokenKind::LineComment => ("", true),
            TokenKind::BlockComment { terminated } | TokenKind::ExecComment { terminated } => {
                ("", terminated)
            }
            _ => continue,
        };
        if !closed {
            return None;
        }
        out.push_str(&text[at..t.start]);
        out.push_str(mark);
        at = t.end;
    }
    out.push_str(&text[at..]);
    Some(out)
}

/// Masks the literals of `text` for the dialect, or for every dialect when it is not known (the
/// passes then agree on what a literal is, or the text is refused). `None` if any pass finds an
/// unclosed quote.
fn mask_for(dialect: Option<Dialect>, text: &str) -> Option<String> {
    let dialects: &[Dialect] = match dialect {
        Some(d) => &[d],
        None => &Dialect::ALL,
    };
    let mut cur = text.to_owned();
    for d in dialects {
        // Each pass runs on the original too, so a dialect that reads a quote differently
        // cannot hide an unclosed one behind another's marks.
        mask_literals(*d, text)?;
        cur = mask_literals(*d, &cur)?;
    }
    Some(cur)
}

/// The one scrub every caller-supplied text of an entry goes through: cut, masked when it
/// mentions a listed word, credential-redacted, cut again. Returns the text and whether it was
/// cut.
///
/// The word check runs on the whole text (not only the kept part), both as SQL of the dialect
/// and as plain words, so a literal after a listed word never survives, whatever the text looks
/// like (a statement, a JSON fragment, free text).
fn scrub_text(raw: &str, rules: SqlRules<'_>, field: Field) -> (String, bool) {
    let max = field.max();
    let (kept, mut truncated) = cut(raw, max);
    let tainted = rules.rules.statement_tainted(rules.dialect, raw)
        || rules.rules.statement_tainted(None, raw);
    let masked;
    let kept = if tainted {
        // Only SQL has literals that can be told apart from the rest. Any other text that
        // mentions a listed word is not kept, and neither is SQL whose quotes do not close:
        // where its literals end is then a guess, and a guess can leave a value unmarked.
        masked = match field {
            Field::Sql => mask_for(rules.dialect, kept),
            Field::Text => None,
        }
        .unwrap_or_else(|| REDACTED.to_owned());
        masked.as_str()
    } else {
        kept
    };
    let redacted = scrub(kept);
    let (done, cut_again) = cut(&redacted, max);
    truncated |= cut_again;
    (done.to_owned(), truncated)
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
                let field = if key == Some("sql") {
                    Field::Sql
                } else {
                    Field::Text
                };
                let (text, cut) = scrub_text(s, self.rules, field);
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

    /// A key is a name. A plain identifier is kept as it is, even a listed one (its value is
    /// replaced, and the audit says which); anything else is text and is scrubbed.
    fn key(&self, key: &str) -> (String, bool) {
        let plain = key.len() <= 128
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if plain {
            (key.to_owned(), false)
        } else {
            scrub_text(key, self.rules, Field::Text)
        }
    }

    fn object(&mut self, map: &Map<String, Value>) -> Map<String, Value> {
        map.iter()
            .map(|(k, v)| {
                let (name, cut) = self.key(k);
                self.truncated |= cut;
                (name, self.value(Some(k), v))
            })
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
    let (text, truncated) = scrub_text(raw, rules, Field::Text);
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
        let text = |t: &str| scrub_text(t, rules, Field::Text).0;
        let (arguments, arguments_truncated) = arguments(d.raw_arguments, rules);
        let outcomes = approvals(&d.statements, d.error.as_ref());
        let total = d.statements.len();
        let statements: Vec<AuditStatement> = d
            .statements
            .iter()
            .zip(&outcomes)
            .take(MAX_AUDIT_STATEMENTS)
            .map(|(s, &approval)| {
                let (sql, sql_truncated) = scrub_text(s.sql, rules, Field::Sql);
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
            tool: text(d.tool),
            connection: d
                .connection
                .map(|(id, name)| AuditConnection::new(text(id), text(name))),
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
                .map(|e| AuditFailure::new(text(e.code), text(e.message))),
            result_bytes: u64::try_from(d.result_bytes).unwrap_or(u64::MAX),
            rows,
            duration_ms: u64::try_from(d.duration.as_millis()).unwrap_or(u64::MAX),
            redaction: d.redaction,
        }
    }
}
