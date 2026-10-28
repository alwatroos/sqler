// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! QA probe of the chat history store: secrets in the file bytes, modes, caps, corrupt and newer files.

#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::assert_is_empty
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use sqler_config::{ChatHistory, ChatHistoryStatus, StoredEntry, StoredStatus};

const PG: &str = "postgres://alice:hunter2pw@db.example.com:5432/app";
const PW: &str = "password=SuperSecretValue9";
const SK: &str = "sk-ABCDEF1234567890abcdef1234567890XYZ";
const AK: &str = "AKIAIOSFODNN7EXAMPLE";
const SECRETS: [&str; 4] = [
    "hunter2pw",
    "SuperSecretValue9",
    "ABCDEF1234567890abcdef",
    "AKIAIOSFODNN7EXAMPLE",
];

fn dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("sqler-qa-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn side_files(db: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|s| PathBuf::from(format!("{}{s}", db.display())))
        .filter(|p| p.exists())
        .collect()
}

fn has(bytes: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    bytes.windows(n.len()).any(|w| w == n)
}

fn assert_clean(db: &Path) {
    for f in side_files(db) {
        let b = fs::read(&f).unwrap();
        for s in SECRETS.iter().chain([&PG, &PW, &SK, &AK]) {
            assert!(!has(&b, s), "{} holds {s}", f.display());
        }
    }
}

#[test]
fn qa_secrets_modes_caps_and_reopen() {
    let d = dir("main");
    let db = d.join("chat-history.db");
    let mut o = sqler_config::ChatHistoryOptions::default();
    o.flush_interval = Duration::from_millis(50);
    let h = ChatHistory::with_options(db.clone(), o.clone());
    h.list().unwrap();
    let id = ChatHistory::new_id();
    let text = format!("connect with {PG} and {PW} using {SK} and {AK}");
    let rows: Vec<_> = (0..500)
        .map(|i| json!({"id": i, "note": "x".repeat(2000)}))
        .collect();
    let entries = vec![
        (0, StoredEntry::User { text: text.clone() }),
        (1, StoredEntry::Assistant { text: text.clone() }),
        (
            2,
            StoredEntry::ToolCall {
                call_id: "c".into(),
                tool: "run_query".into(),
                arguments: json!({"sql": format!("select '{SK}'"), "dsn": PG, "password": "SuperSecretValue9"}),
                status: StoredStatus::Ok,
                result: json!({"columns": ["id", "note"], "rows": rows, "text": text}),
            },
        ),
        (
            3,
            StoredEntry::Sql {
                call_id: "c".into(),
                connection: "conn".into(),
                display_sql: format!("select * from t where k = '{AK}' -- {PW}"),
                class: "select".into(),
                approved: true,
                rows: 500,
                truncated: None,
                failed: false,
            },
        ),
    ];
    h.append(&id, "conn", entries);
    assert!(h.flush(Duration::from_secs(5)));
    // While open: side files must be private and clean too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in side_files(&db) {
            let m = fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(m, 0o600, "{} has mode {m:o}", f.display());
        }
    }
    assert_clean(&db);
    h.close(Duration::from_secs(5));
    assert_clean(&db);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in side_files(&db) {
            let m = fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(m, 0o600, "{} has mode {m:o}", f.display());
        }
    }

    let h = ChatHistory::with_options(db.clone(), o);
    let list = h.list().unwrap();
    assert_eq!(list.len(), 1);
    assert!(
        !list[0].title.contains("hunter2pw"),
        "title: {}",
        list[0].title
    );
    let c = h.load(&list[0].id).unwrap().unwrap();
    assert_eq!(c.entries.len(), 4);
    let all = serde_json::to_string(&c.entries).unwrap();
    for s in SECRETS {
        assert!(!all.contains(s), "loaded transcript holds {s}");
    }
    if let StoredEntry::ToolCall { result, .. } = &c.entries[2] {
        let n = result["rows"].as_array().map_or(0, Vec::len);
        assert!(n <= 50, "rows kept: {n}");
    }
    assert!(
        all.len() < 400 << 10,
        "stored transcript {} bytes",
        all.len()
    );
    eprintln!(
        "QA reopened: entries={} json_bytes={}",
        c.entries.len(),
        all.len()
    );
    eprintln!("QA entry0={:?}", c.entries[0]);
    eprintln!("QA entry3={:?}", c.entries[3]);
    h.close(Duration::from_secs(5));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn qa_corrupt_moved_aside_and_newer_untouched() {
    let d = dir("bad");
    let db = d.join("chat-history.db");
    fs::write(&db, vec![0xA5u8; 8192]).unwrap();
    let h = ChatHistory::open(db.clone());
    assert!(h.list().unwrap().is_empty());
    assert_eq!(h.status(), ChatHistoryStatus::Ready);
    let names: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    eprintln!("QA after corrupt: {names:?} notice={:?}", h.take_notice());
    assert!(names.len() >= 2, "{names:?}");
    h.close(Duration::from_secs(5));

    // newer schema
    let d2 = dir("newer");
    let db2 = d2.join("chat-history.db");
    let h = ChatHistory::open(db2.clone());
    h.list().unwrap();
    h.append(
        &ChatHistory::new_id(),
        "c",
        vec![(0, StoredEntry::User { text: "hi".into() })],
    );
    h.close(Duration::from_secs(5));
    let mut b = fs::read(&db2).unwrap();
    b[60..64].copy_from_slice(&99u32.to_be_bytes());
    fs::write(&db2, &b).unwrap();
    let before = fs::read(&db2).unwrap();
    let h = ChatHistory::open(db2.clone());
    assert!(h.list().is_err());
    assert!(matches!(h.status(), ChatHistoryStatus::Off(_)));
    h.append(
        &ChatHistory::new_id(),
        "c",
        vec![(0, StoredEntry::User { text: "no".into() })],
    );
    h.close(Duration::from_secs(2));
    assert_eq!(fs::read(&db2).unwrap(), before, "newer file was modified");
    let names: Vec<_> = fs::read_dir(&d2)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    eprintln!("QA newer dir: {names:?}");
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&d2);
}
