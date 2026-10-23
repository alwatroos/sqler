// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Ed25519 verification of the envelope.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::digest::{SHA256, digest};
use ring::signature::{ED25519, UnparsedPublicKey};

use crate::keys::EMBEDDED;
use crate::manifest::{ENVELOPE_FORMAT, Envelope, Manifest, parse_manifest};
use crate::{MAX_ENVELOPE_BYTES, SIGNING_CONTEXT, UpdateError};

/// The id of a public key: lowercase hex of the first 8 bytes of its SHA-256.
#[must_use]
pub fn key_id(public_key: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let hash = digest(&SHA256, public_key);
    hash.as_ref()[..8]
        .iter()
        .fold(String::with_capacity(16), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The exact bytes that are signed: [`SIGNING_CONTEXT`] followed by the
/// decoded payload.
#[must_use]
pub fn signing_message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIGNING_CONTEXT.len() + payload.len());
    out.extend_from_slice(SIGNING_CONTEXT);
    out.extend_from_slice(payload);
    out
}

/// A public key the client trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    id: String,
    bytes: [u8; 32],
}

impl TrustedKey {
    /// Parses base64 (standard alphabet) of the 32 raw public key bytes.
    /// Surrounding whitespace, including CRLF, is ignored.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Malformed`] when the text is not base64 or not 32 bytes.
    pub fn from_base64(text: &str) -> Result<Self, UpdateError> {
        let raw = STANDARD
            .decode(text.trim())
            .map_err(|_| UpdateError::Malformed("public key encoding"))?;
        let bytes: [u8; 32] = raw
            .try_into()
            .map_err(|_| UpdateError::Malformed("public key length"))?;
        Ok(Self {
            id: key_id(&bytes),
            bytes,
        })
    }

    /// The key id, see [`key_id`].
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The keys compiled into this build. Empty until R17 adds the production key.
///
/// An entry that does not parse is skipped; a unit test makes sure none does.
#[must_use]
pub fn embedded_keys() -> Vec<TrustedKey> {
    EMBEDDED
        .iter()
        .filter_map(|text| TrustedKey::from_base64(text).ok())
        .collect()
}

/// Verifies an envelope and returns its manifest. The payload is parsed only
/// after the signature verified.
///
/// # Errors
///
/// [`UpdateError::TooLarge`], [`UpdateError::Malformed`],
/// [`UpdateError::UnsupportedFormat`], [`UpdateError::UnknownKey`] or
/// [`UpdateError::BadSignature`].
pub fn verify_envelope(bytes: &[u8], keys: &[TrustedKey]) -> Result<Manifest, UpdateError> {
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(UpdateError::TooLarge);
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| UpdateError::Malformed("envelope JSON"))?;
    if envelope.format != ENVELOPE_FORMAT {
        return Err(UpdateError::UnsupportedFormat);
    }
    let key = keys
        .iter()
        .find(|k| k.id == envelope.key_id)
        .ok_or(UpdateError::UnknownKey)?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .map_err(|_| UpdateError::Malformed("signature encoding"))?;
    let payload = STANDARD
        .decode(&envelope.payload)
        .map_err(|_| UpdateError::Malformed("payload encoding"))?;
    UnparsedPublicKey::new(&ED25519, key.bytes)
        .verify(&signing_message(&payload), &signature)
        .map_err(|_| UpdateError::BadSignature)?;
    parse_manifest(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    // TEST ONLY, never a release key.
    const SEED: [u8; 32] = [0x5A; 32];

    fn pair() -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&SEED).unwrap()
    }

    fn public(pair: &Ed25519KeyPair) -> [u8; 32] {
        pair.public_key().as_ref().try_into().unwrap()
    }

    fn envelope(payload: &[u8], key_id: &str) -> Vec<u8> {
        let sig = pair().sign(&signing_message(payload));
        serde_json::to_vec(&Envelope {
            format: 1,
            key_id: key_id.to_owned(),
            signature: STANDARD.encode(sig.as_ref()),
            payload: STANDARD.encode(payload),
        })
        .unwrap()
    }

    const PAYLOAD: &str = r#"{"schema":1,"product":"sqler","version":"1.0.0","published":"2026-10-23","notes_url":"https://example.com/n","platforms":{}}"#;

    #[test]
    fn key_id_vector() {
        // SHA-256 of 32 zero bytes starts with 66687aadf862bd77.
        assert_eq!(key_id(&[0; 32]), "66687aadf862bd77");
    }

    #[test]
    fn signing_message_is_context_then_payload() {
        let m = signing_message(b"abc");
        assert!(m.starts_with(SIGNING_CONTEXT));
        assert!(m.ends_with(b"abc"));
        assert_eq!(m.len(), SIGNING_CONTEXT.len() + 3);
    }

    #[test]
    fn signing_context_bytes_are_pinned() {
        assert_eq!(SIGNING_CONTEXT, b"sqler-update-manifest-v1\0");
    }

    #[test]
    fn signature_over_the_bare_payload_is_rejected() {
        let raw = public(&pair());
        let key = TrustedKey::from_base64(&STANDARD.encode(raw)).unwrap();
        let sig = pair().sign(PAYLOAD.as_bytes());
        let bytes = serde_json::to_vec(&Envelope {
            format: 1,
            key_id: key.id().to_owned(),
            signature: STANDARD.encode(sig.as_ref()),
            payload: STANDARD.encode(PAYLOAD),
        })
        .unwrap();
        assert_eq!(
            verify_envelope(&bytes, &[key]),
            Err(UpdateError::BadSignature)
        );
    }

    #[test]
    fn base64_key_parsing() {
        let raw = public(&pair());
        let text = STANDARD.encode(raw);
        let key = TrustedKey::from_base64(&format!("{text}\r\n")).unwrap();
        assert_eq!(key.id(), key_id(&raw));
        assert!(TrustedKey::from_base64(&STANDARD.encode([1u8; 31])).is_err());
        assert!(TrustedKey::from_base64("***").is_err());
        assert!(TrustedKey::from_base64("").is_err());
    }

    #[test]
    fn every_embedded_key_parses() {
        for text in EMBEDDED {
            assert!(TrustedKey::from_base64(text).is_ok());
        }
        assert_eq!(embedded_keys().len(), EMBEDDED.len());
    }

    #[test]
    fn verifies_and_rejects() {
        let raw = public(&pair());
        let key = TrustedKey::from_base64(&STANDARD.encode(raw)).unwrap();
        let keys = [key.clone()];
        let good = envelope(PAYLOAD.as_bytes(), key.id());
        assert_eq!(verify_envelope(&good, &keys).unwrap().version, "1.0.0");
        assert_eq!(verify_envelope(&good, &[]), Err(UpdateError::UnknownKey));
        let tampered = envelope(PAYLOAD.as_bytes(), key.id());
        let mut env: Envelope = serde_json::from_slice(&tampered).unwrap();
        env.payload = STANDARD.encode(PAYLOAD.replace("1.0.0", "9.0.0"));
        assert_eq!(
            verify_envelope(&serde_json::to_vec(&env).unwrap(), &keys),
            Err(UpdateError::BadSignature)
        );
        env.format = 2;
        assert_eq!(
            verify_envelope(&serde_json::to_vec(&env).unwrap(), &keys),
            Err(UpdateError::UnsupportedFormat)
        );
        assert!(matches!(
            verify_envelope(b"{", &keys),
            Err(UpdateError::Malformed(_))
        ));
        assert_eq!(
            verify_envelope(&vec![b' '; MAX_ENVELOPE_BYTES + 1], &keys),
            Err(UpdateError::TooLarge)
        );
    }
}
