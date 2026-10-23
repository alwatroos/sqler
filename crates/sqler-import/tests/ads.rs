// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The Azure Data Studio source against fixture files, hostile inputs and random bytes.

#![allow(
    clippy::unwrap_used,
    clippy::assert_is_empty,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::format_push_string
)]

use std::path::PathBuf;

use proptest::prelude::*;
use sqler_config::{ConnectionStore, ProfileSecretKind, TlsMode};
use sqler_core::DriverId;
use sqler_import::{
    AzureDataStudio, EntryOutcome, HostEnv, HostOs, ImportError, ImportOptions, MAX_ENTRIES,
    MAX_GROUP_DEPTH, PlanAction, SkipReason, Source, SourceEntry, SourceReport, plan, source,
    sources,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("ads")
            .join(name),
    )
    .unwrap()
}

fn report(name: &str) -> SourceReport {
    AzureDataStudio.parse(&fixture(name)).unwrap()
}

fn parse_str(s: &str) -> Result<SourceReport, ImportError> {
    AzureDataStudio.parse(s.as_bytes())
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

fn one(options: &str, provider: &str) -> SourceEntry {
    let text = format!(
        r#"{{"datasource.connections":[{{"options":{options},"providerName":"{provider}"}}]}}"#
    );
    parse_str(&text).unwrap().entries.remove(0)
}

#[test]
fn registry_has_azure_data_studio() {
    assert!(sources().iter().any(|s| s.id() == "azure-data-studio"));
    assert!(sources().iter().any(|s| s.id() == "pgadmin"));
    let s = source("azure-data-studio").unwrap();
    assert_eq!(s.label(), "Azure Data Studio");
}

#[test]
fn locations_are_os_aware() {
    let env = |os| HostEnv {
        os,
        home: Some(PathBuf::from("home").join("alice")),
        appdata: Some(PathBuf::from("appdata")),
    };
    let tail = PathBuf::from("azuredatastudio")
        .join("User")
        .join("settings.json");
    let linux = AzureDataStudio.default_locations(&env(HostOs::Linux));
    assert_eq!(
        linux[0],
        PathBuf::from("home")
            .join("alice")
            .join(".config")
            .join(&tail)
    );
    let win = AzureDataStudio.default_locations(&env(HostOs::Windows));
    assert_eq!(win[0], PathBuf::from("appdata").join(&tail));
    let mac = AzureDataStudio.default_locations(&env(HostOs::Mac));
    assert!(mac[0].ends_with(&tail));
    assert!(mac[0].starts_with(PathBuf::from("home").join("alice").join("Library")));
}

#[test]
fn full_fixture_maps_connections_and_groups() {
    let r = report("full.json");
    assert_eq!(r.source, "azure-data-studio");
    assert!(r.notices.is_empty());
    assert_eq!(r.entries.len(), 4);

    let a = &r.entries[0];
    assert_eq!(a.name, "Orders primary");
    assert_eq!(a.group, vec!["Shop".to_owned()]);
    let spec = spec_of(a);
    assert_eq!(spec.driver.as_str(), "sqlserver");
    assert_eq!(
        spec.target,
        "sqlserver://db.example.test:14330/orders?user=alice"
    );
    assert!(!spec.target.contains('@'));
    assert_eq!(spec.color.as_deref(), Some("#aa3300"));
    assert!(!spec.password_in_keychain);
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::VerifyFull);
    assert_eq!(a.secrets, vec![ProfileSecretKind::DbPassword]);
    assert_eq!(a.unsupported, vec!["options.\"connectTimeout\"".to_owned()]);
    assert!(a.notes.is_empty());

    let b = &r.entries[1];
    assert_eq!(b.group, vec!["Shop".to_owned(), "Staging".to_owned()]);
    let spec = spec_of(b);
    assert_eq!(
        spec.target,
        "sqlserver://staging.example.test:1433?user=bob"
    );
    assert_eq!(
        spec.color.as_deref(),
        Some("#aa3300"),
        "nearest group colour"
    );
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::Require);

    let c = &r.entries[2];
    assert_eq!(c.name, "Reporting (PostgreSQL)");
    assert_eq!(c.group, Vec::<String>::new(), "ROOT is not a group");
    let spec = spec_of(c);
    assert_eq!(spec.driver.as_str(), "postgres");
    assert_eq!(
        spec.target,
        "postgres://pg.example.test:5433/reports?user=carol"
    );
    assert!(spec.tls.is_none());
    assert_eq!(spec.color, None, "the ROOT colour is not used");

    let d = &r.entries[3];
    assert_eq!(d.name, "Ungrouped");
    let spec = spec_of(d);
    assert_eq!(spec.target, "sqlserver://[2001:db8::1]:1444?user=dave");
    assert!(spec.tls.is_none());
}

#[test]
fn unsupported_entries_are_skipped_with_a_reason() {
    let r = report("skipped.json");
    assert_eq!(r.entries.len(), 9);
    let messages: Vec<String> = r.entries.iter().map(|e| skip_of(e).message()).collect();
    assert!(messages[0].contains("Windows"), "{}", messages[0]);
    assert!(messages[1].contains("Azure Active Directory"));
    assert!(messages[2].contains("only SQL Server and PostgreSQL"));
    assert!(messages[3].contains("named SQL Server instances"));
    assert!(messages[4].contains("user is missing"));
    assert!(messages[5].contains("port"));
    assert!(messages[6].contains("provider is missing"));
    assert!(messages[7].contains("not an object"));
    assert!(messages[8].contains("no options"));
    assert!(matches!(skip_of(&r.entries[0]), SkipReason::Unsupported(_)));
    assert!(r.entries.iter().all(|e| e.secrets.is_empty()));
    // The Azure account fields are listed by name only.
    assert!(
        r.entries[1]
            .unsupported
            .contains(&"options.\"azureTenantId\"".to_owned())
    );
    // A skipped provider is named in a note.
    assert!(r.entries[2].notes.iter().any(|n| n.contains("KUSTO")));
    assert_eq!(r.entries[7].name, "Azure Data Studio connection 8");
}

#[test]
fn future_fields_are_listed_by_name_and_no_secret_leaks() {
    let r = report("future.json");
    assert_eq!(r.entries.len(), 1);
    let e = &r.entries[0];
    let spec = spec_of(e);
    assert_eq!(spec.tls.as_ref().unwrap().mode, TlsMode::VerifyFull);
    for name in [
        "\"futureKey\"",
        "options.\"futureOption\"",
        "options.\"multiSubnetFailover\"",
        "options.\"hostNameInCertificate\"",
    ] {
        assert!(e.unsupported.contains(&name.to_owned()), "{name}");
    }
    assert!(!e.unsupported.iter().any(|n| n.contains("password")));
    assert!(e.notes.iter().any(|n| n.contains("stored password")));
    assert!(e.notes.iter().any(|n| n.contains("strict")));
    assert!(!format!("{r:?}").contains("hunter2"));
    assert!(!spec.target.contains("hunter2"));
    assert!(!spec.password_in_keychain);
}

#[test]
fn encryption_maps_to_tls_modes() {
    let r = report("tls.json");
    let modes: Vec<Option<TlsMode>> = r
        .entries
        .iter()
        .map(|e| spec_of(e).tls.as_ref().map(|t| t.mode))
        .collect();
    assert_eq!(
        modes,
        vec![
            None,
            Some(TlsMode::VerifyFull),
            Some(TlsMode::Require),
            Some(TlsMode::Prefer),
            Some(TlsMode::Prefer),
            Some(TlsMode::VerifyFull),
            None,
            None,
        ]
    );
    assert!(r.entries[3].notes.iter().any(|n| n.contains("prefer")));
    assert!(r.entries[6].notes.iter().any(|n| n.contains("not known")));
    // The target never carries a TLS query key.
    for e in &r.entries {
        let t = &spec_of(e).target;
        assert_eq!(t.matches('?').count(), 1, "{t}");
        assert!(!t.contains("ssl") && !t.contains("encrypt"), "{t}");
    }
    // PostgreSQL does not map `encrypt`.
    assert!(
        r.entries[7]
            .unsupported
            .contains(&"options.\"encrypt\"".to_owned())
    );
}

#[test]
fn wrong_types_give_a_partial_report() {
    let r = report("wrong-types.json");
    assert_eq!(r.entries.len(), 4);
    assert!(matches!(
        skip_of(&r.entries[0]),
        SkipReason::Unsupported(_) | SkipReason::Invalid(_)
    ));
    assert!(
        r.entries[0]
            .notes
            .iter()
            .any(|n| n.contains("unexpected type"))
    );
    // A provider of the wrong type counts as missing.
    assert!(skip_of(&r.entries[1]).message().contains("provider"));
    assert!(skip_of(&r.entries[2]).message().contains("no options"));
    // PostgreSQL entry with SQL login and no `database` falls back to the default database.
    assert_eq!(
        spec_of(&r.entries[3]).target,
        "postgres://c.example.test:5432/postgres?user=alice"
    );
}

#[test]
fn group_paths_are_capped_and_cycles_do_not_loop() {
    let r = report("groups.json");
    assert_eq!(r.entries.len(), 4);
    assert_eq!(r.notices.len(), 1);
    assert!(r.notices[0].starts_with("4 "), "{:?}", r.notices);

    let deep = &r.entries[0];
    assert_eq!(deep.group.len(), MAX_GROUP_DEPTH);
    assert_eq!(deep.group.first().map(String::as_str), Some("A"));
    assert_eq!(deep.group.last().map(String::as_str), Some("H"));
    assert!(deep.notes.iter().any(|n| n.contains("folded")));

    let looped = &r.entries[1];
    assert!(looped.group.is_empty());
    assert!(looped.notes.iter().any(|n| n.contains("cyclic")));

    assert_eq!(r.entries[2].group, vec!["Orphan".to_owned()]);
    let unknown = &r.entries[3];
    assert!(unknown.group.is_empty());
    assert!(unknown.notes.iter().any(|n| n.contains("not found")));
}

#[test]
fn comments_and_trailing_commas_are_tolerated() {
    let text = "{ // c\n \"datasource.connections\": [ /* x */ { \"options\": { \"server\": \"h.example.test\", \"user\": \"alice\", }, \"providerName\": \"MSSQL\", }, ], }";
    let r = parse_str(text).unwrap();
    assert_eq!(
        spec_of(&r.entries[0]).target,
        "sqlserver://h.example.test:1433?user=alice"
    );
    // `//` inside a string is data, not a comment.
    let r = parse_str(
        r#"{"datasource.connections":[{"options":{"connectionName":"a // b /* c */","server":"h.example.test","user":"u"},"providerName":"MSSQL"}]}"#,
    )
    .unwrap();
    assert_eq!(r.entries[0].name, "a // b /* c */");
}

#[test]
fn empty_and_unrecognised_files() {
    for bad in ["", "   ", "[]", "3", "{}", r#"{"editor.fontSize": 13}"#] {
        assert!(parse_str(bad).is_err(), "{bad:?}");
    }
    assert!(matches!(
        parse_str(r#"{"datasource.connections": 3}"#),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    assert!(matches!(
        parse_str(r#"{"datasource.connections": {}}"#),
        Err(ImportError::UnrecognisedFormat(_))
    ));
    // An empty list and a file with only groups are valid and empty.
    assert!(
        parse_str(r#"{"datasource.connections": []}"#)
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(
        parse_str(r#"{"datasource.connectionGroups": []}"#)
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(matches!(
        AzureDataStudio.parse(&[0xff, 0xfe, b'{']),
        Err(ImportError::NotUtf8)
    ));
}

#[test]
fn malformed_files_report_a_position_and_never_echo_content() {
    let e = AzureDataStudio
        .parse(&fixture("malformed.json"))
        .unwrap_err();
    match &e {
        ImportError::Malformed { line, .. } => assert_eq!(*line, 3),
        other => panic!("{other:?}"),
    }
    assert!(!e.to_string().contains("example.test"));
    assert!(!e.to_string().contains("alice"));

    let e = parse_str("{\n \"password\": \"hunter2\" /* never closed").unwrap_err();
    assert!(matches!(e, ImportError::Malformed { line: 2, .. }));
    assert!(!e.to_string().contains("hunter2"));
    let e = parse_str(r#"{"datasource.connections": [ "hunter2" x ]}"#).unwrap_err();
    assert!(!e.to_string().contains("hunter2"));
}

#[test]
fn bom_and_crlf_are_accepted() {
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend(fixture("full.json"));
    assert_eq!(AzureDataStudio.parse(&bytes).unwrap().entries.len(), 4);
    let crlf = String::from_utf8(fixture("full.json"))
        .unwrap()
        .replace('\n', "\r\n");
    let r = AzureDataStudio.parse(crlf.as_bytes()).unwrap();
    assert_eq!(r, report("full.json"));
}

#[test]
fn hostile_values_are_skipped_or_encoded() {
    for server in [
        "evil@host.example.test",
        "host.example.test/x",
        "host.example.test?x=1",
        "ho st",
        "host#frag",
        "[::1",
        "host:1433",
        "host,12x",
        "host,0",
        "host,",
        "",
        "(localdb)\\\\MSSQLLocalDB",
        "h\u{0}ost",
    ] {
        let e = one(
            &format!(
                r#"{{"server":{},"authenticationType":"SqlLogin","user":"u"}}"#,
                serde_json::to_string(server).unwrap()
            ),
            "MSSQL",
        );
        assert!(
            matches!(e.outcome, EntryOutcome::Skipped(_)),
            "{server:?}: {:?}",
            e.outcome
        );
    }
    let e = one(
        r#"{"server":"h.example.test","database":"a/b?c","authenticationType":"SqlLogin","user":"a&user=b@c"}"#,
        "MSSQL",
    );
    assert_eq!(
        spec_of(&e).target,
        "sqlserver://h.example.test:1433/a%2Fb%3Fc?user=a%26user%3Db%40c"
    );
    let e = one(
        r#"{"server":" TCP:Host.example.test , 1500 ","authenticationType":"sqllogin","user":"u"}"#,
        "mssql",
    );
    assert_eq!(
        spec_of(&e).target,
        "sqlserver://Host.example.test:1500?user=u"
    );
    let e = one(r#"{"server":".","user":"u"}"#, "MSSQL");
    assert_eq!(spec_of(&e).target, "sqlserver://localhost:1433?user=u");
    // Multibyte text near the `tcp:` prefix check must not panic.
    let e = one(r#"{"server":"ééé","user":"u"}"#, "MSSQL");
    assert!(matches!(
        e.outcome,
        EntryOutcome::Mapped(_) | EntryOutcome::Skipped(_)
    ));
}

#[test]
fn default_database_marker_means_no_database() {
    for db in ["<default>", "", "  "] {
        let e = one(
            &format!(r#"{{"server":"h.example.test","user":"u","database":"{db}"}}"#),
            "MSSQL",
        );
        assert_eq!(
            spec_of(&e).target,
            "sqlserver://h.example.test:1433?user=u",
            "{db:?}"
        );
    }
    let e = one(
        r#"{"server":"h.example.test","user":"u","database":"<default>"}"#,
        "PGSQL",
    );
    assert_eq!(
        spec_of(&e).target,
        "postgres://h.example.test:5432/postgres?user=u"
    );
}

#[test]
fn too_long_server_is_noted_once() {
    let long = "x".repeat(5_000);
    let e = one(&format!(r#"{{"server":"{long}","user":"u"}}"#), "MSSQL");
    let n = e.notes.iter().filter(|n| n.contains("too long")).count();
    assert_eq!(n, 1, "{:?}", e.notes);
}

#[test]
fn secret_like_option_names_are_not_listed() {
    let e = one(
        r#"{"server":"h.example.test","user":"u","accessToken":"t","clientSecret":"s","sslKey":"k","pwd":"p","futureOption":1}"#,
        "MSSQL",
    );
    assert_eq!(e.unsupported, vec!["options.\"futureOption\"".to_owned()]);
    let text = format!("{e:?}");
    for s in ["accessToken", "clientSecret", "sslKey", "pwd"] {
        assert!(!text.contains(s), "{s}");
    }
    assert!(e.secrets.contains(&ProfileSecretKind::DbPassword));
}

#[test]
fn names_and_long_fields_are_cleaned() {
    let e = one(
        r#"{"connectionName":"  a\u0007b\nc  ","server":"h.example.test","user":"u"}"#,
        "MSSQL",
    );
    assert_eq!(e.name, "a b c");
    let long = "x".repeat(5_000);
    let e = one(
        &format!(r#"{{"connectionName":"{long}","server":"h.example.test","user":"u"}}"#),
        "MSSQL",
    );
    assert!(e.name.starts_with("h.example.test") || e.name.starts_with("Azure"));
    assert!(e.notes.iter().any(|n| n.contains("too long")));
    let key = "k".repeat(500);
    let e = one(
        &format!(r#"{{"server":"h.example.test","user":"u","{key}":1}}"#),
        "MSSQL",
    );
    assert!(e.unsupported[0].chars().count() < 100);
}

#[test]
fn too_many_entries() {
    let item = r#"{"options":{"server":"h.example.test","user":"u"},"providerName":"MSSQL"}"#;
    let ok = vec![item; MAX_ENTRIES].join(",");
    let r = parse_str(&format!(r#"{{"datasource.connections":[{ok}]}}"#)).unwrap();
    assert_eq!(r.entries.len(), MAX_ENTRIES);
    let over = vec![item; MAX_ENTRIES + 1].join(",");
    assert_eq!(
        parse_str(&format!(r#"{{"datasource.connections":[{over}]}}"#)).unwrap_err(),
        ImportError::TooManyEntries { limit: MAX_ENTRIES }
    );
    let groups = vec![r#"{"id":"a","name":"A"}"#; MAX_ENTRIES + 1].join(",");
    assert_eq!(
        parse_str(&format!(r#"{{"datasource.connectionGroups":[{groups}]}}"#)).unwrap_err(),
        ImportError::TooManyEntries { limit: MAX_ENTRIES }
    );
}

#[test]
fn deep_nesting_is_malformed() {
    let deep = format!(
        r#"{{"datasource.connections":[{{"options":{}1{}}}]}}"#,
        "[".repeat(500),
        "]".repeat(500)
    );
    assert!(matches!(
        parse_str(&deep),
        Err(ImportError::Malformed { .. })
    ));
}

#[test]
fn many_commas_and_comments_stay_linear() {
    let mut text = String::from(r#"{"datasource.connections":["#);
    for _ in 0..20_000 {
        text.push_str("/*c*/ , ");
    }
    text.push_str("]}");
    assert!(parse_str(&text).is_err());
    let text = format!(
        r#"{{"a":[{}]}}"#,
        "1,".repeat(100_000) + &" ".repeat(100_000)
    );
    let _ = parse_str(&text);
}

#[test]
fn plan_uses_the_callers_driver_list() {
    let r = report("full.json");
    let store = ConnectionStore::new();
    let none = plan(&r, &store, &ImportOptions::default());
    assert!(matches!(
        none.items[0].action,
        PlanAction::Skip(SkipReason::DriverUnavailable { .. })
    ));
    let with = plan(
        &r,
        &store,
        &ImportOptions {
            available_drivers: vec![
                DriverId::new("sqlserver").unwrap(),
                DriverId::new("postgres").unwrap(),
            ],
        },
    );
    assert!(
        with.items
            .iter()
            .all(|i| matches!(i.action, PlanAction::Add { .. }))
    );
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
            let _ = AzureDataStudio.parse(&bytes);
        }
    }
}

proptest! {
    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = AzureDataStudio.parse(&bytes);
    }

    #[test]
    fn mutated_fixtures_never_panic(
        which in 0usize..6,
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..24),
        cut in any::<usize>(),
    ) {
        let name = ["full.json", "skipped.json", "future.json", "groups.json", "tls.json", "wrong-types.json"][which];
        let mut bytes = fixture(name);
        for (pos, byte) in edits {
            let len = bytes.len();
            bytes[pos % len] = byte;
        }
        let end = cut % (bytes.len() + 1);
        bytes.truncate(end.max(1));
        let _ = AzureDataStudio.parse(&bytes);
    }
}
