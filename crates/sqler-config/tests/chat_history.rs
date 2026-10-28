// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The chat history store: round trips, caps, scrubbing, file modes, corrupt and newer files.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqler_config::{
    ChatHistory, ChatHistoryError, ChatHistoryOptions, ChatHistoryStatus, ConversationId,
    StoredEntry, StoredNotice, StoredStatus,
};

const WAIT: Duration = Duration::from_secs(10);

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh temporary directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "sqler-chat-history-{}-{}-{test}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn db(&self) -> PathBuf {
        self.0.join("chat-history.db")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fast() -> ChatHistoryOptions {
    let mut o = ChatHistoryOptions::default();
    o.flush_interval = Duration::from_millis(50);
    o
}

fn open(path: &Path) -> ChatHistory {
    ChatHistory::with_options(path.to_owned(), fast())
}

fn ready(h: &ChatHistory) {
    // `list` waits for the open to finish.
    h.list().unwrap();
    assert_eq!(h.status(), ChatHistoryStatus::Ready);
}

fn user(text: &str) -> StoredEntry {
    StoredEntry::User { text: text.into() }
}

fn tool(arguments: Value, result: Value) -> StoredEntry {
    StoredEntry::ToolCall {
        call_id: "c1".into(),
        tool: "run_query".into(),
        arguments,
        status: StoredStatus::Ok,
        result,
    }
}

fn numbered(entries: Vec<StoredEntry>) -> Vec<(u64, StoredEntry)> {
    entries
        .into_iter()
        .enumerate()
        .map(|(i, e)| (i as u64, e))
        .collect()
}

fn bytes_of(path: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.file_name().unwrap().to_owned();
        name.push(suffix);
        all.extend(fs::read(path.with_file_name(name)).unwrap_or_default());
    }
    all
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn every_kind() -> Vec<StoredEntry> {
    vec![
        user("how many orders?"),
        StoredEntry::Assistant {
            text: "Let me look.".into(),
        },
        tool(
            json!({"sql": "select count(*) from orders"}),
            json!({"rows": [[3]]}),
        ),
        StoredEntry::ToolCall {
            call_id: "c2".into(),
            tool: "run_query".into(),
            arguments: Value::Null,
            status: StoredStatus::Error {
                code: "denied".into(),
                message: "not approved".into(),
            },
            result: Value::Null,
        },
        StoredEntry::ToolCall {
            call_id: "c3".into(),
            tool: "run_query".into(),
            arguments: Value::Null,
            status: StoredStatus::NotRun {
                reason: "limit:tool_calls".into(),
            },
            result: Value::Null,
        },
        StoredEntry::ToolCall {
            call_id: "c4".into(),
            tool: "run_query".into(),
            arguments: Value::Null,
            status: StoredStatus::Cancelled,
            result: Value::Null,
        },
        StoredEntry::Sql {
            call_id: "c1".into(),
            connection: "conn-1".into(),
            display_sql: "select count(*) from orders".into(),
            class: "read".into(),
            approved: false,
            rows: 1,
            truncated: Some("row_limit".into()),
            failed: false,
        },
        StoredEntry::Notice {
            notice: StoredNotice::Retrying {
                attempt: 1,
                delay_ms: 1000,
            },
        },
        StoredEntry::Notice {
            notice: StoredNotice::LimitReached {
                limit: "tokens".into(),
            },
        },
        StoredEntry::Notice {
            notice: StoredNotice::LoopDetected {
                tool: "run_query".into(),
            },
        },
        StoredEntry::Notice {
            notice: StoredNotice::Cancelled,
        },
        StoredEntry::Notice {
            notice: StoredNotice::ProviderError {
                message: "overloaded".into(),
            },
        },
        StoredEntry::Notice {
            notice: StoredNotice::OutputTruncated,
        },
    ]
}

#[test]
fn every_kind_survives_close_and_reopen() {
    let dir = Scratch::new("roundtrip");
    let id = ChatHistory::new_id();
    let h = open(&dir.db());
    h.append(&id, "conn-1", numbered(every_kind()));
    h.close(WAIT);

    let h = open(&dir.db());
    let list = h.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].connection, "conn-1");
    assert_eq!(list[0].title, "how many orders?");
    assert_eq!(list[0].entries, every_kind().len() as u64);
    let loaded = h.load(&id).unwrap().unwrap();
    assert_eq!(loaded.first_seq, 0);
    assert_eq!(loaded.entries, every_kind());
    assert!(h.load(&ChatHistory::new_id()).unwrap().is_none());
    h.close(WAIT);
}

#[test]
fn ids_are_unique() {
    assert_ne!(ChatHistory::new_id(), ChatHistory::new_id());
}

#[test]
fn list_is_newest_first_and_trim_delete_clear_work() {
    let dir = Scratch::new("ops");
    let h = open(&dir.db());
    let ids: Vec<ConversationId> = (0..3).map(|_| ChatHistory::new_id()).collect();
    for (i, id) in ids.iter().enumerate() {
        h.append(
            id,
            "c",
            numbered(vec![user(&format!("chat {i}")), user("more")]),
        );
    }
    let list = h.list().unwrap();
    let titles: Vec<_> = list.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["chat 2", "chat 1", "chat 0"]);

    h.trim_before(&ids[0], 1);
    let loaded = h.load(&ids[0]).unwrap().unwrap();
    assert_eq!(loaded.first_seq, 1);
    assert_eq!(loaded.entries, vec![user("more")]);

    // Appending again with a known number replaces it.
    h.append(&ids[0], "c", vec![(1, user("replaced"))]);
    assert_eq!(
        h.load(&ids[0]).unwrap().unwrap().entries,
        vec![user("replaced")]
    );

    h.delete(&ids[1]);
    let list = h.list().unwrap();
    assert_eq!(list.len(), 2);
    assert!(h.load(&ids[1]).unwrap().is_none());

    h.clear();
    assert_eq!(h.list().unwrap().len(), 0);
    h.close(WAIT);
}

#[test]
fn retention_drops_the_oldest_conversations() {
    let dir = Scratch::new("retention");
    let mut o = fast();
    o.max_conversations = 2;
    let h = ChatHistory::with_options(dir.db(), o);
    let ids: Vec<ConversationId> = (0..4).map(|_| ChatHistory::new_id()).collect();
    for (i, id) in ids.iter().enumerate() {
        h.append(id, "c", vec![(0, user(&format!("chat {i}")))]);
    }
    let titles: Vec<_> = h.list().unwrap().into_iter().map(|s| s.title).collect();
    assert_eq!(titles, ["chat 3", "chat 2"]);
    h.close(WAIT);

    let dir = Scratch::new("retention-bytes");
    let mut o = fast();
    o.max_bytes = 2000;
    let h = ChatHistory::with_options(dir.db(), o);
    for i in 0..4 {
        h.append(
            &ChatHistory::new_id(),
            "c",
            vec![(0, user(&format!("{i} {}", "x".repeat(900))))],
        );
    }
    let list = h.list().unwrap();
    assert!(list.len() < 4 && !list.is_empty(), "{}", list.len());
    assert!(list[0].title.starts_with("3 "));
    h.close(WAIT);
}

#[test]
fn rows_entries_and_texts_are_capped() {
    let dir = Scratch::new("caps");
    let mut o = fast();
    o.max_text_bytes = 1000;
    o.max_entry_bytes = 4000;
    let h = ChatHistory::with_options(dir.db(), o);
    let id = ChatHistory::new_id();
    let rows: Vec<Value> = (0..80).map(|i| json!([i, "<redacted>"])).collect();
    let huge: Vec<Value> = (0..200)
        .map(|i| json!({"n": i, "pad": "y".repeat(50)}))
        .collect();
    h.append(
        &id,
        "c",
        numbered(vec![
            user(&"u".repeat(5000)),
            tool(Value::Null, json!({"columns": ["a", "ssn"], "rows": rows})),
            tool(json!({"sql": "select 1"}), json!({"items": huge})),
        ]),
    );
    let loaded = h.load(&id).unwrap().unwrap();
    let StoredEntry::User { text } = &loaded.entries[0] else {
        panic!()
    };
    assert!(
        text.len() <= 1000 && text.ends_with("[cut]"),
        "{}",
        text.len()
    );
    let StoredEntry::ToolCall { result, .. } = &loaded.entries[1] else {
        panic!()
    };
    assert_eq!(result["rows"].as_array().unwrap().len(), 50);
    assert_eq!(result["rows_kept_in_history"], 50);
    assert_eq!(result["rows"][0][1], "<redacted>");
    assert_eq!(result["rows"][49][1], "<redacted>");
    let StoredEntry::ToolCall {
        arguments, result, ..
    } = &loaded.entries[2]
    else {
        panic!()
    };
    assert_eq!(*arguments, Value::Null);
    assert_eq!(*result, json!({"history_note": "result too large to keep"}));
    h.close(WAIT);
}

#[test]
fn no_canary_reaches_the_files_and_a_cleared_title_is_gone() {
    let dir = Scratch::new("canary");
    let h = open(&dir.db());
    let id = ChatHistory::new_id();
    h.append(
        &id,
        "c",
        numbered(vec![
            user("ZEBRAQQTITLE: connect with postgres://u:CANARY@h/db and key sk-CANARYabcdefghijklmnopqrst, token=CANARY"),
            tool(
                json!({"sql": "update users set password = 'CANARY' where id = 1", "password": "CANARY"}),
                json!({"note": "the password is 'CANARY'", "api_key": "CANARY"}),
            ),
            StoredEntry::Sql {
                call_id: "c1".into(),
                connection: "c".into(),
                display_sql: "update users set password = 'CANARY'".into(),
                class: "write".into(),
                approved: true,
                rows: 0,
                truncated: None,
                failed: false,
            },
            StoredEntry::Notice {
                notice: StoredNotice::ProviderError {
                    message: "bad key sk-ant-CANARYabcdefghijklmnop".into(),
                },
            },
        ]),
    );
    assert!(h.flush(WAIT));
    let bytes = bytes_of(&dir.db());
    assert!(!contains(&bytes, "CANARY"), "a secret reached the files");
    assert!(
        contains(&bytes, "ZEBRAQQTITLE"),
        "the title should be on disk before the clear"
    );

    h.clear();
    h.close(WAIT);
    let bytes = bytes_of(&dir.db());
    assert!(!contains(&bytes, "ZEBRAQQTITLE"), "a cleared title stayed");
    assert!(!contains(&bytes, "CANARY"));
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn the_database_and_its_side_files_are_private() {
    let dir = Scratch::new("modes");
    let h = open(&dir.db());
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("hello"))]);
    assert!(h.flush(WAIT));
    assert_eq!(mode(&dir.db()), 0o600);
    for suffix in ["-wal", "-shm"] {
        let p = dir.0.join(format!("chat-history.db{suffix}"));
        assert!(p.exists(), "{suffix}");
        assert_eq!(mode(&p), 0o600, "{suffix}");
    }
    h.close(WAIT);
}

#[cfg(unix)]
#[test]
fn a_world_readable_database_is_repaired() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("repair");
    let h = open(&dir.db());
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("hello"))]);
    h.close(WAIT);
    fs::set_permissions(dir.db(), fs::Permissions::from_mode(0o644)).unwrap();
    let h = open(&dir.db());
    ready(&h);
    assert_eq!(mode(&dir.db()), 0o600);
    assert_eq!(h.list().unwrap().len(), 1);
    h.close(WAIT);
}

fn sha(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

#[test]
fn a_garbage_file_is_moved_aside() {
    let dir = Scratch::new("garbage");
    fs::write(dir.db(), vec![b'x'; 5000]).unwrap();
    let h = open(&dir.db());
    ready(&h);
    assert!(h.take_notice().unwrap().contains("moved aside"));
    assert!(h.take_notice().is_none());
    assert_eq!(h.list().unwrap().len(), 0);
    let moved: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("chat-history.db.corrupt-"))
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(fs::read(dir.0.join(&moved[0])).unwrap(), vec![b'x'; 5000]);
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("fresh"))]);
    assert_eq!(h.list().unwrap().len(), 1);
    h.close(WAIT);
}

#[test]
fn a_short_file_counts_as_corrupt() {
    let dir = Scratch::new("short");
    fs::write(dir.db(), b"SQLite format 3\0").unwrap();
    let h = open(&dir.db());
    ready(&h);
    assert!(h.take_notice().is_some());
    h.close(WAIT);
}

#[test]
fn a_newer_database_turns_the_store_off_and_stays_untouched() {
    let dir = Scratch::new("newer");
    let h = open(&dir.db());
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("hello"))]);
    h.close(WAIT);
    // Raise `user_version` (offset 60) in the header, as a newer SQLer would have.
    let mut bytes = fs::read(dir.db()).unwrap();
    bytes[60..64].copy_from_slice(&99u32.to_be_bytes());
    fs::write(dir.db(), &bytes).unwrap();
    let before = sha(&dir.db());

    let h = open(&dir.db());
    let err = h.list().unwrap_err();
    let ChatHistoryError::Off(text) = err else {
        panic!("{err:?}")
    };
    assert!(text.contains("newer SQLer"), "{text}");
    assert!(matches!(h.status(), ChatHistoryStatus::Off(_)));
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("ignored"))]);
    assert!(!h.flush(WAIT));
    assert!(h.take_notice().is_none());
    h.close(WAIT);
    assert_eq!(sha(&dir.db()), before);
    let side: Vec<_> = fs::read_dir(&dir.0).unwrap().collect();
    assert_eq!(side.len(), 1, "no side files for an untouched file");
}

#[test]
fn a_second_store_sees_the_data_within_the_flush_interval() {
    let dir = Scratch::new("batch");
    let mut o = ChatHistoryOptions::default();
    o.flush_interval = Duration::from_millis(200);
    let writer = ChatHistory::with_options(dir.db(), o);
    ready(&writer);
    let reader = open(&dir.db());
    ready(&reader);
    writer.append(&ChatHistory::new_id(), "c", vec![(0, user("hello"))]);
    // No flush: the batch timer commits it.
    let start = Instant::now();
    loop {
        if reader.list().unwrap().len() == 1 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "never committed");
        std::thread::sleep(Duration::from_millis(50));
    }
    writer.close(WAIT);
    reader.close(WAIT);
}

#[test]
fn a_dropped_handle_commits_what_is_queued() {
    let dir = Scratch::new("drop");
    {
        let mut o = ChatHistoryOptions::default();
        o.flush_interval = Duration::from_secs(60);
        let h = ChatHistory::with_options(dir.db(), o);
        h.append(&ChatHistory::new_id(), "c", vec![(0, user("kept"))]);
    }
    // The thread commits on disconnect; give it a moment.
    let start = Instant::now();
    loop {
        let h = open(&dir.db());
        let n = h.list().map_or(0, |l| l.len());
        h.close(WAIT);
        if n == 1 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_debug_text_shows_the_path_only() {
    let dir = Scratch::new("debug");
    let h = open(&dir.db());
    h.append(&ChatHistory::new_id(), "c", vec![(0, user("CANARY text"))]);
    let text = format!("{h:?}");
    assert!(text.contains("chat-history.db") && !text.contains("CANARY"));
    h.close(WAIT);
}

#[test]
fn the_first_failed_write_raises_one_notice() {
    let dir = Scratch::new("write-fails");
    let h = open(&dir.db());
    ready(&h);
    assert!(h.take_notice().is_none());
    // Another connection takes the table away, so the next batch cannot be written.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        use sqlx::{Connection, Executor, sqlite::SqliteConnection};
        let mut c = SqliteConnection::connect(&format!("sqlite://{}", dir.db().display()))
            .await
            .unwrap();
        c.execute("DROP TABLE entry").await.unwrap();
        c.close().await.unwrap();
    });
    let id = ChatHistory::new_id();
    h.append(&id, "c", numbered(vec![user("lost")]));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut notice = None;
    while notice.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        notice = h.take_notice();
    }
    assert!(notice.unwrap().contains("could not save"));
    h.append(&id, "c", numbered(vec![user("lost again")]));
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.take_notice().is_none());
    h.close(WAIT);
}
