// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Errors of the command registry.

use thiserror::Error;

/// Error returned by command ID validation and registration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CommandError {
    /// The ID does not match `category:action`.
    #[error("invalid command id: {0}")]
    InvalidId(String),
    /// A command with this ID is already registered.
    #[error("command already registered: {0}")]
    Duplicate(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings() {
        assert_eq!(
            CommandError::InvalidId("x".into()).to_string(),
            "invalid command id: x"
        );
        assert_eq!(
            CommandError::Duplicate("x".into()).to_string(),
            "command already registered: x"
        );
    }
}
