// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The per-editor-tab history of assist requests.

/// The kind of an assist request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnKind {
    /// A question turned into SQL.
    NlToSql,
    /// An explanation of SQL.
    Explain,
    /// A fix of failing SQL.
    Fix,
}

/// One request and its answer. Holds the user's text and the model's text, never a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Turn {
    /// What was asked for.
    pub kind: TurnKind,
    /// The question, or the SQL, as sent (credentials removed).
    pub request: String,
    /// The model's answer text.
    pub answer: String,
}

impl Turn {
    /// A turn.
    #[must_use]
    pub fn new(kind: TurnKind, request: impl Into<String>, answer: impl Into<String>) -> Self {
        Self {
            kind,
            request: request.into(),
            answer: answer.into(),
        }
    }
}

/// The history of one editor tab. The host keeps one per tab and passes it to every call; the
/// engine clears it when the connection changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conversation {
    connection: Option<String>,
    turns: Vec<Turn>,
}

impl Conversation {
    /// An empty conversation.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The turns, oldest first.
    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// The number of turns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Whether there are no turns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Forgets every turn.
    pub fn clear(&mut self) {
        self.turns.clear();
        self.connection = None;
    }

    /// Ties the conversation to `connection`, forgetting turns of another one.
    pub(crate) fn bind(&mut self, connection: &str) {
        if self.connection.as_deref() != Some(connection) {
            self.turns.clear();
            self.connection = Some(connection.to_owned());
        }
    }

    /// Adds a turn and drops the oldest beyond `max`.
    pub(crate) fn push(&mut self, turn: Turn, max: usize) {
        self.turns.push(turn);
        let extra = self.turns.len().saturating_sub(max);
        self.turns.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_clears_on_another_connection_and_push_caps() {
        let mut c = Conversation::new();
        assert!(c.is_empty());
        c.bind("a");
        for i in 0..4 {
            c.push(Turn::new(TurnKind::Fix, format!("q{i}"), "a"), 2);
        }
        assert_eq!(c.len(), 2);
        assert_eq!(c.turns()[0].request, "q2");
        c.bind("a");
        assert_eq!(c.len(), 2);
        c.bind("b");
        assert!(c.is_empty());
        c.push(Turn::new(TurnKind::Explain, "x", "y"), 0);
        assert!(c.is_empty());
        c.clear();
    }
}
