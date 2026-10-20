// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The validated command ID.

use std::fmt;

use super::error::CommandError;

/// A validated command ID of the form `category:action`.
///
/// Grammar, lowercase ASCII only: `seg(-seg)* : seg(-seg)*`, where a segment is `[a-z][a-z0-9]*`.
/// The action may be a single segment.
/// Examples: `query:run-selection`, `result-grid:copy-as-csv`, `app:quit`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommandId(String);

fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn is_valid(s: &str) -> bool {
    let Some((category, action)) = s.split_once(':') else {
        return false;
    };
    if action.contains(':') {
        return false;
    }
    category.split('-').all(is_segment) && action.split('-').all(is_segment)
}

impl CommandId {
    /// Validates and builds a command ID.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::InvalidId`] if `s` does not match `category:action`.
    pub fn new(s: &str) -> Result<Self, CommandError> {
        if is_valid(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(CommandError::InvalidId(s.to_owned()))
        }
    }

    /// The ID as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The category: the text before `:`.
    #[must_use]
    pub fn category(&self) -> &str {
        self.0.split_once(':').map_or("", |(c, _)| c)
    }

    /// The action: the text after `:`.
    #[must_use]
    pub fn action(&self) -> &str {
        self.0.split_once(':').map_or("", |(_, a)| a)
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ids() {
        for s in [
            "query:run-selection",
            "result-grid:copy-as-csv",
            "app:open-settings",
            "app:quit",
            "tab:close",
            "settings:open",
        ] {
            assert_eq!(CommandId::new(s).unwrap().as_str(), s);
        }
    }

    #[test]
    fn invalid_ids() {
        for s in [
            "",
            "query",
            "Query:run-all",
            "query:run_all",
            "query::run-all",
            "a:b:c-d",
            "query:-run",
            "query:run-",
            "query:run--all",
            "1query:run-all",
            "query:run-all ",
            ":run-all",
            "query:",
        ] {
            assert_eq!(
                CommandId::new(s),
                Err(CommandError::InvalidId(s.to_owned())),
                "{s:?}"
            );
        }
    }

    #[test]
    fn category_action_display() {
        let id = CommandId::new("result-grid:copy-as-csv").unwrap();
        assert_eq!(id.category(), "result-grid");
        assert_eq!(id.action(), "copy-as-csv");
        assert_eq!(id.to_string(), "result-grid:copy-as-csv");
    }

    #[test]
    fn ordering() {
        let a = CommandId::new("app:open-settings").unwrap();
        let b = CommandId::new("query:run-all").unwrap();
        assert!(a < b);
    }
}
