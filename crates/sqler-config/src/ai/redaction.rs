// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The list of column-name fragments whose values are never sent to an AI provider.

use super::error::AiConfigProblem;

/// Built-in redaction entries. They can be extended by the user, never removed.
pub const DEFAULT_REDACTED_COLUMNS: &[&str] =
    &["password", "passwd", "secret", "token", "api_key", "ssn"];

const MAX_ENTRY_CHARS: usize = 64;

/// The built-in entries plus the user's extra ones.
///
/// An entry is a substring of a column name, compared ignoring case and separators (`_`, `-`, spaces). There is no way to remove a default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RedactionList {
    extra: Vec<String>,
}

impl RedactionList {
    /// The defaults plus `extra`. Entries are trimmed and lowercased; duplicates of a default or
    /// of an earlier entry are dropped.
    ///
    /// # Errors
    /// [`AiConfigProblem::InvalidRedactionEntry`] for an empty entry, one longer than 64
    /// characters or one with control characters.
    pub fn new(extra: impl IntoIterator<Item = String>) -> Result<Self, AiConfigProblem> {
        let mut out: Vec<String> = Vec::new();
        for (index, entry) in extra.into_iter().enumerate() {
            let bad = |reason| AiConfigProblem::InvalidRedactionEntry { index, reason };
            let entry = entry.trim().to_lowercase();
            if entry.is_empty() {
                return Err(bad("entry must not be empty"));
            }
            if entry.chars().count() > MAX_ENTRY_CHARS {
                return Err(bad("entry is longer than 64 characters"));
            }
            if entry.chars().any(char::is_control) {
                return Err(bad("entry must not contain control characters"));
            }
            if DEFAULT_REDACTED_COLUMNS.contains(&entry.as_str()) || out.contains(&entry) {
                continue;
            }
            out.push(entry);
        }
        Ok(Self { extra: out })
    }

    /// The user's extra entries (normalised, without defaults).
    #[must_use]
    pub fn extra(&self) -> &[String] {
        &self.extra
    }

    /// Every entry, defaults first.
    pub fn entries(&self) -> impl Iterator<Item = &str> {
        DEFAULT_REDACTED_COLUMNS
            .iter()
            .copied()
            .chain(self.extra.iter().map(String::as_str))
    }

    /// Whether a column of this name must be redacted. The match is a substring match that ignores
    /// case and separators, so `apiKey`, `api-key` and `API_KEY` all match `api_key`.
    #[must_use]
    pub fn matches_column(&self, column: &str) -> bool {
        let column = fold(column);
        self.entries()
            .map(fold)
            .any(|e| !e.is_empty() && column.contains(&e))
    }
}

/// Lowercases and drops every non-alphanumeric character (`_`, `-`, spaces, dots, ...).
fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Result<RedactionList, AiConfigProblem> {
        RedactionList::new(items.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults_are_always_present() {
        let l = RedactionList::default();
        assert_eq!(l.extra().len(), 0);
        assert_eq!(l.entries().count(), DEFAULT_REDACTED_COLUMNS.len());
        for d in ["password", "ssn", "token"] {
            assert!(l.entries().any(|e| e == d));
        }
    }

    #[test]
    fn entries_are_normalised_and_deduplicated() {
        let l = list(&[" IBAN ", "iban", "Password", "Card_No"]).unwrap();
        assert_eq!(l.extra(), ["iban", "card_no"]);
        let all: Vec<&str> = l.entries().collect();
        assert_eq!(&all[..6], DEFAULT_REDACTED_COLUMNS);
        assert_eq!(&all[6..], ["iban", "card_no"]);
    }

    #[test]
    fn non_ascii_entries_match_case_insensitively() {
        let l = list(&["ÄBC"]).unwrap();
        assert!(l.matches_column("xÄbCx"));
        assert!(l.matches_column("äbc"));
    }

    #[test]
    fn matching_is_case_insensitive_substring() {
        let l = list(&["iban"]).unwrap();
        assert!(l.matches_column("USER_PASSWORD"));
        assert!(l.matches_column("customer_IBAN_code"));
        assert!(l.matches_column("Ssn"));
        assert!(!l.matches_column("name"));
    }

    #[test]
    fn separators_and_case_are_ignored() {
        let l = RedactionList::default();
        for c in [
            "apiKey",
            "api-key",
            "API_KEY",
            "ApiKey",
            "api key",
            "x.api_key.y",
        ] {
            assert!(l.matches_column(c), "{c}");
        }
        let l = list(&["card_no"]).unwrap();
        assert!(l.matches_column("cardNo"));
        assert!(!l.matches_column("api"));
    }

    #[test]
    fn separator_only_entry_matches_nothing() {
        let l = list(&["--"]).unwrap();
        assert!(!l.matches_column("name"));
    }

    #[test]
    fn bad_entries_are_rejected_with_their_index() {
        let long = "a".repeat(65);
        for (items, idx) in [
            (vec!["ok", " "], 1),
            (vec![""], 0),
            (vec!["a\nb"], 0),
            (vec!["ok", long.as_str()], 1),
        ] {
            assert!(
                matches!(
                    list(&items),
                    Err(AiConfigProblem::InvalidRedactionEntry { index, .. }) if index == idx
                ),
                "{items:?}"
            );
        }
        assert!(list(&[&"a".repeat(64)]).is_ok());
    }
}
