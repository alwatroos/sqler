// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The AI chat panel (ADR-0076): the texts, the labels and the SQL blocks of an answer. Plain
//! Rust, no `GPUI`; the panel body lives behind the `gui` feature.
//!
//! The panel reuses the AI session engine under the reserved tab id `TabId::CHAT_PANEL`. It is
//! strictly read-only and always capped, so nothing here ever offers to run a statement.

use sqler_ai_tools::session::ToolCallStatus;
use sqler_config::AiLimits;

use super::session::tool_status_text;

#[cfg(feature = "gui")]
pub(crate) mod history_view;
#[cfg(feature = "gui")]
pub(crate) mod view;

/// The title of the panel.
pub const CHAT_TITLE: &str = "AI Chat";
/// The first line of an empty chat.
pub const CHAT_HELLO: &str =
    "Ask about the schema or the data. This chat only reads; it never changes data.";
/// The note shown under a refused statement.
pub const CHAT_REFUSED_NOTE: &str =
    "Statements that change data are refused here. Open the SQL in an editor to run it yourself.";
/// The line shown when the chat belongs to another connection than the active one.
pub const CHAT_OTHER_CONNECTION: &str =
    "This chat is about {name}. Start a new chat for the active connection.";

/// The title of the saved chats list.
pub const HISTORY_TITLE: &str = "Chat history";
/// The list when no chat is saved.
pub const HISTORY_EMPTY: &str =
    "No saved chats yet. Chats are saved here as you talk to the model.";
/// The list when there is no configuration directory to keep chats in.
pub const HISTORY_OFF_NO_DIR: &str = "Chat history is off: there is no configuration directory.";
/// The list when `chat-history.db` was written by a newer `SQLer`.
pub const HISTORY_OFF_NEWER: &str =
    "Chat history is off: chat-history.db was written by a newer SQLer and is left as it is.";
/// The notice when an unreadable `chat-history.db` was moved aside.
pub const HISTORY_MOVED: &str =
    "chat-history.db could not be read; it was moved aside and a new history was started.";
/// The question of the "delete all" confirmation.
pub const HISTORY_CONFIRM: &str = "Delete all saved chats? This cannot be undone.";
/// The note shown at the top of a restored chat.
pub const RESTORED_NOTE: &str =
    "Restored chat: earlier tool results are not sent to the model again.";
/// The note shown instead of the exact SQL of a restored statement saved with hidden values.
pub const RESTORED_SQL_HIDDEN: &str =
    "This statement was saved with values hidden, so it cannot be opened or copied.";
/// The connection name of a saved chat whose connection was removed.
pub const REMOVED_CONNECTION: &str = "Removed connection";
/// The notice when something needs the connection of a restored chat that was removed.
pub const CHAT_CONNECTION_REMOVED: &str = "The connection of this chat no longer exists.";
/// The title of a saved chat that has no user message yet.
pub const HISTORY_UNTITLED: &str = "Untitled chat";
/// The note that replaces the composer of a restored chat that cannot continue.
pub const HISTORY_CANNOT_CONTINUE: &str = "This chat cannot be continued.";

/// One line of the saved chats list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatRow {
    /// The stored conversation.
    pub id: sqler_config::ConversationId,
    /// The title: the first message.
    pub title: String,
    /// The connection name and the age, for example `shop · 2 h ago`.
    pub line: String,
    /// Whether this is the chat on screen.
    pub current: bool,
}

/// The most SQL blocks of one answer the panel offers.
const MAX_BLOCKS: usize = 8;

/// The bodies of the fenced code blocks of `text` whose info string is empty or `sql` (any
/// case), trimmed. Empty bodies are skipped; at most 8 are returned.
#[must_use]
pub fn chat_sql_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut open: Option<(bool, Vec<&str>)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        match (&mut open, trimmed.strip_prefix("```")) {
            (None, Some(info)) => {
                let info = info.trim();
                open = Some((
                    info.is_empty() || info.eq_ignore_ascii_case("sql"),
                    Vec::new(),
                ));
            }
            (Some(_), Some(rest)) if rest.trim().is_empty() => {
                if let Some((wanted, body)) = open.take() {
                    push_block(&mut blocks, wanted, &body);
                }
            }
            (Some((_, body)), _) => body.push(line),
            (None, None) => {}
        }
    }
    blocks.truncate(MAX_BLOCKS);
    blocks
}

fn push_block(blocks: &mut Vec<String>, wanted: bool, body: &[&str]) {
    let sql = body.join("\n");
    let sql = sql.trim();
    if wanted && !sql.is_empty() {
        blocks.push(sql.to_owned());
    }
}

/// The cap line of the panel header. The caps are always on in the chat, whatever the
/// `unlimited` setting says.
#[must_use]
pub fn caps_label(limits: &AiLimits) -> String {
    format!(
        "Read-only \u{b7} up to {} rows \u{b7} {} s per query",
        limits.max_rows, limits.timeout_secs
    )
}

/// How a tool call ended, for the chat. A refusal of a write names the panel's rule instead of
/// the guard's wording.
#[must_use]
pub fn chat_tool_status_text(status: &ToolCallStatus) -> String {
    match status {
        ToolCallStatus::Error { code, .. } if code == "read_only" || code == "no_approver" => {
            "refused (read-only chat)".to_owned()
        }
        other => tool_status_text(other),
    }
}

/// [`CHAT_OTHER_CONNECTION`] for the connection `name`.
#[must_use]
pub fn other_connection_text(name: &str) -> String {
    CHAT_OTHER_CONNECTION.replace("{name}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_take_sql_and_untagged_fences_only() {
        let text = "Try:\n```sql\nselect 1;\n```\nand\n```\nselect 2\n```\n```python\nprint(1)\n```\n```SQL\n  select 3  \n```";
        assert_eq!(chat_sql_blocks(text), ["select 1;", "select 2", "select 3"]);
    }

    #[test]
    fn blocks_skip_empty_unclosed_and_cap_at_eight() {
        for text in ["```sql\n\n```", "no code here", "```sql\nselect 1"] {
            assert_eq!(chat_sql_blocks(text).len(), 0, "{text}");
        }
        let many = "```sql\nselect 1\n```\n".repeat(12);
        assert_eq!(chat_sql_blocks(&many).len(), 8);
    }

    #[test]
    fn blocks_keep_multiline_bodies() {
        let blocks = chat_sql_blocks("```sql\nselect a\nfrom t\n```");
        assert_eq!(blocks, ["select a\nfrom t"]);
    }

    #[test]
    fn caps_label_ignores_unlimited() {
        let mut limits = AiLimits::default();
        limits.max_rows = 50;
        limits.timeout_secs = 20;
        let capped = caps_label(&limits);
        assert_eq!(
            capped,
            "Read-only \u{b7} up to 50 rows \u{b7} 20 s per query"
        );
        limits.unlimited = true;
        assert_eq!(caps_label(&limits), capped);
    }

    #[test]
    fn refusals_use_the_chat_wording() {
        for code in ["read_only", "no_approver"] {
            let status = ToolCallStatus::Error {
                code: code.into(),
                message: "m".into(),
            };
            assert_eq!(chat_tool_status_text(&status), "refused (read-only chat)");
        }
        let other = ToolCallStatus::Error {
            code: "denied".into(),
            message: "m".into(),
        };
        assert_eq!(chat_tool_status_text(&other), "denied");
        assert_eq!(chat_tool_status_text(&ToolCallStatus::Ok), "done");
    }

    #[test]
    fn other_connection_names_the_chat_connection() {
        assert_eq!(
            other_connection_text("prod-db"),
            "This chat is about prod-db. Start a new chat for the active connection."
        );
    }
}
