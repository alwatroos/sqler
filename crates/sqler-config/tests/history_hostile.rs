// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Hostile-input checks for the history reader and the credential redactor.

#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::assert_is_empty
)]

use std::fs;
use std::time::{Duration, SystemTime};

use sqler_config::{
    ConfigDir, ConnectionId, ConnectionSpec, ConnectionStore, HistoryOptions, HistoryOutcome,
    HistoryQuery, HistoryStore, MAX_HISTORY_SQL_BYTES, NewHistoryEntry, redact_credentials,
};
use sqler_core::DriverId;

fn conn() -> ConnectionId {
    let mut s = ConnectionStore::new();
    s.add_connection(
        ConnectionSpec::new("c", DriverId::new("sqlite").unwrap(), "x.db"),
        None,
    )
    .unwrap()
}

/// xorshift, deterministic.
fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn reader_survives_garbage() {
    let dir = std::env::temp_dir().join(format!("sqler-hostile-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = ConfigDir::new(&dir).history_path();
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    for round in 0..30 {
        let mut bytes = Vec::new();
        for _ in 0..(rng(&mut seed) % 40) {
            match rng(&mut seed) % 8 {
                0 => bytes.extend_from_slice(b"\n"),
                1 => bytes.extend_from_slice(b"\0\0\0"),
                2 => bytes.extend_from_slice(&[0xff, 0xfe, 0xc0, 0x80]),
                3 => bytes.extend_from_slice(br#"{"v":1,"started_ms":1,"connection":"c","duration_ms":1,"outcome":"ok","sql":"x"}"#),
                4 => bytes.extend_from_slice(br#"{"v":99999999999999999999999}"#),
                5 => bytes.extend(std::iter::repeat_n(b'a', 2_000_000)),
                6 => bytes.extend_from_slice(b"{\"v\":1,\"sql\":\"\xff\"}\r\n"),
                _ => bytes.extend((0..64).map(|_| rng(&mut seed) as u8)),
            }
        }
        fs::write(&path, &bytes).unwrap();
        let store = HistoryStore::open(&path, HistoryOptions::default())
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        let _ = store.issues();
        let _ = store.search(&HistoryQuery::default());
        // Appending after garbage must work and keep the bad bytes.
        store
            .record(NewHistoryEntry {
                sql: "select 1".into(),
                connection: conn(),
                started_at: SystemTime::now(),
                duration: Duration::from_millis(1),
                outcome: HistoryOutcome::Ok,
                rows: None,
            })
            .unwrap();
        let again = HistoryStore::open(&path, HistoryOptions::default()).unwrap();
        assert!(
            again
                .search(&HistoryQuery::default())
                .iter()
                .any(|e| e.sql() == "select 1")
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn redactor_hostile_sql() {
    let big = "'".repeat(1_000_001);
    let cases: Vec<String> = vec![
        String::new(),
        "'".into(),
        "$a$".into(),
        "$$".into(),
        "/*".into(),
        "-- PASSWORD 'x'".into(),
        "\"unterminated PASSWORD 'p".into(),
        "alter user x password 'unterminated".into(),
        "alter user x PASSWORD $t$secret$t$ ; select '\u{1F600}é'".into(),
        "select '\0' password '\0'".into(),
        "ALTER USER x PASSWORD  =  'p'".into(),
        "create user u identified by 'p''q'".into(),
        "/* /* nested */ PASSWORD 'p' */".into(),
        big,
        "password ".repeat(200_000),
        "$a$".repeat(300_000),
        "'".to_owned() + &"é".repeat(500_000),
    ];
    for c in &cases {
        let t = std::time::Instant::now();
        let out = redact_credentials(c);
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "slow on {:?}",
            &c[..c.len().min(30)]
        );
        let _ = out.len();
    }
    assert!(!redact_credentials("alter user x password 'hunter2'").contains("hunter2"));
    assert!(!redact_credentials("alter user x password 'hun''ter2'").contains("ter2"));
    assert!(!redact_credentials("alter user x password $t$hunter2$t$").contains("hunter2"));
    assert!(!redact_credentials("alter user x PASSWORD /* c */ 'hunter2'").contains("hunter2"));
    assert!(!redact_credentials("create user u IdEnTiFiEd by 'hunter2'").contains("hunter2"));
    assert!(!redact_credentials("attach 'postgres://u:hunter2@h/d'").contains("hunter2"));
    // unterminated literal after the keyword
    let r = redact_credentials("alter user x password 'hunter2");
    eprintln!("unterminated -> {r:?}");
}

#[test]
fn huge_sql_is_truncated_on_record() {
    let dir = std::env::temp_dir().join(format!("sqler-hostile2-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let path = ConfigDir::new(&dir).history_path();
    let store = HistoryStore::open(&path, HistoryOptions::default()).unwrap();
    store
        .record(NewHistoryEntry {
            sql: "é\0\"\\\n".repeat(400_000),
            connection: conn(),
            started_at: SystemTime::UNIX_EPOCH - Duration::from_secs(5),
            duration: Duration::MAX,
            outcome: HistoryOutcome::Error,
            rows: Some(u64::MAX),
        })
        .unwrap();
    let re = HistoryStore::open(&path, HistoryOptions::default()).unwrap();
    assert!(re.issues().is_empty());
    let e = &re.search(&HistoryQuery::default())[0];
    assert!(e.sql().len() <= MAX_HISTORY_SQL_BYTES && e.truncated());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn megabyte_bulk_insert_is_cut_then_redacted() {
    let dir = std::env::temp_dir().join(format!("sqler-hostile3-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let path = ConfigDir::new(&dir).history_path();
    let store = HistoryStore::open(&path, HistoryOptions::default()).unwrap();
    let mut sql = String::from("insert into users (name, password_hash) values ");
    while sql.len() < 1 << 20 {
        sql.push_str("('alice', '$2b$12$abcdefghijklmnopqrstuv'),");
    }
    let t = std::time::Instant::now();
    store
        .record(NewHistoryEntry {
            sql,
            connection: conn(),
            started_at: SystemTime::now(),
            duration: Duration::from_millis(1),
            outcome: HistoryOutcome::Ok,
            rows: None,
        })
        .unwrap();
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    let e = &store.search(&HistoryQuery::default())[0];
    assert!(e.truncated());
    assert!(e.sql().len() <= MAX_HISTORY_SQL_BYTES);
    assert!(!e.sql().contains("alice"));
    let _ = fs::remove_dir_all(&dir);
}
