// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Keys are generated with `ssh-keygen` into a temp directory; the tests skip when it is absent.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

const PASS: &str = "rsa-test-pass";

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sqler-rsa-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `ssh-keygen`; `None` if it is not installed.
fn keygen(args: &[&str]) -> Option<()> {
    if let Ok(out) = Command::new("ssh-keygen").args(args).output() {
        assert!(out.status.success(), "ssh-keygen {args:?}: {out:?}");
        Some(())
    } else {
        println!("skipped: ssh-keygen is not installed");
        None
    }
}

fn generate(dir: &Dir, name: &str, bits: u32, format: &str, pass: &str) -> Option<PathBuf> {
    let path = dir.path(name);
    let bits = bits.to_string();
    keygen(&[
        "-q",
        "-t",
        "rsa",
        "-b",
        &bits,
        "-m",
        format,
        "-N",
        pass,
        "-C",
        "",
        "-f",
        path.to_str().unwrap(),
    ])?;
    Some(path)
}

fn load(path: &Path, pass: Option<&str>) -> Result<Option<RsaSigner>, TunnelError> {
    RsaSigner::load_if_rsa(path, pass.map(Secret::new).as_ref())
}

fn message(err: &TunnelError) -> String {
    match err {
        TunnelError::KeyFile { message, .. } => message.clone(),
        other => panic!("expected KeyFile, got {other:?}"),
    }
}

/// Splits `string(x) || rest`.
fn string_at(blob: &[u8]) -> (&[u8], &[u8]) {
    let len = u32::from_be_bytes(blob[..4].try_into().unwrap()) as usize;
    (&blob[4..4 + len], &blob[4 + len..])
}

fn block_on<F: Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(fut)
}

/// Signs through the `Signer` path and verifies the appended signature with ring.
fn check_signatures(signer: &mut RsaSigner) {
    let data = b"session id and request".to_vec();
    let identity = AgentIdentity::PublicKey {
        key: signer.public_key().clone(),
        comment: String::new(),
    };
    for (hash, name, alg) in [
        (
            HashAlg::Sha256,
            "rsa-sha2-256",
            &signature::RSA_PKCS1_2048_8192_SHA256,
        ),
        (
            HashAlg::Sha512,
            "rsa-sha2-512",
            &signature::RSA_PKCS1_2048_8192_SHA512,
        ),
    ] {
        let out = block_on(signer.auth_sign(&identity, Some(hash), data.clone())).unwrap();
        assert_eq!(&out[..data.len()], &data[..]);
        let (blob, rest) = string_at(&out[data.len()..]);
        assert_eq!(rest, b"");
        let (alg_name, rest) = string_at(blob);
        assert_eq!(alg_name, name.as_bytes());
        let (sig, rest) = string_at(rest);
        assert_eq!(rest, b"");
        signature::UnparsedPublicKey::new(alg, signer.pair.public().as_ref())
            .verify(&data, sig)
            .unwrap();
    }
}

#[test]
fn crt_exponent_is_d_mod_prime_minus_one() {
    let out = crt_exponent(&[100], &[13]).unwrap();
    assert_eq!(&out[..], &[4]);
    // d longer than the prime: 0x0102030405 mod 250.
    let out = crt_exponent(&[1, 2, 3, 4, 5], &[251]).unwrap();
    assert_eq!(u64::from(out[0]), 0x01_0203_0405_u64 % 250);
    assert_eq!(out.len(), 1);
    // p = 1 gives a zero modulus.
    assert!(crt_exponent(&[5], &[1]).is_none());
}

#[test]
fn openssh_keys_sign_and_verify() {
    let dir = Dir::new("openssh");
    for bits in [2048, 3072, 4096] {
        let Some(path) = generate(&dir, &format!("k{bits}"), bits, "RFC4716", "") else {
            return;
        };
        let mut signer = load(&path, None).unwrap().unwrap();
        check_signatures(&mut signer);
        let public = std::fs::read_to_string(dir.path(&format!("k{bits}.pub"))).unwrap();
        let expected = PublicKey::from_openssh(&public).unwrap();
        assert_eq!(signer.public_key().key_data(), expected.key_data());
        assert_eq!(format!("{signer:?}"), "RsaSigner(***)");
    }
}

#[test]
fn encrypted_openssh_key_needs_the_right_passphrase() {
    let dir = Dir::new("enc");
    let Some(path) = generate(&dir, "enc", 3072, "RFC4716", PASS) else {
        return;
    };
    let mut signer = load(&path, Some(PASS)).unwrap().unwrap();
    check_signatures(&mut signer);
    assert!(matches!(
        load(&path, None),
        Err(TunnelError::KeyPassphraseRequired { .. })
    ));
    let err = load(&path, Some("wrong")).unwrap_err();
    assert!(
        message(&err).contains("passphrase does not decrypt"),
        "{err}"
    );
}

#[test]
fn pem_keys_pkcs1_and_pkcs8() {
    let dir = Dir::new("pem");
    let Some(pem) = generate(&dir, "pem", 2048, "PEM", "") else {
        return;
    };
    let text = std::fs::read_to_string(&pem).unwrap();
    assert!(text.contains("BEGIN RSA PRIVATE KEY"), "{text}");
    check_signatures(&mut load(&pem, None).unwrap().unwrap());

    let pkcs8 = dir.path("pkcs8");
    std::fs::copy(&pem, &pkcs8).unwrap();
    keygen(&[
        "-q",
        "-p",
        "-m",
        "PKCS8",
        "-N",
        "",
        "-P",
        "",
        "-f",
        pkcs8.to_str().unwrap(),
    ])
    .unwrap();
    let text = std::fs::read_to_string(&pkcs8).unwrap();
    assert!(text.contains("BEGIN PRIVATE KEY"), "{text}");
    let mut signer = load(&pkcs8, None).unwrap().unwrap();
    check_signatures(&mut signer);
    // Same key, same public key as the PKCS#1 file.
    let other = load(&pem, None).unwrap().unwrap();
    assert_eq!(
        signer.public_key().key_data(),
        other.public_key().key_data()
    );
}

#[test]
fn unsupported_sizes_are_refused_with_a_readable_message() {
    let dir = Dir::new("sizes");
    for bits in [1024, 2560] {
        for format in ["RFC4716", "PEM"] {
            let Some(path) = generate(&dir, &format!("k{bits}{format}"), bits, format, "") else {
                return;
            };
            let err = load(&path, None).unwrap_err();
            let text = message(&err);
            assert!(
                text.contains("2048, 3072 or 4096"),
                "{bits} {format}: {text}"
            );
            assert!(text.contains("Ed25519 or ECDSA"), "{text}");
            if format == "RFC4716" {
                assert!(text.contains(&bits.to_string()), "{text}");
            }
        }
    }
}

#[test]
fn legacy_encrypted_pem_is_refused() {
    let dir = Dir::new("legacy");
    let Some(path) = generate(&dir, "legacy", 2048, "PEM", PASS) else {
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Proc-Type"), "{text}");
    let err = load(&path, Some(PASS)).unwrap_err();
    let text = message(&err);
    assert!(text.contains("legacy-encrypted"), "{text}");
    assert!(text.contains("ssh-keygen -p -o"), "{text}");
}

#[test]
fn other_keys_and_files_are_left_to_the_generic_loader() {
    let dir = Dir::new("other");
    let ed = dir.path("ed");
    if keygen(&["-q", "-t", "ed25519", "-N", "", "-f", ed.to_str().unwrap()]).is_none() {
        return;
    }
    assert!(load(&ed, None).unwrap().is_none());
    let junk = dir.path("junk");
    std::fs::write(&junk, "not a key").unwrap();
    assert!(load(&junk, None).unwrap().is_none());
    std::fs::write(&junk, [0xff, 0xfe]).unwrap();
    assert!(load(&junk, None).unwrap().is_none());
    assert!(matches!(
        load(&dir.path("missing"), None),
        Err(TunnelError::KeyFile { .. })
    ));
}

#[test]
fn sha1_is_refused_when_signing() {
    let dir = Dir::new("sha1");
    let Some(path) = generate(&dir, "k", 2048, "RFC4716", "") else {
        return;
    };
    let signer = load(&path, None).unwrap().unwrap();
    let err = signer.sign(None, b"x").unwrap_err();
    assert!(err.to_string().contains("SHA-1"), "{err}");
}

#[test]
fn pkcs8_rsa_detection_reads_only_the_header() {
    use super::is_rsa_pkcs8;
    let rsa: &[u8] = &[
        0x30, 0x82, 0x04, 0xA4, 0x02, 0x01, 0x00, 0x30, 0x0D, 0x06, 0x09, 0x2A, 0x86, 0x48, 0x86,
        0xF7, 0x0D, 0x01, 0x01, 0x01, 0x05, 0x00, 0x04, 0x82,
    ];
    assert!(is_rsa_pkcs8(rsa));
    // Ed25519 (1.3.101.112).
    let ed: &[u8] = &[
        0x30, 0x2E, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2B, 0x65, 0x70, 0x04, 0x22,
    ];
    assert!(!is_rsa_pkcs8(ed));
    // The OID bytes appearing later (inside the key octets) do not count.
    let mut late = ed.to_vec();
    late.extend_from_slice(&rsa[11..20]);
    assert!(!is_rsa_pkcs8(&late));
    for bad in [&[][..], &[0x30], &[0x30, 0x82], &rsa[..8], &[0x31, 0x00]] {
        assert!(!is_rsa_pkcs8(bad));
    }
}
