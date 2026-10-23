// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `sqler.toml` manifest: validation into typed errors, hash and grant key.

mod error;
mod walk;

pub use error::ManifestError;

use crate::api::{ApiVersion, CompatError};
use crate::capability::CapabilitySet;
use crate::contribution::Contributions;
use crate::grant::{GrantKey, GrantKeyBuildError};
use crate::hash::{ComponentDigest, ManifestHash, hash_table};
use crate::id::PluginId;
use crate::limits::Limits;

/// The `[entry]` table: the component to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// A relative, `/`-separated `.wasm` path inside the plugin directory.
    pub component: String,
}

/// A validated manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// Plugin id.
    id: PluginId,
    /// Display name.
    name: String,
    /// Plugin version.
    version: semver::Version,
    /// Required ABI version.
    api: ApiVersion,
    /// Description.
    description: Option<String>,
    /// Authors.
    authors: Vec<String>,
    /// SPDX license text.
    license: Option<String>,
    /// Requested capabilities (default deny until approved).
    capabilities: CapabilitySet,
    /// The component; `None` for a declarative-only plugin.
    entry: Option<Entry>,
    /// Contributions.
    contributes: Contributions,
    hash: ManifestHash,
}

pub(crate) struct Fields {
    pub id: PluginId,
    pub name: String,
    pub version: semver::Version,
    pub api: ApiVersion,
    pub description: Option<String>,
    pub authors: Vec<String>,
    pub license: Option<String>,
    pub capabilities: CapabilitySet,
    pub entry: Option<Entry>,
    pub contributes: Contributions,
}

/// Counts bracket and brace nesting outside strings and comments, so a hostile input is refused before the parser
/// recurses. Malformed quoting only makes the count approximate; the parser reports the syntax error.
fn nesting_exceeds(text: &str, max: usize) -> bool {
    let b = text.as_bytes();
    let (mut i, mut depth) = (0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            q @ (b'"' | b'\'') => {
                let triple = b[i..].starts_with(&[q, q, q]);
                i += if triple { 3 } else { 1 };
                while i < b.len() {
                    if q == b'"' && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if triple {
                        if b[i..].starts_with(&[q, q, q]) {
                            i += 3;
                            break;
                        }
                    } else if b[i] == q || b[i] == b'\n' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    false
}

impl Manifest {
    /// Parses and validates manifest text. The first error wins.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let lim = Limits::DEFAULT.manifest;
        if text.len() > lim.max_bytes {
            return Err(ManifestError::TooLarge {
                len: text.len(),
                max: lim.max_bytes,
            });
        }
        // Table headers (`[[x]]`) add 2 per line but close again; inline nesting adds up.
        if nesting_exceeds(text, lim.max_depth) {
            return Err(ManifestError::TooDeep);
        }
        let table: toml::Table =
            text.parse()
                .map_err(|e: toml::de::Error| ManifestError::Syntax {
                    message: crate::id::truncate(e.message()),
                    line: e.span().map(|s| {
                        text.get(..s.start)
                            .map_or(0, |head| head.matches('\n').count())
                            + 1
                    }),
                })?;
        walk::check_depth(&toml::Value::Table(table.clone()), 0, lim.max_depth)?;
        let f = walk::walk_plugin(&table)?;
        Ok(Self {
            id: f.id,
            name: f.name,
            version: f.version,
            api: f.api,
            description: f.description,
            authors: f.authors,
            license: f.license,
            capabilities: f.capabilities,
            entry: f.entry,
            contributes: f.contributes,
            hash: hash_table(&table),
        })
    }

    /// Like [`Manifest::parse`], from bytes. The size is checked before UTF-8 validation.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        let max = Limits::DEFAULT.manifest.max_bytes;
        if bytes.len() > max {
            return Err(ManifestError::TooLarge {
                len: bytes.len(),
                max,
            });
        }
        Self::parse(std::str::from_utf8(bytes).map_err(|_| ManifestError::NotUtf8)?)
    }

    /// Plugin id.
    #[must_use]
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    /// Display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Plugin version.
    #[must_use]
    pub fn version(&self) -> &semver::Version {
        &self.version
    }

    /// Required ABI version.
    #[must_use]
    pub fn api(&self) -> ApiVersion {
        self.api
    }

    /// Description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Authors.
    #[must_use]
    pub fn authors(&self) -> &[String] {
        &self.authors
    }

    /// SPDX license text.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }

    /// Requested capabilities (default deny until approved).
    ///
    /// The fields are private, so a parsed manifest cannot be changed behind the hash it carries:
    ///
    /// ```compile_fail,E0616
    /// # use sqler_plugin_api::{Manifest, CapabilitySet};
    /// # fn f(mut m: Manifest) {
    /// m.capabilities = CapabilitySet::default();
    /// # }
    /// ```
    #[must_use]
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// The component; `None` for a declarative-only plugin.
    #[must_use]
    pub fn entry(&self) -> Option<&Entry> {
        self.entry.as_ref()
    }

    /// Contributions.
    #[must_use]
    pub fn contributes(&self) -> &Contributions {
        &self.contributes
    }

    /// The hash of the whole manifest document.
    #[must_use]
    pub fn hash(&self) -> &ManifestHash {
        &self.hash
    }

    /// The key under which the approval of this plugin is stored.
    ///
    /// A plugin with an `[entry]` needs the SHA-256 of its `.wasm` (computed by the host at load time, from the bytes
    /// it is about to instantiate), so a component swapped under an unchanged manifest gives a new key. A declarative
    /// plugin has no component. The combination is checked, so a key without a digest cannot be built for a wasm plugin:
    ///
    /// - `[entry]` and `Some(digest)`, or no `[entry]` and `None`: the key.
    /// - `[entry]` and `None`: [`GrantKeyBuildError::ComponentRequired`].
    /// - no `[entry]` and `Some(digest)`: [`GrantKeyBuildError::NoComponent`].
    pub fn grant_key(
        &self,
        component: Option<&ComponentDigest>,
    ) -> Result<GrantKey, GrantKeyBuildError> {
        match (self.entry.is_some(), component) {
            (true, None) => Err(GrantKeyBuildError::ComponentRequired),
            (false, Some(_)) => Err(GrantKeyBuildError::NoComponent),
            (_, component) => Ok(GrantKey::new(
                self.id.clone(),
                self.version.clone(),
                self.hash,
                component.copied(),
            )),
        }
    }

    /// Whether a host that provides `host` can run this plugin.
    pub fn check_api(&self, host: ApiVersion) -> Result<(), CompatError> {
        if self.api.host_supports(host) {
            Ok(())
        } else {
            Err(CompatError::Incompatible {
                required: self.api,
                host,
            })
        }
    }
}
