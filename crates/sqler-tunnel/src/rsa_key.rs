// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! RSA user keys without the `rsa` crate (RUSTSEC-2023-0071, ADR-0018).
//!
//! The key is loaded into a `ring` key pair, which signs (PKCS#1 v1.5, blinded) for
//! `rsa-sha2-256` and `rsa-sha2-512`. `ring` needs the CRT exponents `dP` and `dQ`, which an
//! `OpenSSH` key file does not hold; they are computed with the constant-time big-integer
//! arithmetic of `crypto-bigint`. Every buffer this module owns that holds secret key material is
//! zeroized. The `ring` `RsaKeyPair` that `RsaSigner` keeps for the session is `ring` state: `ring`
//! does not zeroize it, so it is not wiped on drop.
//!
//! Limits: no SHA-1 `ssh-rsa`, and only the modulus sizes `ring` signs with (2048, 3072, 4096).

use std::fmt;
use std::path::Path;

use crypto_bigint::{BoxedUint, NonZero};
use ring::rand::SystemRandom;
use ring::rsa::{KeyPairComponents, PublicKeyComponents};
use ring::signature::{self, RsaKeyPair};
use russh::Signer;
use russh::keys::PublicKey;
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::ssh_key::public::{KeyData, RsaPublicKey};
use russh::keys::ssh_key::{HashAlg, Mpint, PrivateKey};
use zeroize::{Zeroize, Zeroizing};

use crate::auth::Secret;
use crate::error::TunnelError;

/// Modulus sizes (bits) that `ring` signs with.
const SUPPORTED_BITS: [u32; 3] = [2048, 3072, 4096];

const HINT: &str = "use an RSA key of 2048, 3072 or 4096 bits, or an Ed25519 or ECDSA key";

/// An RSA user key that signs `rsa-sha2-256` / `rsa-sha2-512` for public key authentication.
/// `Debug` shows no key material.
pub(crate) struct RsaSigner {
    pair: RsaKeyPair,
    public: PublicKey,
}

impl fmt::Debug for RsaSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RsaSigner(***)")
    }
}

fn key_error(path: &Path, message: impl Into<String>) -> TunnelError {
    TunnelError::KeyFile {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

impl RsaSigner {
    /// Loads the key file if it holds an RSA key: `OpenSSH` (plain or encrypted), PKCS#1 PEM
    /// (`RSA PRIVATE KEY`) or PKCS#8 PEM (`PRIVATE KEY`), the last two unencrypted. Returns
    /// `Ok(None)` for any other kind of key or file, which the caller hands to the generic
    /// loader.
    ///
    /// The key is validated by signing once and verifying the signature.
    ///
    /// # Errors
    /// [`TunnelError::KeyFile`] for an unsupported, legacy-encrypted, malformed or inconsistent
    /// key or a passphrase that does not decrypt it, and
    /// [`TunnelError::KeyPassphraseRequired`] for an encrypted key without a passphrase.
    pub(crate) fn load_if_rsa(
        path: &Path,
        passphrase: Option<&Secret>,
    ) -> Result<Option<Self>, TunnelError> {
        let bytes = Zeroizing::new(
            std::fs::read(path)
                .map_err(|e| key_error(path, format!("cannot read the file: {e}")))?,
        );
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Ok(None);
        };
        let Some(pair) = pair_from_text(path, text, passphrase)? else {
            return Ok(None);
        };
        validate(&pair).map_err(|m| key_error(path, m))?;
        let public = public_key(&pair).map_err(|m| key_error(path, m))?;
        Ok(Some(Self { pair, public }))
    }

    /// The public key to offer to the server.
    pub(crate) fn public_key(&self) -> &PublicKey {
        &self.public
    }

    /// Signs `data`; returns the SSH signature blob `string(alg) || string(sig)`.
    fn sign(&self, hash: Option<HashAlg>, data: &[u8]) -> Result<Vec<u8>, TunnelError> {
        let (padding, name): (&'static dyn signature::RsaEncoding, &str) = match hash {
            Some(HashAlg::Sha256) => (&signature::RSA_PKCS1_SHA256, "rsa-sha2-256"),
            Some(HashAlg::Sha512) => (&signature::RSA_PKCS1_SHA512, "rsa-sha2-512"),
            _ => return Err(sha1_only()),
        };
        let mut sig = vec![0u8; self.pair.public().modulus_len()];
        self.pair
            .sign(padding, &SystemRandom::new(), data, &mut sig)
            .map_err(|_| TunnelError::Ssh("RSA signing failed".into()))?;
        let mut blob = Vec::with_capacity(name.len() + sig.len() + 8);
        put_string(&mut blob, name.as_bytes());
        put_string(&mut blob, &sig);
        Ok(blob)
    }
}

/// The error for a server that takes RSA keys only with the SHA-1 `ssh-rsa` signature.
pub(crate) fn sha1_only() -> TunnelError {
    TunnelError::Ssh(
        "the SSH server accepts RSA keys only with the SHA-1 \"ssh-rsa\" signature, which SQLer \
         does not support; use an Ed25519 or ECDSA key, or enable rsa-sha2-256 on the server"
            .into(),
    )
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    // A signature or algorithm name is far below 4 GiB.
    out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(s);
}

impl Signer for RsaSigner {
    type Error = TunnelError;

    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        mut to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        // The signature is appended to the signed data as `string(blob)`.
        let result = self.sign(hash_alg, &to_sign).map(|blob| {
            put_string(&mut to_sign, &blob);
            to_sign
        });
        async move { result }
    }
}

fn pair_from_text(
    path: &Path,
    text: &str,
    passphrase: Option<&Secret>,
) -> Result<Option<RsaKeyPair>, TunnelError> {
    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return pair_from_openssh(path, text, passphrase);
    }
    if text.contains("-----BEGIN RSA PRIVATE KEY-----") {
        if text.contains("Proc-Type:") || text.contains("DEK-Info:") {
            return Err(key_error(
                path,
                "this is a legacy-encrypted PEM key, which is not supported; convert it with \
                 `ssh-keygen -p -o -f <file>` (OpenSSH format), or use an Ed25519 or ECDSA key",
            ));
        }
        let der = pem_der(path, text, "-----BEGIN RSA PRIVATE KEY-----")?;
        return RsaKeyPair::from_der(&der)
            .map(Some)
            .map_err(|e| rejected(path, &e));
    }
    if text.contains("-----BEGIN PRIVATE KEY-----") {
        let der = pem_der(path, text, "-----BEGIN PRIVATE KEY-----")?;
        return match RsaKeyPair::from_pkcs8(&der) {
            Ok(pair) => Ok(Some(pair)),
            // `rsaEncryption` (1.2.840.113549.1.1.1): an RSA key that ring refuses.
            Err(e) if is_rsa_pkcs8(&der) => Err(rejected(path, &e)),
            Err(_) => Ok(None),
        };
    }
    Ok(None)
}

/// Whether a PKCS#8 `PrivateKeyInfo` names `rsaEncryption` (1.2.840.113549.1.1.1). It reads
/// only the fixed header (`SEQUENCE`, `INTEGER 0`, `SEQUENCE { OID .. }`) that precedes the
/// private key octets, never the key itself, so no scan over secret bytes happens.
fn is_rsa_pkcs8(der: &[u8]) -> bool {
    const RSA_ENCRYPTION: [u8; 9] = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
    // Skips a tag byte and a DER length, returning the rest after the length.
    fn header(der: &[u8], tag: u8) -> Option<&[u8]> {
        let (&t, rest) = der.split_first()?;
        let (&len, rest) = rest.split_first()?;
        if t != tag {
            return None;
        }
        // Long form: 0x80 | number of length bytes (at most 4 here).
        let skip = match len {
            0..=0x7F => 0,
            0x81..=0x84 => usize::from(len & 0x7F),
            _ => return None,
        };
        rest.get(skip..)
    }
    let Some(body) = header(der, 0x30) else {
        return false;
    };
    // `INTEGER 0` (the version), then the AlgorithmIdentifier.
    let Some(body) = body.strip_prefix(&[0x02, 0x01, 0x00]) else {
        return false;
    };
    let Some(algorithm) = header(body, 0x30) else {
        return false;
    };
    algorithm
        .strip_prefix(&[0x06, 0x09])
        .is_some_and(|oid| oid.starts_with(&RSA_ENCRYPTION))
}

fn rejected(path: &Path, e: &ring::error::KeyRejected) -> TunnelError {
    key_error(path, format!("the RSA key is not usable ({e}); {HINT}"))
}

/// Decodes the first PEM block that starts at `begin`.
fn pem_der(path: &Path, text: &str, begin: &str) -> Result<Zeroizing<Vec<u8>>, TunnelError> {
    let start = text.find(begin).unwrap_or(0);
    pem_rfc7468::decode_vec(&text.as_bytes()[start..])
        .map(|(_, der)| Zeroizing::new(der))
        .map_err(|_| key_error(path, "the PEM block is malformed"))
}

fn pair_from_openssh(
    path: &Path,
    text: &str,
    passphrase: Option<&Secret>,
) -> Result<Option<RsaKeyPair>, TunnelError> {
    let Ok(key) = PrivateKey::from_openssh(text) else {
        return Ok(None);
    };
    if !key.algorithm().is_rsa() {
        return Ok(None);
    }
    let key = if key.is_encrypted() {
        let pass = passphrase.ok_or_else(|| TunnelError::KeyPassphraseRequired {
            path: path.to_path_buf(),
        })?;
        key.decrypt(pass.expose())
            .map_err(|_| key_error(path, "the passphrase does not decrypt the key"))?
    } else {
        key
    };
    let KeypairData::Rsa(pair) = key.key_data() else {
        return Ok(None);
    };
    let bits = pair.public().key_size();
    if !SUPPORTED_BITS.contains(&bits) {
        return Err(key_error(
            path,
            format!("an RSA key of {bits} bits is not supported; {HINT}"),
        ));
    }
    let malformed = || key_error(path, "the RSA key is malformed");
    let bytes = |m| positive(m, &malformed);
    let private = pair.private();
    let (d, p, q) = (
        bytes(private.d())?,
        bytes(private.p())?,
        bytes(private.q())?,
    );
    let dp = crt_exponent(d, p).ok_or_else(malformed)?;
    let dq = crt_exponent(d, q).ok_or_else(malformed)?;
    let components = KeyPairComponents {
        public_key: PublicKeyComponents {
            n: bytes(pair.public().n())?,
            e: bytes(pair.public().e())?,
        },
        d,
        p,
        q,
        dP: &dp[..],
        dQ: &dq[..],
        qInv: bytes(private.iqmp())?,
    };
    RsaKeyPair::from_components(&components)
        .map(Some)
        .map_err(|e| rejected(path, &e))
}

fn positive<'a>(m: &'a Mpint, err: &dyn Fn() -> TunnelError) -> Result<&'a [u8], TunnelError> {
    m.as_positive_bytes().ok_or_else(err)
}

/// `d mod (prime - 1)` as big-endian bytes of the length of `prime`, in constant time for the
/// given lengths. Every intermediate value is zeroized: `BoxedUint` has no zeroizing `Drop`, so
/// each one (including both results of `div_rem`) is wiped by hand before it goes out of scope.
fn crt_exponent(d: &[u8], prime: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let bits = u32::try_from(d.len().max(prime.len()) * 8)
        .ok()?
        .next_multiple_of(64);
    let mut d_int = BoxedUint::from_be_slice(d, bits).ok()?;
    let mut prime_int = BoxedUint::from_be_slice(prime, bits).ok()?;
    let mut one = BoxedUint::one_with_precision(bits);
    let nonzero = NonZero::new(prime_int.wrapping_sub(&one));
    let result = Option::<NonZero<BoxedUint>>::from(nonzero).map(|mut nz| {
        // `rem` would drop the quotient (`d / (p - 1)`, secret-derived) without wiping it.
        let (mut quotient, mut r) = d_int.div_rem(&nz);
        quotient.zeroize();
        let mut full = r.to_be_bytes();
        let out = full
            .len()
            .checked_sub(prime.len())
            .map(|skip| Zeroizing::new(full[skip..].to_vec()));
        full.as_mut().zeroize();
        r.zeroize();
        nz.zeroize();
        out
    });
    d_int.zeroize();
    prime_int.zeroize();
    one.zeroize();
    result.flatten()
}

/// Signs once with SHA-256 and verifies the signature, so a key whose CRT values do not match
/// fails at load time and not during authentication.
fn validate(pair: &RsaKeyPair) -> Result<(), String> {
    let message = b"sqler rsa key check";
    let mut sig = vec![0u8; pair.public().modulus_len()];
    pair.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message,
        &mut sig,
    )
    .map_err(|_| "the RSA key is inconsistent: it cannot sign".to_owned())?;
    signature::UnparsedPublicKey::new(
        &signature::RSA_PKCS1_2048_8192_SHA256,
        pair.public().as_ref(),
    )
    .verify(message, &sig)
    .map_err(|_| "the RSA key is inconsistent: its signature does not verify".to_owned())
}

fn public_key(pair: &RsaKeyPair) -> Result<PublicKey, String> {
    let components = PublicKeyComponents::<Vec<u8>>::from(pair.public());
    let bad = |_| "the RSA public key is malformed".to_owned();
    let public = RsaPublicKey::new(
        Mpint::from_positive_bytes(&components.e),
        Mpint::from_positive_bytes(&components.n),
    )
    .map_err(bad)?;
    Ok(PublicKey::from(KeyData::Rsa(public)))
}

#[cfg(test)]
mod tests;
