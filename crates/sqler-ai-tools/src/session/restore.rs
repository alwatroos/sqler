// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Turning transcript entries into the plain records of the chat history store and back, and
//! seeding a new session with a restored chat (ADR-0077).
//!
//! The store keeps no exact SQL: a restored [`SqlEntry`] has an empty `sql`, so it cannot be
//! re-run ([`SqlEntry::can_rerun`]). Only the user and assistant text goes back to the model as
//! history; no tool result and no provider state is resumed.

use std::time::Duration;

use sqler_ai_api::{Message, Role};
use sqler_config::{StoredConversation, StoredEntry, StoredNotice, StoredStatus};
use sqler_sql::StatementClass;

use crate::session::LimitKind;
use crate::session::transcript::{
    MAX_TRANSCRIPT_ENTRIES, NotRunReason, Notice, SqlEntry, ToolCallEntry, ToolCallStatus,
    TranscriptEntry,
};
use crate::toolset::Truncation;

/// The most bytes of restored text sent to the model as history.
pub const PRIOR_CONTEXT_MAX_BYTES: usize = 32 * 1024;

/// Put in front of the first restored user message.
const RESTORED_NOTE: &str = "Restored chat: earlier tool results are not sent to the model again.";

/// A restored chat, ready for [`ExplorationSession::with_prior`](crate::session::ExplorationSession::with_prior).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PriorChat {
    /// The entries, oldest first (at most [`MAX_TRANSCRIPT_ENTRIES`]).
    pub entries: Vec<TranscriptEntry>,
    /// The stable number of `entries[0]`.
    pub first_seq: usize,
}

impl PriorChat {
    /// A restored chat from its parts.
    #[must_use]
    pub fn new(entries: Vec<TranscriptEntry>, first_seq: usize) -> Self {
        Self { entries, first_seq }
    }
}

fn limit_name(kind: LimitKind) -> &'static str {
    match kind {
        LimitKind::ModelSteps => "model_steps",
        LimitKind::ToolCalls => "tool_calls",
        LimitKind::Tokens => "tokens",
        LimitKind::Time => "time",
    }
}

fn limit_from(name: &str) -> Option<LimitKind> {
    match name {
        "model_steps" => Some(LimitKind::ModelSteps),
        "tool_calls" => Some(LimitKind::ToolCalls),
        "tokens" => Some(LimitKind::Tokens),
        "time" => Some(LimitKind::Time),
        _ => None,
    }
}

fn reason_name(reason: NotRunReason) -> String {
    match reason {
        NotRunReason::Limit(kind) => format!("limit:{}", limit_name(kind)),
        NotRunReason::Repeated => "repeated".to_owned(),
        NotRunReason::Cancelled => "cancelled".to_owned(),
    }
}

fn reason_from(name: &str) -> NotRunReason {
    match name {
        "repeated" => NotRunReason::Repeated,
        other => other
            .strip_prefix("limit:")
            .and_then(limit_from)
            .map_or(NotRunReason::Cancelled, NotRunReason::Limit),
    }
}

fn class_from(name: &str) -> StatementClass {
    match name {
        "read" => StatementClass::Read,
        "other" => StatementClass::Other,
        "write" => StatementClass::Write,
        "ddl" => StatementClass::Ddl,
        _ => StatementClass::Unknown,
    }
}

fn truncation_from(name: &str) -> Option<Truncation> {
    match name {
        "row_limit" => Some(Truncation::RowLimit),
        "time_limit" => Some(Truncation::TimeLimit),
        _ => None,
    }
}

/// The record to store for `entry`. The exact SQL of a statement is never part of it.
#[must_use]
pub fn stored_entry(entry: &TranscriptEntry) -> Option<StoredEntry> {
    Some(match entry {
        TranscriptEntry::User { text } => StoredEntry::User { text: text.clone() },
        TranscriptEntry::Assistant { text } => StoredEntry::Assistant { text: text.clone() },
        TranscriptEntry::ToolCall(call) => StoredEntry::ToolCall {
            call_id: call.call_id.clone(),
            tool: call.tool.clone(),
            arguments: call.arguments.clone(),
            status: match &call.status {
                ToolCallStatus::Ok => StoredStatus::Ok,
                ToolCallStatus::Error { code, message } => StoredStatus::Error {
                    code: code.clone(),
                    message: message.clone(),
                },
                ToolCallStatus::NotRun(reason) => StoredStatus::NotRun {
                    reason: reason_name(*reason),
                },
                ToolCallStatus::Cancelled => StoredStatus::Cancelled,
            },
            result: call.result.clone(),
        },
        TranscriptEntry::Sql(sql) => StoredEntry::Sql {
            call_id: sql.call_id.clone(),
            connection: sql.connection.clone(),
            display_sql: sql.display_sql.clone(),
            class: sql.class.as_str().to_owned(),
            approved: sql.approved,
            rows: sql.rows,
            truncated: sql.truncated.map(|t| t.as_str().to_owned()),
            failed: sql.failed,
        },
        TranscriptEntry::Notice(notice) => StoredEntry::Notice {
            notice: match notice {
                Notice::Retrying { attempt, delay } => StoredNotice::Retrying {
                    attempt: *attempt,
                    delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                },
                Notice::LimitReached(kind) => StoredNotice::LimitReached {
                    limit: limit_name(*kind).to_owned(),
                },
                Notice::LoopDetected { tool } => StoredNotice::LoopDetected { tool: tool.clone() },
                Notice::Cancelled => StoredNotice::Cancelled,
                Notice::ProviderError { message } => StoredNotice::ProviderError {
                    message: message.clone(),
                },
                Notice::OutputTruncated => StoredNotice::OutputTruncated,
            },
        },
    })
}

/// The transcript entry for a stored record, or `None` for a record this version does not know.
/// A restored statement has no exact SQL and an unknown class is [`StatementClass::Unknown`].
#[must_use]
pub fn restored_entry(stored: StoredEntry) -> Option<TranscriptEntry> {
    Some(match stored {
        StoredEntry::User { text } => TranscriptEntry::User { text },
        StoredEntry::Assistant { text } => TranscriptEntry::Assistant { text },
        StoredEntry::ToolCall {
            call_id,
            tool,
            arguments,
            status,
            result,
        } => TranscriptEntry::ToolCall(ToolCallEntry {
            call_id,
            tool,
            arguments,
            status: match status {
                StoredStatus::Ok => ToolCallStatus::Ok,
                StoredStatus::Error { code, message } => ToolCallStatus::Error { code, message },
                StoredStatus::NotRun { reason } => ToolCallStatus::NotRun(reason_from(&reason)),
                StoredStatus::Cancelled => ToolCallStatus::Cancelled,
                _ => return None,
            },
            result,
        }),
        StoredEntry::Sql {
            call_id,
            connection,
            display_sql,
            class,
            approved,
            rows,
            truncated,
            failed,
        } => TranscriptEntry::Sql(SqlEntry {
            call_id,
            connection,
            sql: String::new(),
            display_sql,
            class: class_from(&class),
            approved,
            rows,
            truncated: truncated.as_deref().and_then(truncation_from),
            failed,
        }),
        StoredEntry::Notice { notice } => TranscriptEntry::Notice(match notice {
            StoredNotice::Retrying { attempt, delay_ms } => Notice::Retrying {
                attempt,
                delay: Duration::from_millis(delay_ms),
            },
            StoredNotice::LimitReached { limit } => Notice::LimitReached(limit_from(&limit)?),
            StoredNotice::LoopDetected { tool } => Notice::LoopDetected { tool },
            StoredNotice::Cancelled => Notice::Cancelled,
            StoredNotice::ProviderError { message } => Notice::ProviderError { message },
            StoredNotice::OutputTruncated => Notice::OutputTruncated,
            _ => return None,
        }),
        _ => return None,
    })
}

/// A stored conversation as a [`PriorChat`]: the records this version knows, the last
/// [`MAX_TRANSCRIPT_ENTRIES`] of them.
#[must_use]
pub fn prior_chat(conversation: StoredConversation) -> PriorChat {
    let mut first_seq = usize::try_from(conversation.first_seq).unwrap_or(usize::MAX);
    let mut entries: Vec<TranscriptEntry> = conversation
        .entries
        .into_iter()
        .filter_map(restored_entry)
        .collect();
    if entries.len() > MAX_TRANSCRIPT_ENTRIES {
        let excess = entries.len() - MAX_TRANSCRIPT_ENTRIES;
        entries.drain(..excess);
        first_seq = first_seq.saturating_add(excess);
    }
    PriorChat { entries, first_seq }
}

/// Appends `text` to `messages` as `role`, merging into the last message of the same role.
fn push_merged(messages: &mut Vec<(Role, String)>, role: Role, text: String) {
    match messages.last_mut() {
        Some((last, existing)) if *last == role => {
            existing.push_str("\n\n");
            existing.push_str(&text);
        }
        _ => messages.push((role, text)),
    }
}

fn cut_to(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The history a restored chat gives the model: only the user and assistant text. Consecutive
/// user texts merge, as for a live session. An assistant message starts with the SQL its turn
/// ran ("SQL run earlier:" lines, the display text). The most recent messages within
/// [`PRIOR_CONTEXT_MAX_BYTES`] are kept, the list starts with a user message, and that message
/// starts with a note that earlier tool results are not sent again.
pub(crate) fn prior_history(entries: &[TranscriptEntry]) -> Vec<Message> {
    let mut messages: Vec<(Role, String)> = Vec::new();
    let mut sql_lines: Vec<&str> = Vec::new();
    for entry in entries {
        match entry {
            TranscriptEntry::User { text } => {
                sql_lines.clear();
                push_merged(&mut messages, Role::User, text.clone());
            }
            TranscriptEntry::Sql(sql) => sql_lines.push(&sql.display_sql),
            TranscriptEntry::Assistant { text } => {
                let full = if sql_lines.is_empty() {
                    text.clone()
                } else {
                    format!("SQL run earlier:\n{}\n\n{text}", sql_lines.join("\n"))
                };
                sql_lines.clear();
                push_merged(&mut messages, Role::Assistant, full);
            }
            _ => {}
        }
    }
    // The most recent messages that fit.
    let mut used = 0usize;
    let mut start = messages.len();
    while start > 0 {
        let len = messages[start - 1].1.len();
        if used + len > PRIOR_CONTEXT_MAX_BYTES && start < messages.len() {
            break;
        }
        used += len;
        start -= 1;
    }
    let mut kept: Vec<(Role, String)> = messages.split_off(start);
    if let Some((_, only)) = kept.last_mut() {
        // A single message over the cap (it was the newest one) is cut.
        if only.len() > PRIOR_CONTEXT_MAX_BYTES {
            *only = cut_to(only, PRIOR_CONTEXT_MAX_BYTES).to_owned();
        }
    }
    let first_user = kept.iter().position(|(role, _)| *role == Role::User);
    let Some(first_user) = first_user else {
        return Vec::new();
    };
    kept.drain(..first_user);
    if let Some((_, text)) = kept.first_mut() {
        *text = format!("{RESTORED_NOTE}\n\n{text}");
    }
    kept.into_iter()
        .map(|(role, text)| match role {
            Role::Assistant => Message::assistant(text),
            _ => Message::user(text),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn sql(display: &str) -> TranscriptEntry {
        TranscriptEntry::Sql(SqlEntry {
            call_id: "c1".into(),
            connection: "db".into(),
            sql: "select  1 -- exact".into(),
            display_sql: display.into(),
            class: StatementClass::Write,
            approved: true,
            rows: 7,
            truncated: Some(Truncation::TimeLimit),
            failed: true,
        })
    }

    fn all_entries() -> Vec<TranscriptEntry> {
        let call = |status| {
            TranscriptEntry::ToolCall(ToolCallEntry {
                call_id: "c1".into(),
                tool: "run_query".into(),
                arguments: json!({"sql": "select 1"}),
                status,
                result: json!({"rows": [[1]]}),
            })
        };
        vec![
            TranscriptEntry::User { text: "q".into() },
            TranscriptEntry::Assistant { text: "a".into() },
            call(ToolCallStatus::Ok),
            call(ToolCallStatus::Error {
                code: "denied".into(),
                message: "no".into(),
            }),
            call(ToolCallStatus::NotRun(NotRunReason::Limit(LimitKind::Time))),
            call(ToolCallStatus::NotRun(NotRunReason::Repeated)),
            call(ToolCallStatus::NotRun(NotRunReason::Cancelled)),
            call(ToolCallStatus::Cancelled),
            sql("update t set a = 1"),
            TranscriptEntry::Notice(Notice::Retrying {
                attempt: 2,
                delay: Duration::from_millis(1500),
            }),
            TranscriptEntry::Notice(Notice::LimitReached(LimitKind::ModelSteps)),
            TranscriptEntry::Notice(Notice::LoopDetected { tool: "t".into() }),
            TranscriptEntry::Notice(Notice::Cancelled),
            TranscriptEntry::Notice(Notice::ProviderError {
                message: "m".into(),
            }),
            TranscriptEntry::Notice(Notice::OutputTruncated),
        ]
    }

    #[test]
    fn every_entry_round_trips_except_the_exact_sql() {
        for entry in all_entries() {
            let stored = stored_entry(&entry).unwrap();
            let back = restored_entry(stored).unwrap();
            match (&entry, &back) {
                (TranscriptEntry::Sql(a), TranscriptEntry::Sql(b)) => {
                    assert_eq!(b.sql, "");
                    assert_eq!(
                        (
                            &a.call_id,
                            &a.connection,
                            &a.display_sql,
                            a.class,
                            a.approved
                        ),
                        (
                            &b.call_id,
                            &b.connection,
                            &b.display_sql,
                            b.class,
                            b.approved
                        )
                    );
                    assert_eq!(
                        (a.rows, a.truncated, a.failed),
                        (b.rows, b.truncated, b.failed)
                    );
                }
                _ => assert_eq!(entry, back),
            }
        }
    }

    #[test]
    fn the_exact_sql_is_not_in_the_stored_json() {
        let stored = stored_entry(&sql("update t set a = 1")).unwrap();
        let text = serde_json::to_string(&stored).unwrap();
        assert!(!text.contains("exact"), "{text}");
        assert!(text.contains("update t set a = 1"));
    }

    #[test]
    fn a_restored_statement_cannot_be_re_run() {
        let TranscriptEntry::Sql(live) = sql("x") else {
            panic!()
        };
        assert!(live.can_rerun());
        let TranscriptEntry::Sql(restored) =
            restored_entry(stored_entry(&TranscriptEntry::Sql(live)).unwrap()).unwrap()
        else {
            panic!()
        };
        assert!(!restored.can_rerun());
    }

    #[test]
    fn unknown_values_degrade() {
        let stored = StoredEntry::Sql {
            call_id: "c".into(),
            connection: "db".into(),
            display_sql: "x".into(),
            class: "from_the_future".into(),
            approved: false,
            rows: 0,
            truncated: Some("who_knows".into()),
            failed: false,
        };
        let Some(TranscriptEntry::Sql(sql)) = restored_entry(stored) else {
            panic!()
        };
        assert_eq!(sql.class, StatementClass::Unknown);
        assert_eq!(sql.truncated, None);
        assert!(
            restored_entry(StoredEntry::Notice {
                notice: StoredNotice::LimitReached {
                    limit: "nope".into()
                }
            })
            .is_none()
        );
    }

    fn texts(messages: &[Message]) -> Vec<(Role, String)> {
        messages.iter().map(|m| (m.role(), m.text())).collect()
    }

    #[test]
    fn history_keeps_text_merges_users_and_prefixes_sql() {
        let entries = vec![
            TranscriptEntry::User { text: "q1".into() },
            TranscriptEntry::User { text: "q1b".into() },
            TranscriptEntry::Assistant {
                text: "looking".into(),
            },
            TranscriptEntry::ToolCall(ToolCallEntry {
                call_id: "c".into(),
                tool: "run_query".into(),
                arguments: json!({"secret": 1}),
                status: ToolCallStatus::Ok,
                result: json!({"rows": [[42]]}),
            }),
            sql("select 1"),
            sql("select 2"),
            TranscriptEntry::Assistant {
                text: "done".into(),
            },
            TranscriptEntry::Notice(Notice::Cancelled),
            TranscriptEntry::User { text: "q2".into() },
        ];
        let history = texts(&prior_history(&entries));
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].0, Role::User);
        assert_eq!(history[0].1, format!("{RESTORED_NOTE}\n\nq1\n\nq1b"));
        assert_eq!(history[1].0, Role::Assistant);
        assert_eq!(
            history[1].1,
            "looking\n\nSQL run earlier:\nselect 1\nselect 2\n\ndone"
        );
        assert_eq!(history[2], (Role::User, "q2".to_owned()));
        assert!(!history.iter().any(|(_, t)| t.contains("42")));
    }

    #[test]
    fn history_is_capped_to_the_newest_messages_and_starts_with_a_user() {
        let big = "x".repeat(10 * 1024);
        let mut entries = Vec::new();
        for i in 0..6 {
            entries.push(TranscriptEntry::User {
                text: format!("q{i} {big}"),
            });
            entries.push(TranscriptEntry::Assistant {
                text: format!("a{i} {big}"),
            });
        }
        let history = texts(&prior_history(&entries));
        let total: usize = history.iter().map(|(_, t)| t.len()).sum();
        assert!(
            total <= PRIOR_CONTEXT_MAX_BYTES + RESTORED_NOTE.len() + 2,
            "{total}"
        );
        assert_eq!(history[0].0, Role::User);
        assert!(history[0].1.starts_with(RESTORED_NOTE));
        assert!(history.last().unwrap().1.starts_with("a5 "));
        assert!(!history.iter().any(|(_, t)| t.contains("q0 ")));
    }

    #[test]
    fn a_chat_without_a_user_message_gives_no_history() {
        let entries = vec![TranscriptEntry::Assistant { text: "a".into() }];
        assert_eq!(prior_history(&entries).len(), 0);
        assert_eq!(prior_history(&[]).len(), 0);
    }

    #[test]
    fn prior_chat_keeps_the_last_entries_and_numbers_them() {
        let entries: Vec<_> = (0..MAX_TRANSCRIPT_ENTRIES + 10)
            .map(|i| StoredEntry::Assistant {
                text: i.to_string(),
            })
            .collect();
        let prior = prior_chat_from(entries, 100);
        assert_eq!(prior.entries.len(), MAX_TRANSCRIPT_ENTRIES);
        assert_eq!(prior.first_seq, 110);
        assert_eq!(
            prior.entries[0],
            TranscriptEntry::Assistant { text: "10".into() }
        );
    }

    /// Goes through a real store, because a `StoredConversation` cannot be built outside
    /// `sqler-config`.
    fn prior_chat_from(entries: Vec<StoredEntry>, first_seq: u64) -> PriorChat {
        let dir =
            std::env::temp_dir().join(format!("sqler-ai-tools-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = sqler_config::ChatHistory::open(dir.join("chat-history.db"));
        let id = sqler_config::ChatHistory::new_id();
        let numbered = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| (first_seq + i as u64, e))
            .collect();
        store.append(&id, "db", numbered);
        let loaded = store.load(&id).unwrap().unwrap();
        store.close(Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
        prior_chat(loaded)
    }
}
