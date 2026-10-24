// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Key file reading, signing, envelope construction and verification.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use sqler_update::{
    Envelope, MAX_ENVELOPE_BYTES, Manifest, TrustedKey, key_id, signing_message, verify_envelope,
};
use zeroize::Zeroizing;

const MAX_KEY_FILE_BYTES: u64 = 1024;
const KEY_FORMAT_ERROR: &str = "signing key file is not one line of base64 of a 32-byte seed";

/// Read a base64 Ed25519 seed from `path`. Errors never name the path or the content.
pub fn read_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>, String> {
    let file = File::open(path).map_err(|_| KEY_FORMAT_ERROR.to_owned())?;
    let meta = file.metadata().map_err(|_| KEY_FORMAT_ERROR.to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        const KEY_MODE_ERROR: &str = "signing key file must not be readable by others";
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(KEY_MODE_ERROR.to_owned());
        }
    }
    if !meta.is_file() {
        return Err(KEY_FORMAT_ERROR.to_owned());
    }
    let mut raw = Zeroizing::new(Vec::new());
    file.take(MAX_KEY_FILE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| KEY_FORMAT_ERROR.to_owned())?;
    if raw.len() as u64 > MAX_KEY_FILE_BYTES {
        return Err(KEY_FORMAT_ERROR.to_owned());
    }
    let text = std::str::from_utf8(&raw).map_err(|_| KEY_FORMAT_ERROR.to_owned())?;
    let decoded = Zeroizing::new(
        STANDARD
            .decode(text.trim())
            .map_err(|_| KEY_FORMAT_ERROR.to_owned())?,
    );
    let mut seed = Zeroizing::new([0_u8; 32]);
    if decoded.len() != 32 {
        return Err(KEY_FORMAT_ERROR.to_owned());
    }
    seed.copy_from_slice(&decoded);
    Ok(seed)
}

/// A production key must be compiled in; a TEST ONLY key must not be.
pub fn check_key(kid: &str, test: bool, embedded: &[TrustedKey]) -> Result<(), String> {
    let known = embedded.iter().any(|k| k.id() == kid);
    match (test, known) {
        (false, false) => Err(format!("signing key {kid} is not a compiled-in key")),
        (true, true) => Err(format!("TEST ONLY mode refuses the compiled-in key {kid}")),
        _ => Ok(()),
    }
}

/// Sign `manifest` and return the envelope bytes (JSON plus a newline). The envelope is
/// verified before it is returned.
pub fn sign(
    manifest: &Manifest,
    seed: &[u8; 32],
    test: bool,
    embedded: &[TrustedKey],
) -> Result<Vec<u8>, String> {
    let pair =
        Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| KEY_FORMAT_ERROR.to_owned())?;
    let public: [u8; 32] = pair
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| "unexpected public key length".to_owned())?;
    let kid = key_id(&public);
    check_key(&kid, test, embedded)?;
    let derived = TrustedKey::from_base64(&STANDARD.encode(public))
        .map_err(|e| format!("cannot derive the public key: {e}"))?;
    let trust = if test {
        vec![derived]
    } else {
        embedded.to_vec()
    };
    let payload = serde_json::to_vec(manifest).map_err(|_| "cannot encode manifest".to_owned())?;
    let signature = pair.sign(&signing_message(&payload));
    let envelope = Envelope {
        format: 1,
        key_id: kid,
        signature: STANDARD.encode(signature.as_ref()),
        payload: STANDARD.encode(&payload),
    };
    let mut bytes =
        serde_json::to_vec(&envelope).map_err(|_| "cannot encode envelope".to_owned())?;
    bytes.push(b'\n');
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err("envelope exceeds the size limit".to_owned());
    }
    let checked = verify_envelope(&bytes, &trust).map_err(|e| format!("self-check failed: {e}"))?;
    if checked != *manifest {
        return Err("self-check failed: manifest differs".to_owned());
    }
    Ok(bytes)
}

/// Verify an envelope file against the compiled-in keys or one public key file.
/// Returns the key id and the manifest.
pub fn verify_file(
    file: &Path,
    public_key_file: Option<&Path>,
) -> Result<(String, Manifest), String> {
    let mut bytes = Vec::new();
    File::open(file)
        .and_then(|f| {
            f.take(MAX_ENVELOPE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| "cannot read the envelope file".to_owned())?;
    let trust = if let Some(path) = public_key_file {
        let mut text = String::new();
        File::open(path)
            .and_then(|f| f.take(4096).read_to_string(&mut text))
            .map_err(|_| "cannot read the public key file".to_owned())?;
        vec![TrustedKey::from_base64(&text).map_err(|e| e.to_string())?]
    } else {
        sqler_update::embedded_keys()
    };
    let manifest = verify_envelope(&bytes, &trust).map_err(|e| e.to_string())?;
    let envelope: Envelope =
        serde_json::from_slice(&bytes).map_err(|_| "envelope is not valid JSON".to_owned())?;
    Ok((envelope.key_id, manifest))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use sqler_update::{Envelope, TrustedKey, UpdateError, verify_envelope};

    use super::{check_key, read_seed, sign};
    use crate::release::build::{
        self,
        tests::{PLAN, archives, opts},
    };

    /// TEST ONLY, never a release key.
    const SEED_A: [u8; 32] = [0xA1; 32];
    /// TEST ONLY, never a release key.
    const SEED_B: [u8; 32] = [0xB2; 32];

    fn trusted(seed: &[u8; 32]) -> TrustedKey {
        let pair = Ed25519KeyPair::from_seed_unchecked(seed).unwrap();
        TrustedKey::from_base64(&STANDARD.encode(pair.public_key().as_ref())).unwrap()
    }

    fn manifest(case: &str) -> sqler_update::Manifest {
        let d = archives(case);
        build::build(PLAN.as_bytes(), &d.0, &opts(Some("v1.2.3"), false)).unwrap()
    }

    fn reencode(bytes: &[u8], f: impl FnOnce(&mut Envelope)) -> Vec<u8> {
        let mut env: Envelope = serde_json::from_slice(bytes).unwrap();
        f(&mut env);
        serde_json::to_vec(&env).unwrap()
    }

    #[test]
    fn round_trip_and_tamper() {
        let m = manifest("rt");
        let bytes = sign(&m, &SEED_A, true, &[]).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let a = [trusted(&SEED_A)];
        assert_eq!(verify_envelope(&bytes, &a).unwrap(), m);

        let version = reencode(&bytes, |e| {
            let p = STANDARD.decode(&e.payload).unwrap();
            let p = String::from_utf8(p).unwrap().replace("1.2.3", "1.2.9");
            e.payload = STANDARD.encode(p);
        });
        assert_eq!(
            verify_envelope(&version, &a),
            Err(UpdateError::BadSignature)
        );

        let flipped = reencode(&bytes, |e| {
            let mut s = STANDARD.decode(&e.signature).unwrap();
            s[0] ^= 1;
            e.signature = STANDARD.encode(s);
        });
        assert_eq!(
            verify_envelope(&flipped, &a),
            Err(UpdateError::BadSignature)
        );

        let other = reencode(&bytes, |e| e.key_id = "0000000000000000".to_owned());
        assert_eq!(verify_envelope(&other, &a), Err(UpdateError::UnknownKey));

        assert_eq!(
            verify_envelope(&bytes, &[trusted(&SEED_B)]),
            Err(UpdateError::UnknownKey)
        );

        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["extra"] = serde_json::json!(1);
        let extra = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            verify_envelope(&extra, &a),
            Err(UpdateError::Malformed(_))
        ));

        let v2 = reencode(&bytes, |e| e.format = 2);
        assert_eq!(
            verify_envelope(&v2, &a),
            Err(UpdateError::UnsupportedFormat)
        );
    }

    #[test]
    fn production_mode_signs_only_with_a_listed_key() {
        let m = manifest("prod");
        let listed = [trusted(&SEED_A)];
        let bytes = sign(&m, &SEED_A, false, &listed).unwrap();
        assert_eq!(verify_envelope(&bytes, &listed).unwrap(), m);
        assert!(sign(&m, &SEED_B, false, &listed).is_err());
    }

    #[test]
    fn test_mode_refuses_a_listed_key() {
        let m = manifest("testmode");
        assert!(sign(&m, &SEED_A, true, &[trusted(&SEED_A)]).is_err());
        assert!(sign(&m, &SEED_A, true, &[trusted(&SEED_B)]).is_ok());
    }

    #[test]
    fn check_key_guard() {
        let fake = [trusted(&SEED_A)];
        let id = fake[0].id().to_owned();
        assert!(check_key(&id, false, &fake).is_ok());
        assert!(check_key(&id, true, &fake).is_err());
        assert!(check_key("ffffffffffffffff", false, &fake).is_err());
        assert!(check_key("ffffffffffffffff", true, &fake).is_ok());
        assert!(check_key(&id, false, &[]).is_err());
    }

    fn key_file(case: &str, content: &[u8], mode: u32) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("xtask-release-{}-key-{case}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
        path
    }

    #[test]
    fn key_file_rules() {
        let good = format!("{}\r\n", STANDARD.encode(SEED_A));
        let p = key_file("good", good.as_bytes(), 0o600);
        assert_eq!(*read_seed(&p).unwrap(), SEED_A);

        let short = key_file("short", STANDARD.encode([1_u8; 31]).as_bytes(), 0o600);
        assert!(read_seed(&short).is_err());

        let junk = key_file("junk", b"this is !! not base64", 0o600);
        let err = read_seed(&junk).unwrap_err();
        assert!(!err.contains("not base64"), "{err}");

        let big = key_file("big", &vec![b'A'; 2000], 0o600);
        assert!(read_seed(&big).is_err());

        assert!(read_seed(std::path::Path::new("/nonexistent/xtask-key")).is_err());

        #[cfg(unix)]
        {
            let open = key_file("open", good.as_bytes(), 0o644);
            let err = read_seed(&open).unwrap_err();
            assert!(err.contains("must not be readable by others"), "{err}");
            assert!(!err.contains(&good.trim().to_owned()));
            let _ = std::fs::remove_file(&open);
        }
        for f in [p, short, junk, big] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn verify_file_uses_the_given_public_key() {
        let m = manifest("vf");
        let bytes = sign(&m, &SEED_A, true, &[]).unwrap();
        let dir =
            std::env::temp_dir().join(format!("xtask-release-{}-vf-files", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let env = dir.join("e.json");
        std::fs::write(&env, &bytes).unwrap();
        let pair = Ed25519KeyPair::from_seed_unchecked(&SEED_A).unwrap();
        let pubf = dir.join("a.pub");
        std::fs::write(&pubf, STANDARD.encode(pair.public_key().as_ref())).unwrap();
        let (kid, got) = super::verify_file(&env, Some(&pubf)).unwrap();
        assert_eq!(got, m);
        assert_eq!(kid, trusted(&SEED_A).id());
        // Without a key file only the compiled-in keys are trusted.
        assert!(super::verify_file(&env, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
