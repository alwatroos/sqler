// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The bearer token of the HTTP transport.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::error::McpError;

const MIN_LEN: usize = 32;
const MAX_LEN: usize = 512;

/// The shared secret a client presents as `Authorization: Bearer <token>`.
///
/// `Debug` never prints the text; the memory is wiped on drop.
#[derive(Clone)]
pub struct AuthToken(Zeroizing<String>);

impl AuthToken {
    /// A fresh token: 32 random bytes from the system generator, base64url without padding.
    ///
    /// # Errors
    ///
    /// `McpError::Random` when the system generator fails.
    pub fn generate() -> Result<Self, McpError> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        SystemRandom::new()
            .fill(&mut bytes[..])
            .map_err(|_| McpError::Random)?;
        Ok(Self(Zeroizing::new(URL_SAFE_NO_PAD.encode(&bytes[..]))))
    }

    /// Wraps a given token after trimming surrounding whitespace.
    ///
    /// # Errors
    ///
    /// `McpError::InvalidToken` unless the trimmed text is 32 to 512 visible ASCII characters.
    pub fn new(text: String) -> Result<Self, McpError> {
        let text = Zeroizing::new(text);
        let trimmed = text.trim();
        if trimmed.len() < MIN_LEN {
            return Err(McpError::InvalidToken("shorter than 32 characters"));
        }
        if trimmed.len() > MAX_LEN {
            return Err(McpError::InvalidToken("longer than 512 characters"));
        }
        if !trimmed.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(McpError::InvalidToken("not visible ASCII"));
        }
        Ok(Self(Zeroizing::new(trimmed.to_owned())))
    }

    /// The token text, for the one place that hands it to the user or a client.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Compares a presented token in constant time (for equal lengths; the length itself is not
    /// a secret worth hiding because tokens have a fixed shape).
    #[must_use]
    pub fn matches(&self, presented: &[u8]) -> bool {
        let own = self.0.as_bytes();
        let same_len = own.len() == presented.len();
        // Compare against a same-length view so the work does not depend on where bytes differ.
        let probe = if same_len { presented } else { own };
        let equal: bool = own.ct_eq(probe).into();
        same_len && equal
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthToken(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_43_url_safe_characters_and_unique() {
        let a = AuthToken::generate().unwrap();
        let b = AuthToken::generate().unwrap();
        assert_eq!(a.expose().len(), 43);
        assert!(
            a.expose()
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_ne!(a.expose(), b.expose());
        assert!(AuthToken::new(a.expose().to_owned()).is_ok());
    }

    #[test]
    fn new_trims_and_checks_bounds() {
        let ok = "a".repeat(32);
        assert_eq!(
            AuthToken::new(format!("  {ok}\n")).unwrap().expose(),
            ok.as_str()
        );
        assert!(matches!(
            AuthToken::new("a".repeat(31)),
            Err(McpError::InvalidToken(_))
        ));
        assert!(AuthToken::new("a".repeat(512)).is_ok());
        assert!(AuthToken::new("a".repeat(513)).is_err());
        assert!(AuthToken::new(format!("{}b c", "a".repeat(40))).is_err());
        assert!(AuthToken::new(format!("{}\u{e9}", "a".repeat(40))).is_err());
        assert!(AuthToken::new(String::new()).is_err());
    }

    #[test]
    fn matches_is_exact() {
        let t = AuthToken::new("k".repeat(40)).unwrap();
        assert!(t.matches("k".repeat(40).as_bytes()));
        assert!(!t.matches("k".repeat(39).as_bytes()));
        assert!(!t.matches("k".repeat(41).as_bytes()));
        assert!(!t.matches(format!("{}x", "k".repeat(39)).as_bytes()));
        assert!(!t.matches(b""));
    }

    #[test]
    fn debug_hides_the_text() {
        let t = AuthToken::new("secretsecretsecretsecretsecretsecret".into()).unwrap();
        let shown = format!("{t:?}");
        assert_eq!(shown, "AuthToken(***)");
        assert!(!shown.contains("secret"));
    }
}
