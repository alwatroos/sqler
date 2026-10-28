// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `session.json`: the open editor tabs, the active tab and the unsent AI chat draft.
//!
//! The file holds SQL text, so it is private to the user (mode `0600` on Unix) and it is kept
//! apart from `workbench.toml`: it can be large and a corrupt file must not hide the layout.
//! Only connection ids and driver ids are stored, never names, URLs or secrets.
//!
//! The file is read leniently: a field of the wrong type falls back to its default and an
//! unusable tab entry is skipped. Only a JSON syntax error, a non-object root, a bad `version`
//! or an oversized file makes it unusable. A message never echoes file content.

use serde::Serialize;
use serde_json::Value;

use crate::fs::{read_optional_bytes, write_atomic_private};
use crate::{ConfigDir, ConfigError};

/// The format version this build reads and writes.
pub const SESSION_VERSION: u32 = 1;

/// The largest accepted `session.json`, in bytes.
pub const MAX_SESSION_BYTES: u64 = 4 * 1024 * 1024;

/// How many tabs are kept.
pub const MAX_SESSION_TABS: usize = 50;

/// The largest text of one tab, in bytes.
pub const MAX_SESSION_TAB_BYTES: usize = 1024 * 1024;

/// The largest chat draft, in bytes.
pub const MAX_CHAT_DRAFT_BYTES: usize = 64 * 1024;

/// One open editor tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionTab {
    /// The id of the saved connection the tab was bound to, if any.
    pub connection: Option<String>,
    /// The driver id of the tab (kept when the connection no longer exists).
    pub driver: String,
    /// The exact editor text.
    pub text: String,
    /// Whether the text has unsaved changes.
    pub dirty: bool,
}

/// The content of `session.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    /// Format version; always [`SESSION_VERSION`] when written.
    pub version: u32,
    /// The editor tabs in strip order.
    pub tabs: Vec<SessionTab>,
    /// Index into `tabs` of the active tab.
    pub active: Option<usize>,
    /// The unsent AI chat draft.
    pub chat_draft: String,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            tabs: Vec::new(),
            active: None,
            chat_draft: String::new(),
        }
    }
}

/// The outcome of reading `session.json`.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionLoad {
    /// There is no file.
    Missing,
    /// The file was read (fields may have fallen back to defaults).
    Loaded(SessionState),
    /// The file was written by a newer `SQLer`; it must not be overwritten.
    Newer(u32),
    /// The file is unusable; the message says why and never echoes file content.
    Invalid(String),
}

#[derive(Serialize)]
struct WireTab<'a> {
    kind: &'static str,
    connection: Option<&'a str>,
    driver: &'a str,
    text: &'a str,
    dirty: bool,
}

#[derive(Serialize)]
struct WireChat<'a> {
    draft: &'a str,
}

#[derive(Serialize)]
struct Wire<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<usize>,
    tabs: Vec<WireTab<'a>>,
    chat: WireChat<'a>,
}

fn serialize(state: &SessionState) -> Result<String, serde_json::Error> {
    serde_json::to_string(&Wire {
        version: state.version,
        active: state.active,
        tabs: state
            .tabs
            .iter()
            .map(|t| WireTab {
                kind: "query",
                connection: t.connection.as_deref(),
                driver: &t.driver,
                text: &t.text,
                dirty: t.dirty,
            })
            .collect(),
        chat: WireChat {
            draft: &state.chat_draft,
        },
    })
}

fn tab_of(v: &Value) -> Option<SessionTab> {
    let o = v.as_object()?;
    if o.get("kind").is_some_and(|k| k.as_str() != Some("query")) {
        return None;
    }
    let text = o.get("text")?.as_str()?;
    if text.len() > MAX_SESSION_TAB_BYTES {
        return None;
    }
    Some(SessionTab {
        connection: o
            .get("connection")
            .and_then(Value::as_str)
            .map(str::to_owned),
        driver: o
            .get("driver")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        text: text.to_owned(),
        dirty: o.get("dirty").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn parse(bytes: &[u8]) -> SessionLoad {
    let root: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            return SessionLoad::Invalid(format!(
                "{} error at line {}, column {}",
                match e.classify() {
                    serde_json::error::Category::Io => "I/O",
                    serde_json::error::Category::Syntax => "syntax",
                    serde_json::error::Category::Data => "data",
                    serde_json::error::Category::Eof => "unexpected end of file",
                },
                e.line(),
                e.column()
            ));
        }
    };
    let Some(obj) = root.as_object() else {
        return SessionLoad::Invalid("the root must be an object".to_owned());
    };
    match obj.get("version") {
        None => {}
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => {
                if n > u64::from(SESSION_VERSION) {
                    return SessionLoad::Newer(u32::try_from(n).unwrap_or(u32::MAX));
                }
            }
            _ => {
                return SessionLoad::Invalid("version must be an integer of at least 1".to_owned());
            }
        },
    }
    let mut tabs = Vec::new();
    let mut file_index = Vec::new();
    if let Some(a) = obj.get("tabs").and_then(Value::as_array) {
        for (i, v) in a.iter().enumerate() {
            if tabs.len() >= MAX_SESSION_TABS {
                break;
            }
            if let Some(t) = tab_of(v) {
                tabs.push(t);
                file_index.push(i);
            }
        }
    }
    let active = obj
        .get("active")
        .and_then(Value::as_u64)
        .and_then(|a| file_index.iter().position(|i| u64::try_from(*i) == Ok(a)));
    let chat_draft = obj
        .get("chat")
        .and_then(|c| c.get("draft"))
        .and_then(Value::as_str)
        .filter(|d| d.len() <= MAX_CHAT_DRAFT_BYTES)
        .unwrap_or_default()
        .to_owned();
    SessionLoad::Loaded(SessionState {
        version: SESSION_VERSION,
        tabs,
        active,
        chat_draft,
    })
}

fn parse_file(bytes: &[u8]) -> SessionLoad {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SESSION_BYTES {
        return SessionLoad::Invalid(format!("the file is larger than {MAX_SESSION_BYTES} bytes"));
    }
    parse(bytes)
}

/// Fits `state` into the caps and returns it with the number of tabs dropped.
///
/// Tabs are dropped in this order: text over [`MAX_SESSION_TAB_BYTES`], tabs past
/// [`MAX_SESSION_TABS`], then tabs from the end until the serialised size is at most
/// [`MAX_SESSION_BYTES`]. `active` is remapped, or `None` when its tab was dropped. A draft
/// over [`MAX_CHAT_DRAFT_BYTES`] becomes empty.
#[must_use]
pub fn fit_session(state: SessionState) -> (SessionState, usize) {
    let SessionState {
        version,
        tabs,
        active,
        chat_draft,
    } = state;
    let total = tabs.len();
    let mut kept: Vec<(usize, SessionTab)> = tabs
        .into_iter()
        .enumerate()
        .filter(|(_, t)| t.text.len() <= MAX_SESSION_TAB_BYTES)
        .collect();
    kept.truncate(MAX_SESSION_TABS);
    let chat_draft = if chat_draft.len() > MAX_CHAT_DRAFT_BYTES {
        String::new()
    } else {
        chat_draft
    };
    let build = |kept: &[(usize, SessionTab)]| SessionState {
        version,
        tabs: kept.iter().map(|(_, t)| t.clone()).collect(),
        active: active.and_then(|a| kept.iter().position(|(i, _)| *i == a)),
        chat_draft: chat_draft.clone(),
    };
    loop {
        let candidate = build(&kept);
        let fits = serialize(&candidate)
            .is_ok_and(|s| u64::try_from(s.len()).unwrap_or(u64::MAX) <= MAX_SESSION_BYTES);
        if fits || kept.is_empty() {
            let dropped = total - candidate.tabs.len();
            return (candidate, dropped);
        }
        kept.pop();
    }
}

impl ConfigDir {
    /// Path of `session.json`.
    #[must_use]
    pub fn session_path(&self) -> std::path::PathBuf {
        self.root().join("session.json")
    }

    /// Path of the backup `session.json.bak`.
    #[must_use]
    pub fn session_backup_path(&self) -> std::path::PathBuf {
        self.root().join("session.json.bak")
    }

    /// Loads `session.json` leniently; never fails and never panics.
    ///
    /// An I/O error, a file over [`MAX_SESSION_BYTES`], a JSON syntax error, a non-object
    /// root and a bad `version` give [`SessionLoad::Invalid`]. A `version` above
    /// [`SESSION_VERSION`] gives [`SessionLoad::Newer`].
    #[must_use]
    pub fn load_session_state(&self) -> SessionLoad {
        let path = self.session_path();
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_SESSION_BYTES => {
                return SessionLoad::Invalid(format!(
                    "the file is larger than {MAX_SESSION_BYTES} bytes"
                ));
            }
            _ => {}
        }
        match read_optional_bytes(&path) {
            Ok(None) => SessionLoad::Missing,
            Ok(Some(bytes)) => parse_file(&bytes),
            Err(e) => SessionLoad::Invalid(e.to_string()),
        }
    }

    /// Writes `session.json` (mode `0600` on Unix) and returns how many tabs did not fit.
    ///
    /// A newer file on disk is never overwritten. A file that does not load is copied
    /// byte for byte to `session.json.bak` (also `0600`) before it is replaced; when the copy
    /// fails nothing is written.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] when `state.version` is not [`SESSION_VERSION`] or the file on
    /// disk is newer, [`ConfigError::Serialize`], [`ConfigError::Read`] or
    /// [`ConfigError::Write`] (also when the backup cannot be made).
    pub fn save_session_state(&self, state: &SessionState) -> Result<usize, ConfigError> {
        let path = self.session_path();
        if state.version != SESSION_VERSION {
            return Err(ConfigError::Invalid {
                path,
                message: format!(
                    "cannot write version {} (this build writes version {SESSION_VERSION})",
                    state.version
                ),
            });
        }
        match self.load_session_state() {
            SessionLoad::Newer(n) => {
                return Err(ConfigError::Invalid {
                    path,
                    message: format!("the file is version {n}, written by a newer SQLer"),
                });
            }
            SessionLoad::Invalid(_) => {
                if let Some(raw) = read_optional_bytes(&path)? {
                    write_atomic_private(&self.session_backup_path(), raw)?;
                }
            }
            SessionLoad::Missing | SessionLoad::Loaded(_) => {}
        }
        let (fitted, dropped) = fit_session(state.clone());
        let text = serialize(&fitted).map_err(|e| ConfigError::Serialize {
            path: path.clone(),
            message: e.to_string(),
        })?;
        write_atomic_private(&path, text)?;
        Ok(dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> (Scratch, ConfigDir) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("sqler-config-{}-ss-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        let d = ConfigDir::new(&p);
        (Scratch(p), d)
    }

    fn loaded(text: &str) -> SessionState {
        match parse(text.as_bytes()) {
            SessionLoad::Loaded(s) => s,
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    fn tab(text: &str) -> SessionTab {
        SessionTab {
            connection: Some("c1".into()),
            driver: "postgres".into(),
            text: text.into(),
            dirty: true,
        }
    }

    #[test]
    fn missing_file() {
        let (_t, d) = dir();
        assert_eq!(d.load_session_state(), SessionLoad::Missing);
        assert_eq!(d.session_path(), d.root().join("session.json"));
        assert_eq!(d.session_backup_path(), d.root().join("session.json.bak"));
    }

    #[test]
    fn round_trip_tricky_text() {
        let (_t, d) = dir();
        let big = "ab\r\n".repeat(MAX_SESSION_TAB_BYTES / 4);
        assert_eq!(big.len(), MAX_SESSION_TAB_BYTES);
        let texts = [
            "a\r\nb\rc\td\0e",
            "emoji \u{1F600} \u{e9} \"\"\" \\ \\n 'q'",
            "",
            big.as_str(),
        ];
        let s = SessionState {
            version: SESSION_VERSION,
            tabs: texts.iter().map(|t| tab(t)).collect(),
            active: Some(2),
            chat_draft: "draft\r\nline".into(),
        };
        assert_eq!(d.save_session_state(&s).unwrap(), 0);
        assert_eq!(d.load_session_state(), SessionLoad::Loaded(s));
    }

    #[test]
    fn syntax_error_does_not_echo_content() {
        let r = parse(b"{\"text\": SECRETMARKER}");
        let SessionLoad::Invalid(m) = r else {
            panic!("expected Invalid")
        };
        assert!(!m.contains("SECRETMARKER"), "{m}");
        assert!(m.contains("line 1"), "{m}");
    }

    #[test]
    fn non_object_root_is_invalid() {
        assert!(matches!(parse(b"[1]"), SessionLoad::Invalid(_)));
        assert!(matches!(parse(b""), SessionLoad::Invalid(_)));
        assert!(matches!(parse(&[0xff, 0xfe]), SessionLoad::Invalid(_)));
    }

    #[test]
    fn version_rules() {
        assert!(matches!(parse(b"{}"), SessionLoad::Loaded(_)));
        assert!(matches!(parse(b"{\"version\":1}"), SessionLoad::Loaded(_)));
        assert!(matches!(parse(b"{\"version\":0}"), SessionLoad::Invalid(_)));
        assert!(matches!(
            parse(b"{\"version\":-1}"),
            SessionLoad::Invalid(_)
        ));
        assert!(matches!(
            parse(b"{\"version\":\"1\"}"),
            SessionLoad::Invalid(_)
        ));
        assert!(matches!(
            parse(b"{\"version\":1.5}"),
            SessionLoad::Invalid(_)
        ));
        assert_eq!(parse(b"{\"version\":2}"), SessionLoad::Newer(2));
        assert_eq!(
            parse(b"{\"version\":99999999999}"),
            SessionLoad::Newer(u32::MAX)
        );
    }

    #[test]
    fn unknown_keys_and_kinds_are_ignored() {
        let s = loaded(
            r#"{"extra":1,"active":1,"tabs":[
                {"kind":"grid","text":"x"},
                {"kind":"query","text":"keep","future":true},
                {"text":"no kind"}
            ]}"#,
        );
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].text, "keep");
        assert_eq!(s.tabs[1].text, "no kind");
        assert_eq!(s.active, Some(0));
    }

    #[test]
    fn wrong_types_fall_back() {
        let s = loaded(
            r#"{"tabs":[
                {"text":"a","connection":5,"driver":7,"dirty":"yes"},
                {"text":3},
                "str",
                {"text":"b","connection":null}
            ],"chat":{"draft":4}}"#,
        );
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].connection, None);
        assert_eq!(s.tabs[0].driver, "");
        assert!(!s.tabs[0].dirty);
        assert_eq!(s.chat_draft, "");
        assert_eq!(loaded(r#"{"tabs":"nope"}"#).tabs.len(), 0);
    }

    #[test]
    fn caps_on_read() {
        let huge = "x".repeat(MAX_SESSION_TAB_BYTES + 1);
        let doc = format!(
            r#"{{"active":0,"tabs":[{{"text":"{huge}"}},{{"text":"ok"}}],"chat":{{"draft":"{}"}}}}"#,
            "d".repeat(MAX_CHAT_DRAFT_BYTES + 1)
        );
        let s = loaded(&doc);
        assert_eq!(s.tabs.len(), 1);
        assert_eq!(s.active, None);
        assert_eq!(s.chat_draft, "");

        let many: Vec<String> = (0..MAX_SESSION_TABS + 5)
            .map(|i| format!(r#"{{"text":"{i}"}}"#))
            .collect();
        let s = loaded(&format!(r#"{{"active":52,"tabs":[{}]}}"#, many.join(",")));
        assert_eq!(s.tabs.len(), MAX_SESSION_TABS);
        assert_eq!(s.active, None);
    }

    #[test]
    fn active_remaps_past_skipped_entry() {
        let s = loaded(r#"{"active":2,"tabs":[{"text":"a"},{"text":1},{"text":"c"}]}"#);
        assert_eq!(s.active, Some(1));
        assert_eq!(loaded(r#"{"active":9,"tabs":[{"text":"a"}]}"#).active, None);
        assert_eq!(
            loaded(r#"{"active":-1,"tabs":[{"text":"a"}]}"#).active,
            None
        );
    }

    #[test]
    fn oversized_file_is_invalid() {
        let (_t, d) = dir();
        let pad = " ".repeat(usize::try_from(MAX_SESSION_BYTES).unwrap() + 1);
        std::fs::write(d.session_path(), pad).unwrap();
        assert!(matches!(d.load_session_state(), SessionLoad::Invalid(_)));
    }

    #[test]
    fn fit_drop_order_and_active_remap() {
        let big = "x".repeat(MAX_SESSION_TAB_BYTES + 1);
        let s = SessionState {
            version: 1,
            tabs: vec![tab("a"), tab(&big), tab("c")],
            active: Some(2),
            chat_draft: "d".repeat(MAX_CHAT_DRAFT_BYTES + 1),
        };
        let (f, dropped) = fit_session(s);
        assert_eq!(dropped, 1);
        assert_eq!(f.tabs.len(), 2);
        assert_eq!(f.active, Some(1));
        assert_eq!(f.chat_draft, "");

        let s = SessionState {
            version: 1,
            tabs: vec![tab("a"), tab(&big)],
            active: Some(1),
            chat_draft: String::new(),
        };
        assert_eq!(fit_session(s).0.active, None);

        let s = SessionState {
            version: 1,
            tabs: (0..MAX_SESSION_TABS + 3).map(|_| tab("q")).collect(),
            active: Some(MAX_SESSION_TABS + 1),
            chat_draft: String::new(),
        };
        let (f, dropped) = fit_session(s);
        assert_eq!(
            (f.tabs.len(), dropped, f.active),
            (MAX_SESSION_TABS, 3, None)
        );
    }

    #[test]
    fn fit_drops_from_the_end_to_fit_total_size() {
        // Control characters serialise to six bytes each, so only one of these fits the file cap.
        let heavy = "\u{1}".repeat(600_000);
        let s = SessionState {
            version: 1,
            tabs: (0..6).map(|_| tab(&heavy)).collect(),
            active: Some(5),
            chat_draft: String::new(),
        };
        let (f, dropped) = fit_session(s);
        assert_eq!(f.tabs.len(), 1);
        assert_eq!(dropped, 6 - f.tabs.len());
        assert_eq!(f.active, None);
        let len = serialize(&f).unwrap().len();
        assert!(u64::try_from(len).unwrap() <= MAX_SESSION_BYTES);
    }

    #[test]
    fn save_rejects_other_version() {
        let (_t, d) = dir();
        let s = SessionState {
            version: 2,
            ..SessionState::default()
        };
        assert!(matches!(
            d.save_session_state(&s),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(!d.session_path().exists());
    }

    #[test]
    fn newer_file_is_never_overwritten() {
        let (_t, d) = dir();
        std::fs::write(d.session_path(), r#"{"version":2,"x":1}"#).unwrap();
        let r = d.save_session_state(&SessionState::default());
        assert!(matches!(r, Err(ConfigError::Invalid { .. })));
        assert_eq!(
            std::fs::read_to_string(d.session_path()).unwrap(),
            r#"{"version":2,"x":1}"#
        );
        assert!(!d.session_backup_path().exists());
    }

    #[test]
    fn invalid_file_is_backed_up_then_replaced() {
        let (_t, d) = dir();
        std::fs::write(d.session_path(), "{not json").unwrap();
        d.save_session_state(&SessionState::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.session_backup_path()).unwrap(),
            "{not json"
        );
        assert!(matches!(d.load_session_state(), SessionLoad::Loaded(_)));
    }

    #[test]
    fn failed_backup_writes_nothing() {
        let (_t, d) = dir();
        std::fs::write(d.session_path(), "{not json").unwrap();
        // A directory in the backup's place makes the rename fail.
        std::fs::create_dir(d.session_backup_path()).unwrap();
        let r = d.save_session_state(&SessionState::default());
        assert!(r.is_err());
        assert_eq!(
            std::fs::read_to_string(d.session_path()).unwrap(),
            "{not json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, d) = dir();
        std::fs::write(d.session_path(), "{not json").unwrap();
        d.save_session_state(&SessionState::default()).unwrap();
        for p in [d.session_path(), d.session_backup_path()] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
