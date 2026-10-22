// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Identifier newtypes with their grammar (ADR-0056).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Why an identifier was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IdError {
    /// The identifier is empty (or shorter than the minimum).
    #[error("identifier is empty or too short")]
    Empty,
    /// The identifier is longer than the maximum.
    #[error("identifier is too long")]
    TooLong,
    /// The identifier has a character outside its grammar.
    #[error("identifier has a character outside its grammar")]
    BadCharacter,
}

/// Cuts echoed input to 64 characters (errors never echo input in full).
pub(crate) fn truncate(s: &str) -> String {
    s.chars().take(64).collect()
}

/// `[a-z][a-z0-9]*(-[a-z0-9]+)*`, length `min..=64`.
fn check_kebab(s: &str, min: usize) -> Result<(), IdError> {
    if s.len() < min {
        return Err(IdError::Empty);
    }
    if s.len() > 64 {
        return Err(IdError::TooLong);
    }
    let b = s.as_bytes();
    if !b[0].is_ascii_lowercase() {
        return Err(IdError::BadCharacter);
    }
    let mut prev_dash = false;
    for &c in b {
        match c {
            b'a'..=b'z' | b'0'..=b'9' => prev_dash = false,
            b'-' if !prev_dash => prev_dash = true,
            _ => return Err(IdError::BadCharacter),
        }
    }
    if prev_dash {
        return Err(IdError::BadCharacter);
    }
    Ok(())
}

/// `[a-z][a-z0-9_<extra>]{0,max-1}`.
fn check_simple(s: &str, max: usize, extra: &[u8]) -> Result<(), IdError> {
    if s.is_empty() {
        return Err(IdError::Empty);
    }
    if s.len() > max {
        return Err(IdError::TooLong);
    }
    let b = s.as_bytes();
    if !b[0].is_ascii_lowercase() {
        return Err(IdError::BadCharacter);
    }
    if b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(c))
    {
        Ok(())
    } else {
        Err(IdError::BadCharacter)
    }
}

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident, $check:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The identifier text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, IdError> {
                let check: fn(&str) -> Result<(), IdError> = $check;
                check(s)?;
                Ok(Self(s.to_owned()))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_newtype!(
    /// A plugin id: `[a-z][a-z0-9]*(-[a-z0-9]+)*`, 2 to 64 characters.
    PluginId,
    |s| check_kebab(s, 2)
);
id_newtype!(
    /// A driver engine id: `[a-z][a-z0-9]*(-[a-z0-9]+)*`, 1 to 64 characters.
    EngineId,
    |s| check_kebab(s, 1)
);
id_newtype!(
    /// The id of a contribution (command, panel, element, ...): `[a-z][a-z0-9]*(-[a-z0-9]+)*`, 1 to 64 characters.
    ContributionId,
    |s| check_kebab(s, 1)
);
id_newtype!(
    /// An AI tool name: `[a-z][a-z0-9_]{0,47}`.
    ToolName,
    |s| check_simple(s, 48, b"_")
);
id_newtype!(
    /// A plugin setting key: `[a-z][a-z0-9_.-]{0,63}`.
    SettingKey,
    |s| check_simple(s, 64, b"_.-")
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_table() {
        for ok in ["ab", "a1", "a-b", "duck-db2", "a1b"] {
            assert!(ok.parse::<PluginId>().is_ok(), "{ok}");
        }
        for bad in ["a-", "-a", "a--b", "A", "1a", "a_b", "a b", "é", "a.b"] {
            assert!(bad.parse::<PluginId>().is_err(), "{bad}");
        }
        assert_eq!("".parse::<PluginId>(), Err(IdError::Empty));
        assert_eq!("a".parse::<PluginId>(), Err(IdError::Empty));
        assert!("a".parse::<EngineId>().is_ok());
        assert_eq!("a".repeat(65).parse::<EngineId>(), Err(IdError::TooLong));
        assert!("a".repeat(64).parse::<ContributionId>().is_ok());
    }

    #[test]
    fn tool_and_setting_names() {
        assert!("run_query".parse::<ToolName>().is_ok());
        assert!("run-query".parse::<ToolName>().is_err());
        assert_eq!("a".repeat(49).parse::<ToolName>(), Err(IdError::TooLong));
        assert!("a.b-c_d1".parse::<SettingKey>().is_ok());
        assert!(".a".parse::<SettingKey>().is_err());
        assert_eq!("".parse::<SettingKey>(), Err(IdError::Empty));
    }

    #[test]
    fn display_round_trips() {
        let id: PluginId = "my-plugin".parse().unwrap();
        assert_eq!(id.to_string(), "my-plugin");
        assert_eq!(id.as_str(), "my-plugin");
    }
}
