// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Black-box checks of `session.json` through the public API (F1).

#![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]

use sqler_config::{
    ConfigDir, MAX_SESSION_BYTES, MAX_SESSION_TABS, SESSION_VERSION, SessionLoad, SessionState,
    SessionTab,
};

fn root(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("sqler-f1-probe-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn tab(conn: Option<&str>, text: &str, dirty: bool) -> SessionTab {
    SessionTab {
        connection: conn.map(str::to_owned),
        driver: "postgres".into(),
        text: text.into(),
        dirty,
    }
}

#[test]
fn round_trip_order_active_crlf_unicode_and_mode() {
    let r = root("rt");
    let dir = ConfigDir::new(&r);
    let st = SessionState {
        version: SESSION_VERSION,
        tabs: vec![
            tab(Some("c1"), "select 1;\r\nselect 'zażółć 🦀';\r\n", true),
            tab(None, "\r", false),
            tab(Some("gone"), "-- \u{0}nul\t\"\"\" \\ end\n", true),
        ],
        active: Some(2),
        chat_draft: "draft\r\nline 🦀".into(),
    };
    assert_eq!(dir.save_session_state(&st).unwrap(), 0);
    assert_eq!(dir.load_session_state(), SessionLoad::Loaded(st.clone()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.session_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(m, 0o600);
    }
    let raw = std::fs::read_to_string(dir.session_path()).unwrap();
    for k in ["password", "token", "api_key", "secret"] {
        assert!(!raw.contains(k), "{k} leaked");
    }
    std::fs::remove_dir_all(&r).unwrap();
}

#[test]
fn corrupt_is_invalid_then_backed_up_and_replaced() {
    let r = root("bad");
    let dir = ConfigDir::new(&r);
    std::fs::write(dir.session_path(), b"{ not json").unwrap();
    assert!(matches!(dir.load_session_state(), SessionLoad::Invalid(_)));
    let st = SessionState {
        tabs: vec![tab(None, "x", false)],
        active: Some(0),
        ..SessionState::default()
    };
    dir.save_session_state(&st).unwrap();
    assert_eq!(
        std::fs::read(dir.session_backup_path()).unwrap(),
        b"{ not json"
    );
    assert_eq!(dir.load_session_state(), SessionLoad::Loaded(st));
    std::fs::remove_dir_all(&r).unwrap();
}

#[test]
fn newer_version_is_reported_and_never_overwritten() {
    let r = root("new");
    let dir = ConfigDir::new(&r);
    let body = format!(r#"{{"version":{},"tabs":[]}}"#, SESSION_VERSION + 1);
    std::fs::write(dir.session_path(), &body).unwrap();
    assert_eq!(
        dir.load_session_state(),
        SessionLoad::Newer(SESSION_VERSION + 1)
    );
    let _ = dir.save_session_state(&SessionState {
        tabs: vec![tab(None, "x", false)],
        ..SessionState::default()
    });
    assert_eq!(std::fs::read_to_string(dir.session_path()).unwrap(), body);
    std::fs::remove_dir_all(&r).unwrap();
}

#[test]
fn size_caps() {
    let r = root("cap");
    let dir = ConfigDir::new(&r);
    std::fs::write(
        dir.session_path(),
        vec![b' '; MAX_SESSION_BYTES as usize + 10],
    )
    .unwrap();
    assert!(matches!(dir.load_session_state(), SessionLoad::Invalid(_)));
    let tabs = (0..MAX_SESSION_TABS + 5)
        .map(|i| tab(None, &format!("q{i}"), true))
        .collect();
    let dropped = dir
        .save_session_state(&SessionState {
            tabs,
            active: Some(0),
            ..SessionState::default()
        })
        .unwrap();
    assert_eq!(dropped, 5);
    match dir.load_session_state() {
        SessionLoad::Loaded(s) => assert_eq!(s.tabs.len(), MAX_SESSION_TABS),
        o => panic!("{o:?}"),
    }
    std::fs::remove_dir_all(&r).unwrap();
}

#[test]
fn missing_file() {
    let r = root("miss");
    assert_eq!(
        ConfigDir::new(&r).load_session_state(),
        SessionLoad::Missing
    );
    std::fs::remove_dir_all(&r).unwrap();
}
