// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Redaction of tool results before anything leaves the machine (ADR-0037).
//!
//! [`Redaction::apply`] walks the JSON body of a tool result and hides values by the shape of
//! the ADR-0036 results, not inside the tool renderers, so a new tool that emits `columns` and
//! `rows` is covered without doing anything. See [`Redaction::apply`] for the four rules.

mod shape;
mod walk;

use sqler_config::RedactionList;
use sqler_sql::{Dialect, TokenKind, tokenize};

/// The text that replaces a hidden value.
pub const REDACTED: &str = "<redacted>";

/// The most column names a [`RedactionSummary`] lists.
pub(crate) const MAX_SUMMARY_COLUMNS: usize = 64;

/// Applies a [`RedactionList`] to tool results.
#[derive(Debug, Clone, Default)]
pub struct Redaction {
    list: RedactionList,
}

/// Where a result comes from: decides rule R3 and the dialect used for rule R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RedactScope {
    /// Whether row values may reach the model. `false` empties every `rows` array.
    pub send_rows: bool,
    /// The dialect of the connection, if the driver has one.
    pub dialect: Option<Dialect>,
}

impl RedactScope {
    /// A scope.
    #[must_use]
    pub fn new(send_rows: bool, dialect: Option<Dialect>) -> Self {
        Self { send_rows, dialect }
    }
}

/// What a redaction pass hid. Holds names and counts, never values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RedactionSummary {
    /// Names of the columns that were hidden, deduplicated, sorted, at most 64.
    pub columns: Vec<String>,
    /// Statements whose every value was hidden because the SQL mentions a listed word.
    pub tainted_statements: u32,
    /// Whether any `rows` array was emptied because rows are not sent.
    pub rows_withheld: bool,
    /// Error texts that were replaced.
    pub errors_withheld: u32,
}

impl Redaction {
    /// A redaction over `list`.
    #[must_use]
    pub fn new(list: RedactionList) -> Self {
        Self { list }
    }

    /// The list in use.
    #[must_use]
    pub fn list(&self) -> &RedactionList {
        &self.list
    }

    /// Whether values of a column of this name are hidden.
    #[must_use]
    pub fn column_redacted(&self, name: &str) -> bool {
        self.list.matches_column(name)
    }

    /// Whether `sql` mentions a listed word, so none of its values may be sent.
    ///
    /// With a dialect, the words, quoted identifiers and executable comments of the statement
    /// are checked (string literals are not). Without one, the whole text is matched. This is
    /// what stops `select password as p`.
    #[must_use]
    pub fn statement_tainted(&self, dialect: Option<Dialect>, sql: &str) -> bool {
        let Some(dialect) = dialect else {
            return self.list.matches_column(sql);
        };
        tokenize(dialect, sql).iter().any(|t| match t.kind {
            TokenKind::Word | TokenKind::ExecComment { .. } => {
                self.list.matches_column(t.text(sql))
            }
            TokenKind::QuotedIdent { .. } => self
                .list
                .matches_column(t.text(sql).trim_matches(['"', '`', '[', ']'])),
            _ => false,
        })
    }

    /// Hides values in `value`, a tool result body, and says what it hid.
    ///
    /// Every JSON object is visited, recursively (cells inside `rows` are not):
    /// - **R1**: an object with `columns: [{name, ..}]` marks each column whose name is listed
    ///   with `redacted: true`, replaces a non-null `default` with [`REDACTED`], and replaces
    ///   that column's cell in every row of `rows` (nulls too).
    /// - **R2**: an object with a string `sql` and a `rows` array, where the SQL is tainted, has
    ///   every cell replaced and every column marked.
    /// - **R3**: with `send_rows` off, every `rows` array becomes `[]`.
    /// - **R4**: a statement-level `error` string, and `error.message` of a `driver` error, is
    ///   replaced when rows are not sent, or when a word of it matches the list.
    ///
    /// A statement object (one with `rows`) that a rule touched gets a `redaction` object. The
    /// pass is idempotent on the JSON.
    pub fn apply(&self, value: &mut serde_json::Value, scope: RedactScope) -> RedactionSummary {
        walk::run(self, value, scope)
    }
}

#[cfg(test)]
mod tests;
