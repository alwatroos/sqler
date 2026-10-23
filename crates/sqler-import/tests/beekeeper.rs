// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `Beekeeper Studio` source against fixture files, hostile inputs and random bytes.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::format_push_string,
    clippy::needless_pass_by_value,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]

use std::path::PathBuf;

use proptest::prelude::*;
use sqler_config::{ConnectionStore, ProfileSecretKind, SshAuth, TlsMode};
use sqler_import::{
    Beekeeper, EntryOutcome, ImportError, ImportOptions, MAX_ENTRIES, SkipReason, Source,
    SourceEntry, SourceReport, plan, source, sources,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("beekeeper")
            .join(name),
    )
    .unwrap()
}

/// The directory prefix the fixtures use for absolute paths, and its form on this OS.
const FIXTURE_HOME: &str = "/home/alice/";
#[cfg(unix)]
const HOME: &str = "/home/alice/";
#[cfg(windows)]
const HOME: &str = "C:\\Users\\alice\\";

fn native(tail: &str) -> PathBuf {
    PathBuf::from(format!("{HOME}{tail}"))
}

/// Fixture bytes with every `/home/alice/` path rewritten to an absolute path of this OS.
fn portable(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    text.replace(FIXTURE_HOME, &HOME.replace('\\', "\\\\"))
        .into_bytes()
}

fn report(name: &str) -> SourceReport {
    Beekeeper.parse(&portable(&fixture(name))).unwrap()
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
fn registry_has_beekeeper() {
    assert!(sources().iter().any(|s| s.id() == "beekeeper"));
    assert!(sources().iter().any(|s| s.id() == "pgadmin"));
    assert_eq!(source("beekeeper").unwrap().label(), "Beekeeper Studio");
}

#[test]
fn full_fixture_maps_every_field() {
    let r = report("full.json");
    assert_eq!(r.source, "beekeeper");
    assert!(r.notices.is_empty());
    assert_eq!(r.entries.len(), 9);

    let a = &r.entries[0];
    assert_eq!(a.name, "Orders primary");
    assert_eq!(a.group, vec!["Shop".to_owned(), "Staging".to_owned()]);
    let spec = spec_of(a);
    assert_eq!(spec.driver.as_str(), "postgres");
    assert_eq!(
        spec.target,
        "postgres://db.example.test:5433/orders?user=alice"
    );
    assert_eq!(spec.color.as_deref(), Some("#3182ce"));
    assert!(!spec.password_in_keychain);
    let tls = spec.tls.as_ref().unwrap();
    assert_eq!(tls.mode, TlsMode::VerifyFull);
    assert_eq!(tls.ca_cert, Some(native("certs/ca.pem")));
    assert_eq!(tls.client_cert, Some(native("certs/client.pem")));
    assert_eq!(tls.client_key, Some(native("certs/client.key")));
    let ssh = spec.ssh.as_ref().unwrap();
    assert_eq!(
        (ssh.host.as_str(), ssh.port, ssh.user.as_str()),
        ("bastion.example.test", 2222, "tunnel")
    );
    assert!(matches!(
        &ssh.auth,
        SshAuth::KeyFile { path, passphrase_in_keychain: false } if *path == native(".ssh/id_test")
    ));
    assert_eq!(
        a.secrets,
        vec![
            ProfileSecretKind::DbPassword,
            ProfileSecretKind::SshPassphrase
        ]
    );
    assert_eq!(a.unsupported, vec!["\"readOnlyMode\"".to_owned()]);

    // MariaDB maps to the mysql driver; string port; colour hex; ssl without verification.
    let b = &r.entries[1];
    let spec = spec_of(b);
    assert_eq!(spec.driver.as_str(), "mysql");
    assert_eq!(spec.target, "mysql://bob@maria.example.test:3307/reports");
    assert_eq!(spec.color.as_deref(), Some("#aa3300"));
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::Require);
    assert_eq!(b.group, vec!["Shop".to_owned()]);
    assert_eq!(b.secrets, vec![ProfileSecretKind::DbPassword]);

    // SQLite: the path is the target.
    let c = &r.entries[2];
    let spec = spec_of(c);
    assert_eq!(spec.driver.as_str(), "sqlite");
    assert_eq!(PathBuf::from(&spec.target), native("data/notes.sqlite3"));
    assert!(c.secrets.is_empty());
    assert_eq!(spec.color.as_deref(), Some("#38a169"));

    // SQL Server: sqlserver driver and target, default port, SSH password.
    let d = &r.entries[3];
    let spec = spec_of(d);
    assert_eq!(spec.driver.as_str(), "sqlserver");
    assert_eq!(
        spec.target,
        "sqlserver://sa_alice@mssql.example.test:1433/ledger"
    );
    assert!(!spec.target.contains("fake"));
    assert_eq!(spec.ssh.as_ref().unwrap().auth, SshAuth::Password);
    assert_eq!(d.secrets, vec![ProfileSecretKind::SshPassword]);
    assert!(d.notes.iter().any(|n| n.contains("password")));

    // DuckDB and the other engines are skipped.
    assert_eq!(
        skip_of(&r.entries[4]),
        &SkipReason::NotInThisRelease { engine: "DuckDB" }
    );
    assert_eq!(
        skip_of(&r.entries[4]).message(),
        "DuckDB is not in SQLer 1.0"
    );
    assert_eq!(
        skip_of(&r.entries[5]),
        &SkipReason::Unsupported("MongoDB connections are not supported".to_owned())
    );
    assert_eq!(
        skip_of(&r.entries[6]),
        &SkipReason::Unsupported("CockroachDB connections are not supported".to_owned())
    );
    assert_eq!(
        skip_of(&r.entries[7]),
        &SkipReason::Unsupported("this connection type is not supported".to_owned())
    );

    // ssl false disables TLS; IPv6 host; agent tunnel needs no secret.
    let e = &r.entries[8];
    let spec = spec_of(e);
    assert_eq!(spec.target, "postgres://[::1]:5432/postgres?user=carol");
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::Disable);
    assert_eq!(spec.ssh.as_ref().unwrap().auth, SshAuth::Agent);
    assert_eq!(spec.ssh.as_ref().unwrap().port, 22);
    assert_eq!(e.secrets, vec![ProfileSecretKind::DbPassword]);
}

#[test]
fn plaintext_passwords_never_reach_any_output() {
    let bytes = portable(&fixture("full.json"));
    let r = Beekeeper.parse(&bytes).unwrap();
    let opts = ImportOptions {
        available_drivers: ["postgres", "mysql", "sqlite", "sqlserver"]
            .iter()
            .map(|d| sqler_core::DriverId::new(d).unwrap())
            .collect(),
    };
    let p = plan(&r, &ConnectionStore::new(), &opts);
    let everything = format!("{r:?} {r:#?} {p:?} {p:#?}");
    for needle in ["fake-pw-123", "fake-ssh-pw-456", "fake-passphrase-789"] {
        assert!(!everything.contains(needle), "{needle}");
    }
    // The field names of secrets are not listed either.
    for e in &r.entries {
        assert!(
            !e.unsupported
                .iter()
                .any(|u| u.to_lowercase().contains("password") || u.contains("Url"))
        );
    }
    // A truncated copy fails without echoing the content.
    let cut = &bytes[..bytes.len() / 3];
    let e = Beekeeper.parse(cut).unwrap_err();
    assert!(!format!("{e} {e:?}").contains("fake-"));
}

#[test]
fn plan_adds_mapped_and_skips_the_rest() {
    let r = report("full.json");
    let opts = ImportOptions {
        available_drivers: ["postgres", "mysql", "sqlite"]
            .iter()
            .map(|d| sqler_core::DriverId::new(d).unwrap())
            .collect(),
    };
    let p = plan(&r, &ConnectionStore::new(), &opts);
    // Without the sqlserver driver the SQL Server entry is "not available in this build".
    assert!(matches!(
        &p.items[3].action,
        sqler_import::PlanAction::Skip(SkipReason::DriverUnavailable { .. })
    ));
    assert!(matches!(
        &p.items[0].action,
        sqler_import::PlanAction::Add { .. }
    ));
}

#[test]
fn array_form_and_folder_name() {
    let r = report("array.json");
    assert!(r.notices.is_empty());
    assert_eq!(r.entries.len(), 2);
    assert_eq!(r.entries[0].group, vec!["Team".to_owned()]);
    assert_eq!(
        spec_of(&r.entries[0]).target,
        "mysql://alice@mysql.example.test:3306"
    );
    assert!(spec_of(&r.entries[0]).tls.is_none());
}

#[test]
fn hostile_entries_are_skipped_or_noted() {
    let r = report("hostile.json");
    assert_eq!(r.notices.len(), 1);
    assert!(r.notices[0].contains("version"));
    let e = &r.entries;
    assert_eq!(e.len(), 17);
    assert!(matches!(skip_of(&e[0]), SkipReason::Invalid(m) if m.contains("not an object")));
    assert!(matches!(skip_of(&e[1]), SkipReason::Invalid(m) if m.contains("type")));
    assert_eq!(e[2].name, "Beekeeper connection 3");
    assert!(e[2].notes.len() >= 2);
    assert!(matches!(skip_of(&e[3]), SkipReason::Invalid(m) if m.contains("port")));
    assert!(matches!(skip_of(&e[4]), SkipReason::Invalid(m) if m.contains("host")));
    // Cyclic folders are cut with a note.
    assert!(spec_of(&e[5]).tls.is_none());
    assert_eq!(e[5].group.len(), 2);
    assert!(e[5].notes.iter().any(|n| n.contains("cyclic")));
    assert!(matches!(skip_of(&e[6]), SkipReason::Unsupported(m) if m.contains("socket")));
    assert!(matches!(skip_of(&e[7]), SkipReason::Unsupported(m) if m.contains("bastion")));
    assert!(matches!(skip_of(&e[8]), SkipReason::Invalid(m) if m.contains("user")));
    assert!(matches!(skip_of(&e[9]), SkipReason::Invalid(m) if m.contains("authentication")));
    assert_eq!(
        skip_of(&e[10]),
        &SkipReason::Invalid("SSH key path not usable".to_owned())
    );
    assert!(matches!(skip_of(&e[11]), SkipReason::Invalid(m) if m.contains("path")));
    assert!(matches!(skip_of(&e[12]), SkipReason::Invalid(m) if m.contains("path")));
    // Half pair and bad paths are dropped with notes.
    let t = spec_of(&e[13]).tls.as_ref().unwrap();
    assert!(t.client_cert.is_none() && t.client_key.is_none());
    assert!(e[13].notes.iter().any(|n| n.contains("pair")));
    let t = spec_of(&e[14]).tls.as_ref().unwrap();
    assert!(t.ca_cert.is_none());
    assert!(e[14].notes.iter().any(|n| n.contains("dropped")));
    assert!(e[14].notes.iter().any(|n| n.contains("colour")));
    // SQL Server drops client certificates.
    let t = spec_of(&e[15]).tls.as_ref().unwrap();
    assert!(t.client_cert.is_none() && t.client_key.is_none());
    assert!(matches!(skip_of(&e[16]), SkipReason::Invalid(m) if m.contains("type")));
}

#[test]
fn paths_of_the_other_os_are_dropped() {
    #[cfg(unix)]
    let foreign = r"C:\\Users\\alice\\ca.pem";
    #[cfg(windows)]
    let foreign = "/home/alice/ca.pem";
    let json = format!(
        r#"[{{"connectionType":"sqlite","name":"f","defaultDatabase":"{foreign}"}},
            {{"connectionType":"mysql","name":"t","host":"h","ssl":true,"sslCaFile":"{foreign}"}}]"#
    );
    let r = Beekeeper.parse(json.as_bytes()).unwrap();
    assert!(matches!(skip_of(&r.entries[0]), SkipReason::Invalid(_)));
    assert!(
        spec_of(&r.entries[1])
            .tls
            .as_ref()
            .unwrap()
            .ca_cert
            .is_none()
    );
}

#[test]
fn broken_files_give_typed_errors() {
    assert!(matches!(
        Beekeeper.parse(&fixture("malformed.json")),
        Err(ImportError::Malformed { .. })
    ));
    assert!(matches!(
        Beekeeper.parse(b""),
        Err(ImportError::Malformed { .. })
    ));
    assert!(matches!(
        Beekeeper.parse(&fixture("not-export.json")),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert!(matches!(
        Beekeeper.parse(b"\"text\""),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert!(matches!(
        Beekeeper.parse(b"{\"connections\": {}}"),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert_eq!(Beekeeper.parse(&[0xFF, 0xFE]), Err(ImportError::NotUtf8));
    assert!(Beekeeper.parse(b"[]").unwrap().entries.is_empty());
}

#[test]
fn secret_like_top_level_keys_are_not_listed() {
    let json = br#"{"connections":[],"apiToken":"x","dbPassword":"y","version":2}"#;
    let r = Beekeeper.parse(json).unwrap();
    assert_eq!(r.notices.len(), 1);
    assert!(r.notices[0].contains("version"));
    let all = r.notices.join(" ").to_ascii_lowercase();
    assert!(!all.contains("token") && !all.contains("password"));
}

#[test]
fn bom_and_crlf_are_accepted() {
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    let text = String::from_utf8(portable(&fixture("array.json"))).unwrap();
    bytes.extend(text.replace('\n', "\r\n").into_bytes());
    assert_eq!(Beekeeper.parse(&bytes).unwrap().entries.len(), 2);
}

#[test]
fn caps_and_field_hygiene() {
    let mut json = String::from("[");
    for i in 0..=MAX_ENTRIES {
        if i > 0 {
            json.push(',');
        }
        json.push_str("{}");
    }
    json.push(']');
    assert_eq!(
        Beekeeper.parse(json.as_bytes()),
        Err(ImportError::TooManyEntries { limit: MAX_ENTRIES })
    );
    let ok = json.replacen(",{}", "", 1);
    assert_eq!(
        Beekeeper.parse(ok.as_bytes()).unwrap().entries.len(),
        MAX_ENTRIES
    );

    let long = "x".repeat(5_000);
    let json = format!(
        r#"[{{"connectionType":"mysql","name":"  a\tb\u0000c  ","host":"h","username":"{long}"}}]"#
    );
    let r = Beekeeper.parse(json.as_bytes()).unwrap();
    assert_eq!(r.entries[0].name, "a b c");
    assert_eq!(spec_of(&r.entries[0]).target, "mysql://h:3306");
    assert!(r.entries[0].notes.iter().any(|n| n.contains("too long")));

    // Too many folders.
    let folders: Vec<String> = (0..=MAX_ENTRIES)
        .map(|i| format!("{{\"id\":{i}}}"))
        .collect();
    let json = format!(
        "{{\"connection_folders\":[{}],\"connections\":[]}}",
        folders.join(",")
    );
    assert_eq!(
        Beekeeper.parse(json.as_bytes()),
        Err(ImportError::TooManyEntries { limit: MAX_ENTRIES })
    );
}

#[test]
fn deep_nesting_is_malformed() {
    let json = format!("[{}{}]", "[".repeat(200), "]".repeat(200));
    assert!(matches!(
        Beekeeper.parse(json.as_bytes()),
        Err(ImportError::Malformed { .. })
    ));
}

#[test]
fn deep_folder_chain_is_bounded() {
    let mut folders = Vec::new();
    for i in 0..100 {
        folders.push(format!(
            "{{\"id\":{i},\"name\":\"f{i}\",\"parentId\":{}}}",
            i + 1
        ));
    }
    let json = format!(
        "{{\"connection_folders\":[{}],\"connections\":[{{\"connectionType\":\"sqlite\",\"name\":\"s\",\"connectionFolderId\":0,\"defaultDatabase\":\"{}\"}}]}}",
        folders.join(","),
        HOME.replace('\\', "\\\\") + "x.db"
    );
    let r = Beekeeper.parse(json.as_bytes()).unwrap();
    assert!(r.entries[0].group.len() <= 16);
}

#[test]
fn deterministic_garbage_never_panics() {
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
            let _ = Beekeeper.parse(&bytes);
        }
    }
}

proptest! {
    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Beekeeper.parse(&bytes);
    }

    #[test]
    fn mutated_fixtures_never_panic(
        which in 0usize..3,
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..24),
        cut in any::<usize>(),
    ) {
        let name = ["full.json", "hostile.json", "array.json"][which];
        let mut bytes = fixture(name);
        for (pos, byte) in edits {
            let len = bytes.len();
            bytes[pos % len] = byte;
        }
        let end = cut % (bytes.len() + 1);
        bytes.truncate(end.max(1));
        let _ = Beekeeper.parse(&bytes);
    }
}
