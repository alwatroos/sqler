// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The key under which a user's approval is stored (ADR-0056).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::capability::CapabilitySet;
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
///
/// The fields are private and there is no other constructor: outside this crate a key exists only as the result of
/// `Manifest::grant_key`, which checks that the component digest fits the manifest. A key cannot be built from text or
/// edited afterwards. To read what a stored key text says, parse it as a [`ParsedGrantKey`], which can be compared with a
/// key but is not accepted by [`GrantSource::approved`].
///
/// ```compile_fail,E0451
/// use sqler_plugin_api::{GrantKey, ManifestHash};
///
/// let _ = GrantKey {
///     id: "my-plugin".parse().unwrap(),
///     version: "1.0.0".parse().unwrap(),
///     hash: ManifestHash::from_bytes([0; 32]),
///     component: None,
/// };
/// ```
///
/// ```compile_fail,E0616
/// use sqler_plugin_api::{GrantKey, Manifest};
///
/// fn strip(m: &Manifest, mut key: GrantKey) {
///     key.component = None;
/// }
/// ```
///
/// ```compile_fail,E0277
/// use sqler_plugin_api::GrantKey;
///
/// // A key has no text constructor.
/// let _: GrantKey = "my-plugin@1.0.0#00".parse().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GrantKey {
    id: PluginId,
    version: semver::Version,
    hash: ManifestHash,
    component: Option<ComponentDigest>,
}

impl GrantKey {
    /// Only the manifest code builds a key.
    pub(crate) fn new(
        id: PluginId,
        version: semver::Version,
        hash: ManifestHash,
        component: Option<ComponentDigest>,
    ) -> Self {
        Self {
            id,
            version,
            hash,
            component,
        }
    }

    /// The plugin id.
    #[must_use]
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    /// The plugin version.
    #[must_use]
    pub fn version(&self) -> &semver::Version {
        &self.version
    }

    /// The hash of the whole manifest.
    #[must_use]
    pub fn hash(&self) -> &ManifestHash {
        &self.hash
    }

    /// SHA-256 of the `.wasm` computed by the host at load time; `None` only for a declarative plugin without `[entry]`.
    #[must_use]
    pub fn component(&self) -> Option<&ComponentDigest> {
        self.component.as_ref()
    }
}

/// A grant key read back from text (for example from `plugin-grants.toml`).
///
/// It is for comparison and display only: it compares equal to a [`GrantKey`] with the same text form, and no API that
/// looks up or enforces a grant accepts it. Only a key derived from a manifest does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedGrantKey(GrantKey);

impl ParsedGrantKey {
    /// The plugin id.
    #[must_use]
    pub fn id(&self) -> &PluginId {
        &self.0.id
    }

    /// The plugin version.
    #[must_use]
    pub fn version(&self) -> &semver::Version {
        &self.0.version
    }

    /// The hash of the whole manifest.
    #[must_use]
    pub fn hash(&self) -> &ManifestHash {
        &self.0.hash
    }

    /// The component digest, if the text had one.
    #[must_use]
    pub fn component(&self) -> Option<&ComponentDigest> {
        self.0.component.as_ref()
    }
}

impl PartialEq<GrantKey> for ParsedGrantKey {
    fn eq(&self, other: &GrantKey) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ParsedGrantKey> for GrantKey {
    fn eq(&self, other: &ParsedGrantKey) -> bool {
        *self == other.0
    }
}

impl fmt::Display for ParsedGrantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Where the host looks up what the user approved (ADR-0059).
pub trait GrantSource: Send + Sync {
    /// The capabilities approved under exactly `key`; `None` = nothing granted.
    fn approved(&self, key: &GrantKey) -> Option<CapabilitySet>;
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

impl FromStr for ParsedGrantKey {
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
        Ok(Self(GrantKey {
            id: id.parse().map_err(|_| GrantKeyError)?,
            version: version.parse().map_err(|_| GrantKeyError)?,
            hash: hash.parse().map_err(|_| GrantKeyError)?,
            component,
        }))
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
        assert_eq!(text.parse::<ParsedGrantKey>().unwrap(), key);
        assert_eq!(key, text.parse::<ParsedGrantKey>().unwrap());
        for bad in [
            "",
            "a@1.0.0",
            "a@1.0.0#zz",
            "x@1.0.0#",
            "@1.0.0#ab",
            "My@1.0.0#ab",
            "a@1.0.0#+",
        ] {
            assert!(bad.parse::<ParsedGrantKey>().is_err(), "{bad}");
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
        assert_eq!(text.parse::<ParsedGrantKey>().unwrap(), a);
        assert_eq!(
            key(None).to_string().parse::<ParsedGrantKey>().unwrap(),
            key(None)
        );
        let bad = format!("{text}+{}", ComponentDigest::of(b"x"));
        assert!(bad.parse::<ParsedGrantKey>().is_err());
        assert!(
            format!("{}+zz", key(None))
                .parse::<ParsedGrantKey>()
                .is_err()
        );
    }
}
