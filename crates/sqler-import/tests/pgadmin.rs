// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `pgAdmin` source against fixture files, hostile inputs and random bytes.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::format_push_string
)]

use std::path::PathBuf;

use proptest::prelude::*;
use sqler_config::{ProfileSecretKind, SshAuth, TlsMode};
use sqler_import::{
    EntryOutcome, ImportError, MAX_ENTRIES, MAX_FILE_BYTES, PgAdmin, SkipReason, Source,
    SourceEntry, SourceReport, read_source, source, sources,
};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pgadmin")
        .join(name)
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).unwrap()
}

/// The directory prefix the fixtures use for absolute paths, and its form on this OS.
const FIXTURE_HOME: &str = "/home/alice/";
#[cfg(unix)]
const HOME: &str = "/home/alice/";
#[cfg(windows)]
const HOME: &str = "C:\\Users\\alice\\";

/// The path a fixture path becomes on this OS (`/home/alice/x` is absolute on unix only).
fn native(tail: &str) -> PathBuf {
    PathBuf::from(format!("{HOME}{tail}"))
}

/// Fixture bytes with every `/home/alice/` path rewritten to an absolute path of this OS, so the
/// same fixture asserts the full mapping on unix and on Windows.
fn portable(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // JSON text: a backslash is written twice.
    text.replace(FIXTURE_HOME, &HOME.replace('\\', "\\\\"))
        .into_bytes()
}

fn report(name: &str) -> SourceReport {
    PgAdmin.parse(&portable(&fixture(name))).unwrap()
}

fn spec_of(entry: &SourceEntry) -> &sqler_config::ConnectionSpec {
    match &entry.outcome {
        EntryOutcome::Mapped(s) => s,
        EntryOutcome::Skipped(r) => panic!("skipped: {r:?}"),
    }
}

fn skip_of(entry: &SourceEntry) -> &SkipReason {
    match &entry.outcome {
        EntryOutcome::Skipped(r) => r,
        EntryOutcome::Mapped(s) => panic!("mapped: {s:?}"),
    }
}

#[test]
fn registry_has_pgadmin() {
    assert!(sources().iter().any(|s| s.id() == "pgadmin"));
    let s = source("pgadmin").unwrap();
    assert_eq!(s.label(), "pgAdmin 4");
    assert!(source("nope").is_none());
}

#[test]
fn full_fixture_maps_every_field() {
    let r = report("full.json");
    assert_eq!(r.source, "pgadmin");
    assert!(r.notices.is_empty());
    assert_eq!(r.entries.len(), 7);

    let a = &r.entries[0];
    assert_eq!(a.name, "Orders primary");
    assert_eq!(a.group, vec!["Shop".to_owned()]);
    let spec = spec_of(a);
    assert_eq!(spec.driver.as_str(), "postgres");
    assert_eq!(
        spec.target,
        "postgres://db.example.test:5433/orders?user=alice"
    );
    assert!(!spec.target.contains('@'));
    assert_eq!(spec.color.as_deref(), Some("#aa3300"));
    assert!(!spec.password_in_keychain);
    let tls = spec.tls.as_ref().unwrap();
    assert_eq!(tls.mode, TlsMode::VerifyFull);
    assert_eq!(tls.ca_cert, Some(native("certs/ca.pem")));
    assert_eq!(tls.client_cert, Some(native("certs/client.pem")));
    assert_eq!(tls.client_key, Some(native("certs/client.key")));
    assert_eq!(a.secrets, vec![ProfileSecretKind::DbPassword]);
    for name in [
        "\"FGColor\"",
        "\"Comment\"",
        "\"Role\"",
        "\"PassFile\"",
        "ConnectionParameters.\"connect_timeout\"",
    ] {
        assert!(a.unsupported.contains(&name.to_owned()), "{name}");
    }
    assert!(!a.unsupported.iter().any(|u| u.contains("Name")));

    // Legacy form: string port and top-level SSL keys.
    let b = spec_of(&r.entries[1]);
    assert_eq!(
        b.target,
        "postgres://staging.example.test:5432/postgres?user=bob"
    );
    assert_eq!(b.tls.as_ref().unwrap().mode, TlsMode::Require);
    assert!(r.entries[1].unsupported.contains(&"\"Timeout\"".to_owned()));

    // SSH with a key file; "allow" maps to prefer with a note.
    let c = &r.entries[2];
    let spec = spec_of(c);
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::Prefer);
    assert!(c.notes.iter().any(|n| n.contains("allow")));
    let ssh = spec.ssh.as_ref().unwrap();
    assert_eq!(
        (ssh.host.as_str(), ssh.port, ssh.user.as_str()),
        ("bastion.example.test", 2222, "tunnel")
    );
    assert_eq!(
        b.tls.as_ref().unwrap().ca_cert,
        Some(native("certs/ca.pem"))
    );
    assert!(matches!(
        &ssh.auth,
        SshAuth::KeyFile {
            path,
            passphrase_in_keychain: false,
            ..
        } if *path == native(".ssh/id_test")
    ));
    assert_eq!(
        c.secrets,
        vec![
            ProfileSecretKind::DbPassword,
            ProfileSecretKind::SshPassphrase
        ]
    );

    // SSH password, IPv6 host in brackets, default SSH port.
    let d = &r.entries[3];
    let spec = spec_of(d);
    assert_eq!(spec.target, "postgres://[::1]:5432/postgres?user=alice");
    let ssh = spec.ssh.as_ref().unwrap();
    assert_eq!(ssh.port, 22);
    assert_eq!(ssh.auth, SshAuth::Password);
    assert_eq!(
        d.secrets,
        vec![
            ProfileSecretKind::DbPassword,
            ProfileSecretKind::SshPassword
        ]
    );

    // Socket directory is percent-encoded.
    assert_eq!(
        spec_of(&r.entries[4]).target,
        "postgres://%2Fvar%2Frun%2Fpostgresql:5432/postgres?user=carol"
    );
    assert_eq!(
        spec_of(&r.entries[4]).tls.as_ref().unwrap().mode,
        TlsMode::Disable
    );

    // Kerberos: no password to enter; the user name with '@' is encoded.
    let k = &r.entries[5];
    assert!(k.secrets.is_empty());
    assert_eq!(
        spec_of(k).target,
        "postgres://krb.example.test:5432/postgres?user=alice%40EXAMPLE.TEST"
    );
    assert!(
        k.unsupported
            .contains(&"\"KerberosAuthentication\"".to_owned())
    );

    // pg_service.
    assert_eq!(
        skip_of(&r.entries[6]),
        &SkipReason::Unsupported("pg_service entries are not supported".to_owned())
    );
}

#[test]
fn empty_and_broken_files() {
    assert!(report("empty.json").entries.is_empty());
    assert!(matches!(
        PgAdmin.parse(&fixture("empty-file.json")),
        Err(ImportError::Malformed { .. })
    ));
    assert!(matches!(
        PgAdmin.parse(&fixture("malformed.json")),
        Err(ImportError::Malformed { .. })
    ));
    assert!(matches!(
        PgAdmin.parse(&fixture("not-servers.json")),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert!(matches!(
        PgAdmin.parse(b"[1,2]"),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert!(matches!(
        PgAdmin.parse(b"{\"Servers\": []}"),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert_eq!(
        PgAdmin.parse(&[0xFF, 0xFE, b'{']),
        Err(ImportError::NotUtf8)
    );
}

#[test]
fn error_texts_do_not_echo_content() {
    let e = PgAdmin
        .parse(b"{\"Servers\": {\"1\": {\"Name\": \"fake-pw-123\" \"x\"")
        .unwrap_err();
    assert!(!e.to_string().contains("fake-pw-123"));
    assert!(!format!("{e:?}").contains("fake-pw-123"));
}

#[test]
fn wrong_types_give_a_partial_report() {
    let r = report("wrong-types.json");
    assert_eq!(r.entries.len(), 12);
    let e = &r.entries;
    // Numeric name falls back; the bad port skips the entry.
    assert_eq!(e[0].name, "pgAdmin server 1");
    assert!(e[0].notes.iter().any(|n| n.contains("Name")));
    assert!(matches!(skip_of(&e[0]), SkipReason::Invalid(_)));
    assert!(matches!(skip_of(&e[1]), SkipReason::Invalid(_)));
    assert!(matches!(skip_of(&e[2]), SkipReason::Invalid(m) if m.contains("not an object")));
    assert!(matches!(skip_of(&e[3]), SkipReason::Invalid(m) if m.contains("port")));
    assert!(matches!(skip_of(&e[4]), SkipReason::Invalid(m) if m.contains("port")));
    assert!(matches!(skip_of(&e[5]), SkipReason::Invalid(m) if m.contains("host")));
    // Mapped with notes.
    assert!(spec_of(&e[6]).color.is_none() && spec_of(&e[6]).tls.is_none());
    assert!(e[6].notes.len() >= 3, "{:?}", e[6].notes);
    assert!(matches!(skip_of(&e[7]), SkipReason::Invalid(m) if m.contains("user")));
    assert!(matches!(skip_of(&e[8]), SkipReason::Invalid(m) if m.contains("authentication")));
    assert_eq!(
        skip_of(&e[9]),
        &SkipReason::Invalid("SSH key path not usable".to_owned())
    );
    assert!(spec_of(&e[10]).tls.is_none());
    assert!(e[10].notes.iter().any(|n| n.contains("sslmode")));
    assert_eq!(e[11].name, "pgAdmin server 12");
    assert!(spec_of(&e[11]).tls.is_none());
}

#[test]
fn future_fields_are_listed_by_name_only() {
    let r = report("future.json");
    assert_eq!(r.notices.len(), 2);
    let e = &r.entries[0];
    for name in [
        "\"Tags\"",
        "\"PostConnectionSQL\"",
        "\"PasswordExecCommand\"",
        "\"BrandNewField\"",
        "ConnectionParameters.\"sslcrl\"",
        "ConnectionParameters.\"brand_new_parameter\"",
    ] {
        assert!(e.unsupported.contains(&name.to_owned()), "{name}");
    }
    let everything = format!("{r:?}");
    assert!(!everything.contains("fake-pw-123"));
    assert!(!everything.contains("SELECT 1"));
    assert_eq!(spec_of(e).tls.as_ref().unwrap().mode, TlsMode::Prefer);
}

#[test]
fn duplicates_fixture_parses_all_entries() {
    let r = report("duplicates.json");
    assert_eq!(r.entries.len(), 5);
    assert!(
        r.entries
            .iter()
            .all(|e| matches!(e.outcome, EntryOutcome::Mapped(_)))
    );
}

#[test]
fn bad_paths_are_dropped_with_notes() {
    let r = report("paths.json");
    let e = &r.entries;
    for i in [0, 1, 2, 3, 5] {
        let tls = spec_of(&e[i]).tls.as_ref().unwrap();
        assert!(tls.ca_cert.is_none(), "{i}");
        assert!(e[i].notes.iter().any(|n| n.contains("dropped")), "{i}");
    }
    let half = spec_of(&e[4]).tls.as_ref().unwrap();
    assert!(half.client_cert.is_none() && half.client_key.is_none());
    assert!(e[4].notes.iter().any(|n| n.contains("pair")));
    assert_eq!(
        skip_of(&e[6]),
        &SkipReason::Invalid("SSH key path not usable".to_owned())
    );
}

#[test]
fn paths_of_the_other_os_are_dropped_with_notes() {
    // Absolute on the other OS only: a `C:\` path on unix, a `/home/...` path on Windows.
    #[cfg(unix)]
    let foreign = r"C:\\Users\\alice\\certs\\ca.pem";
    #[cfg(windows)]
    let foreign = "/home/alice/certs/ca.pem";
    let json = format!(
        r#"{{"Servers":{{
            "1":{{"Name":"tls","Host":"h","SSLMode":"verify-ca","SSLRootCert":"{foreign}"}},
            "2":{{"Name":"ssh","Host":"h","UseSSHTunnel":1,"TunnelHost":"b","TunnelUsername":"t",
                  "TunnelAuthentication":1,"TunnelIdentityFile":"{foreign}"}}
        }}}}"#
    );
    let r = PgAdmin.parse(json.as_bytes()).unwrap();
    let tls = spec_of(&r.entries[0]).tls.as_ref().unwrap();
    assert!(tls.ca_cert.is_none());
    assert!(r.entries[0].notes.iter().any(|n| n.contains("dropped")));
    assert_eq!(
        skip_of(&r.entries[1]),
        &SkipReason::Invalid("SSH key path not usable".to_owned())
    );
}

#[test]
fn crlf_and_bom_are_accepted() {
    let r = report("crlf-sample.crlf.json");
    assert_eq!(r.entries.len(), 1);
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend(fixture("full.json"));
    assert_eq!(PgAdmin.parse(&bytes).unwrap().entries.len(), 7);
}

#[test]
fn entries_are_ordered_by_numeric_key() {
    let json = br#"{"Servers":{"10":{"Name":"ten","Host":"h"},"2":{"Name":"two","Host":"h"},"b":{"Name":"b","Host":"h"},"a":{"Name":"a","Host":"h"}}}"#;
    let names: Vec<String> = PgAdmin
        .parse(json)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["two", "ten", "a", "b"]);
}

#[test]
fn name_and_field_hygiene() {
    let long = "x".repeat(5_000);
    let json = format!(
        r#"{{"Servers":{{"1":{{"Name":"  a\tb\u0000c  ","Host":"h","Username":"{long}"}},"2":{{"Name":"","Host":"h"}}}}}}"#
    );
    let r = PgAdmin.parse(json.as_bytes()).unwrap();
    assert_eq!(r.entries[0].name, "a b c");
    assert!(spec_of(&r.entries[0]).target.ends_with("/postgres"));
    assert!(r.entries[0].notes.iter().any(|n| n.contains("too long")));
    assert_eq!(r.entries[1].name, "pgAdmin server 2");
}

#[test]
fn too_many_entries() {
    let mut json = String::from("{\"Servers\":{");
    for i in 0..=MAX_ENTRIES {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!("\"{i}\":{{}}"));
    }
    json.push_str("}}");
    assert_eq!(
        PgAdmin.parse(json.as_bytes()),
        Err(ImportError::TooManyEntries { limit: MAX_ENTRIES })
    );
    // Exactly the limit is fine.
    let ok = json.replacen(&format!(",\"{MAX_ENTRIES}\":{{}}"), "", 1);
    assert_eq!(
        PgAdmin.parse(ok.as_bytes()).unwrap().entries.len(),
        MAX_ENTRIES
    );
}

#[test]
fn deep_nesting_is_malformed() {
    let depth = 200;
    let json = format!(
        "{{\"Servers\":{{\"1\":{}{}}}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    assert!(matches!(
        PgAdmin.parse(json.as_bytes()),
        Err(ImportError::Malformed { .. })
    ));
}

#[test]
fn oversized_file_is_refused_before_parsing() {
    let dir = std::env::temp_dir().join(format!("sqler-import-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("servers.json");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    drop(file);
    assert_eq!(
        read_source(&path),
        Err(ImportError::TooLarge {
            limit: MAX_FILE_BYTES
        })
    );
    // A directory and a missing file.
    assert_eq!(read_source(&dir), Err(ImportError::NotAFile));
    assert!(matches!(
        read_source(&dir.join("missing.json")),
        Err(ImportError::Io { .. })
    ));
    // A file at the limit is read.
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(MAX_FILE_BYTES).unwrap();
    drop(file);
    assert_eq!(read_source(&path).unwrap().len() as u64, MAX_FILE_BYTES);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn read_source_reads_a_fixture() {
    let bytes = read_source(&fixture_path("full.json")).unwrap();
    assert_eq!(bytes, fixture("full.json"));
}

#[test]
fn deterministic_garbage_never_panics() {
    // A fixed xorshift stream: the same inputs on every run and every OS.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for len in [0_usize, 1, 2, 7, 64, 513, 4_096] {
        for _ in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|_| next().to_le_bytes()[0]).collect();
            let _ = PgAdmin.parse(&bytes);
        }
    }
}

proptest! {
    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = PgAdmin.parse(&bytes);
    }

    #[test]
    fn mutated_fixtures_never_panic(
        which in 0usize..5,
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..24),
        cut in any::<usize>(),
    ) {
        let name = ["full.json", "wrong-types.json", "future.json", "paths.json", "duplicates.json"][which];
        let mut bytes = fixture(name);
        for (pos, byte) in edits {
            let len = bytes.len();
            bytes[pos % len] = byte;
        }
        let end = cut % (bytes.len() + 1);
        bytes.truncate(end.max(1));
        let _ = PgAdmin.parse(&bytes);
    }
}
