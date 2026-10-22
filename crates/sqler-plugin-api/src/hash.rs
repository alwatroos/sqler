// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The manifest hash (ADR-0056).

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use thiserror::Error;
use toml::Value;

/// The text is not 64 lowercase hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("not a manifest hash (expected 64 lowercase hex characters)")]
pub struct HashParseError;

/// SHA-256 of `b"sqler-manifest-v1\0"` and a canonical encoding of the parsed document.
///
/// Comments, whitespace, CRLF, quoting style, key order and `1_000` against `1000` do not change it; array order does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ManifestHash([u8; 32]);

impl ManifestHash {
    /// The raw digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Wraps a raw digest.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl fmt::Display for ManifestHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(64);
        for b in self.0 {
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
        f.write_str(&out)
    }
}

impl fmt::Debug for ManifestHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ManifestHash({self})")
    }
}

impl FromStr for ManifestHash {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, HashParseError> {
        let b = s.as_bytes();
        if b.len() != 64 {
            return Err(HashParseError);
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(HashParseError),
        };
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = (nibble(b[2 * i])? << 4) | nibble(b[2 * i + 1])?;
        }
        Ok(Self(out))
    }
}

/// SHA-256 of the bytes of a plugin's `.wasm` component.
///
/// The host (P6) computes it at load time and puts it in the [`GrantKey`](crate::GrantKey), so a component swapped under
/// an unchanged manifest asks the user again.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentDigest([u8; 32]);

impl ComponentDigest {
    /// The digest of the component bytes.
    #[must_use]
    pub fn of(component: &[u8]) -> Self {
        Self(Sha256::digest(component).into())
    }

    /// Wraps a raw digest.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ComponentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&ManifestHash(self.0), f)
    }
}

impl fmt::Debug for ComponentDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ComponentDigest({self})")
    }
}

impl FromStr for ComponentDigest {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, HashParseError> {
        s.parse::<ManifestHash>().map(|h| Self(h.0))
    }
}

fn enc_str(h: &mut Sha256, s: &str) {
    h.update(b"s");
    h.update((s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

/// Encodes a value. The recursion depth is bounded by the nesting check that runs before hashing.
fn enc(h: &mut Sha256, v: &Value) {
    match v {
        Value::String(s) => enc_str(h, s),
        Value::Integer(i) => {
            h.update(b"i");
            h.update(i.to_be_bytes());
        }
        Value::Float(x) => {
            h.update(b"f");
            let bits = if x.is_nan() {
                0x7ff8_0000_0000_0000
            } else {
                x.to_bits()
            };
            h.update(bits.to_be_bytes());
        }
        Value::Boolean(b) => h.update([b'b', u8::from(*b)]),
        Value::Datetime(d) => {
            h.update(b"d");
            enc_str(h, &d.to_string());
        }
        Value::Array(a) => {
            h.update(b"a");
            h.update((a.len() as u64).to_be_bytes());
            for item in a {
                enc(h, item);
            }
        }
        Value::Table(t) => enc_table(h, t),
    }
}

fn enc_table(h: &mut Sha256, t: &toml::Table) {
    h.update(b"m");
    h.update((t.len() as u64).to_be_bytes());
    let mut entries: Vec<(&String, &Value)> = t.iter().collect();
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (k, v) in entries {
        enc_str(h, k);
        enc(h, v);
    }
}

pub(crate) fn hash_table(t: &toml::Table) -> ManifestHash {
    let mut h = Sha256::new();
    h.update(b"sqler-manifest-v1\0");
    enc_table(&mut h, t);
    ManifestHash(h.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(text: &str) -> ManifestHash {
        hash_table(&text.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn hex_round_trip() {
        let h = hash("a = 1");
        let text = h.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<ManifestHash>().unwrap(), h);
        for bad in ["", "zz", &text.to_uppercase(), &text[..63]] {
            assert!(bad.parse::<ManifestHash>().is_err(), "{bad}");
        }
    }

    #[test]
    fn component_digest() {
        let d = ComponentDigest::of(b"\0asm");
        let text = d.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<ComponentDigest>().unwrap(), d);
        assert_ne!(d, ComponentDigest::of(b"\0asn"));
        assert!(text.to_uppercase().parse::<ComponentDigest>().is_err());
    }

    #[test]
    fn insensitive_to_formatting() {
        let a = hash("a = 1000\nb = \"x\"\n# c\n[t]\nk = [1, 2]\n");
        let b = hash("b = 'x'\r\na = 1_000\r\n\r\n[t]\r\nk = [ 1,\r\n 2 ]\r\n");
        assert_eq!(a, b);
    }

    #[test]
    fn sensitive_to_content() {
        let a = hash("k = [1, 2]");
        assert_ne!(a, hash("k = [2, 1]"));
        assert_ne!(a, hash("k = [1, 2, 3]"));
        assert_ne!(hash("a = 1"), hash("a = 1.0"));
        assert_ne!(hash("a = \"1\""), hash("a = 1"));
        assert_ne!(hash("a = 1"), hash("b = 1"));
    }

    #[test]
    fn nan_is_canonical() {
        assert_eq!(hash("a = nan"), hash("a = -nan"));
    }
}
