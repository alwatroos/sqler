// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The envelope (the signed asset) and the manifest (its payload).

use std::collections::BTreeMap;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::UpdateError;

/// The only envelope `format` this crate reads.
pub(crate) const ENVELOPE_FORMAT: u32 = 1;
/// The only manifest `schema` this crate reads.
const MANIFEST_SCHEMA: u32 = 1;
/// The only `product` value.
const PRODUCT: &str = "sqler";

/// The asset published on GitHub Releases: a signed manifest.
///
/// Unknown fields are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Must be `1`.
    pub format: u32,
    /// Lowercase hex of the first 8 bytes of SHA-256 over the public key.
    pub key_id: String,
    /// Base64 (standard alphabet) of the 64-byte Ed25519 signature.
    pub signature: String,
    /// Base64 (standard alphabet) of the manifest JSON bytes.
    pub payload: String,
}

/// The signed statement about the newest release.
///
/// Unknown fields are ignored, so later schema-1 additions stay readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Must be `1`.
    pub schema: u32,
    /// Must be `"sqler"`.
    pub product: String,
    /// Semantic version without a leading `v`.
    pub version: String,
    /// RFC 3339 date of the release. Informational.
    pub published: String,
    /// `https` URL of the release notes.
    pub notes_url: String,
    /// Downloads by Rust target triple.
    pub platforms: BTreeMap<String, Download>,
}

/// One downloadable artifact. This crate never fetches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// `https` URL of the artifact.
    pub url: String,
    /// 64 lowercase hex characters.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

fn is_https(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host().is_some())
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parses and validates manifest bytes. Call it only after the signature
/// verified.
///
/// # Errors
///
/// [`UpdateError::UnsupportedFormat`] for another `schema`;
/// [`UpdateError::Malformed`] for anything else that is wrong.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, UpdateError> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|_| UpdateError::Malformed("manifest JSON"))?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(UpdateError::UnsupportedFormat);
    }
    if manifest.product != PRODUCT {
        return Err(UpdateError::Malformed("product"));
    }
    if semver::Version::parse(&manifest.version).is_err() {
        return Err(UpdateError::Malformed("version"));
    }
    if !is_https(&manifest.notes_url) {
        return Err(UpdateError::Malformed("notes_url"));
    }
    for download in manifest.platforms.values() {
        if !is_https(&download.url) {
            return Err(UpdateError::Malformed("download url"));
        }
        if !is_sha256_hex(&download.sha256) {
            return Err(UpdateError::Malformed("download sha256"));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest_json(version: &str, notes: &str, url: &str, sha: &str) -> String {
        format!(
            r#"{{"schema":1,"product":"sqler","version":"{version}","published":"2026-10-23",
"notes_url":"{notes}","extra":true,
"platforms":{{"x86_64-unknown-linux-gnu":{{"url":"{url}","sha256":"{sha}","size":10}}}}}}"#
        )
    }

    fn good() -> String {
        manifest_json(
            "1.2.3",
            "https://example.com/n",
            "https://example.com/a",
            SHA,
        )
    }

    #[test]
    fn accepts_a_valid_manifest_and_ignores_unknown_fields() {
        let m = parse_manifest(good().as_bytes()).unwrap();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.platforms.len(), 1);
    }

    #[test]
    fn rejects_bad_fields() {
        let cases = [
            manifest_json("v1.2.3", "https://e.com/n", "https://e.com/a", SHA),
            manifest_json("1.2", "https://e.com/n", "https://e.com/a", SHA),
            manifest_json("1.2.3", "http://e.com/n", "https://e.com/a", SHA),
            manifest_json("1.2.3", "https://e.com/n", "http://e.com/a", SHA),
            manifest_json("1.2.3", "https://e.com/n", "not a url", SHA),
            manifest_json(
                "1.2.3",
                "https://e.com/n",
                "https://e.com/a",
                &SHA.to_uppercase(),
            ),
            manifest_json("1.2.3", "https://e.com/n", "https://e.com/a", "abcd"),
        ];
        for c in cases {
            assert!(
                matches!(parse_manifest(c.as_bytes()), Err(UpdateError::Malformed(_))),
                "{c}"
            );
        }
        assert!(matches!(
            parse_manifest(b"not json"),
            Err(UpdateError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_other_schema_and_product() {
        let other_schema = good().replace(r#""schema":1"#, r#""schema":2"#);
        assert_eq!(
            parse_manifest(other_schema.as_bytes()),
            Err(UpdateError::UnsupportedFormat)
        );
        let other_product = good().replace(r#""product":"sqler""#, r#""product":"x""#);
        assert_eq!(
            parse_manifest(other_product.as_bytes()),
            Err(UpdateError::Malformed("product"))
        );
    }

    #[test]
    fn envelope_rejects_unknown_fields() {
        let json = r#"{"format":1,"key_id":"a","signature":"b","payload":"c","x":1}"#;
        assert!(serde_json::from_str::<Envelope>(json).is_err());
    }
}
