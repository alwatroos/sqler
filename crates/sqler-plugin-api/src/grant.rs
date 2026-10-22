// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The key under which a user's approval is stored (ADR-0056).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::hash::{ComponentDigest, ManifestHash};
use crate::id::PluginId;

/// The text is not `<id>@<version>#<hash>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("not a grant key (expected <id>@<version>#<64 hex characters>[+<64 hex characters>])")]
pub struct GrantKeyError;

/// The component digest does not fit the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GrantKeyBuildError {
    /// The manifest has an `[entry]`, so the key needs the digest of its `.wasm`.
    #[error("the plugin has an [entry]; a grant key needs the digest of its component")]
    ComponentRequired,
    /// The manifest has no `[entry]`, so there is no component to bind.
    #[error("the plugin has no [entry]; a grant key has no component digest")]
    NoComponent,
}

/// Plugin id, version, manifest hash and component digest. Any manifest change gives a new key, so any capability change
/// re-prompts; so does a swapped `.wasm` under an unchanged manifest.
///
/// Text form: `<id>@<version>#<manifest_hash>` for a declarative plugin (no `[entry]`), and
/// `<id>@<version>#<manifest_hash>+<component_hash>` for a plugin with a component.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GrantKey {
    /// The plugin id.
    pub id: PluginId,
    /// The plugin version.
    pub version: semver::Version,
    /// The hash of the whole manifest.
    pub hash: ManifestHash,
    /// SHA-256 of the `.wasm` computed by the host at load time; `None` only for a declarative plugin without `[entry]`.
    pub component: Option<ComponentDigest>,
}

impl fmt::Display for GrantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}#{}", self.id, self.version, self.hash)?;
        if let Some(c) = &self.component {
            write!(f, "+{c}")?;
        }
        Ok(())
    }
}

impl FromStr for GrantKey {
    type Err = GrantKeyError;

    fn from_str(s: &str) -> Result<Self, GrantKeyError> {
        if s.len() > 340 {
            return Err(GrantKeyError);
        }
        let (rest, tail) = s.rsplit_once('#').ok_or(GrantKeyError)?;
        let (hash, component) = match tail.split_once('+') {
            Some((h, c)) => (h, Some(c.parse().map_err(|_| GrantKeyError)?)),
            None => (tail, None),
        };
        let (id, version) = rest.split_once('@').ok_or(GrantKeyError)?;
        Ok(Self {
            id: id.parse().map_err(|_| GrantKeyError)?,
            version: version.parse().map_err(|_| GrantKeyError)?,
            hash: hash.parse().map_err(|_| GrantKeyError)?,
            component,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = GrantKey {
            id: "my-plugin".parse().unwrap(),
            version: "1.2.3-rc.1+build.5".parse().unwrap(),
            hash: ManifestHash::from_bytes([0xab; 32]),
            component: None,
        };
        let text = key.to_string();
        assert!(text.starts_with("my-plugin@1.2.3-rc.1+build.5#abab"));
        assert_eq!(text.parse::<GrantKey>().unwrap(), key);
        for bad in [
            "",
            "a@1.0.0",
            "a@1.0.0#zz",
            "x@1.0.0#",
            "@1.0.0#ab",
            "My@1.0.0#ab",
            "a@1.0.0#+",
        ] {
            assert!(bad.parse::<GrantKey>().is_err(), "{bad}");
        }
    }

    #[test]
    fn component_digest_is_part_of_the_key() {
        let hash = ManifestHash::from_bytes([0xab; 32]);
        let key = |c: Option<ComponentDigest>| GrantKey {
            id: "my-plugin".parse().unwrap(),
            version: "1.2.3+b.5".parse().unwrap(),
            hash,
            component: c,
        };
        let a = key(Some(ComponentDigest::of(b"one")));
        let b = key(Some(ComponentDigest::of(b"two")));
        assert_ne!(a, b);
        assert_ne!(a, key(None));
        let text = a.to_string();
        assert!(text.contains(&format!("#{hash}+")));
        assert_eq!(text.parse::<GrantKey>().unwrap(), a);
        assert_eq!(
            key(None).to_string().parse::<GrantKey>().unwrap(),
            key(None)
        );
        let bad = format!("{text}+{}", ComponentDigest::of(b"x"));
        assert!(bad.parse::<GrantKey>().is_err());
        assert!(format!("{}+zz", key(None)).parse::<GrantKey>().is_err());
    }
}
