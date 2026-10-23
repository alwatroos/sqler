// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `update-state.toml`: when the last update check ran, how many checks in a row failed and
//! which version the user dismissed (ADR-0066).
//!
//! The file holds only those three values: no URL, no path, no identifier. It is read
//! leniently: a field of the wrong type or a version string that is not a plain version
//! falls back to its default and never discards the rest of the file.

use serde::Serialize;
use toml::{Table, Value};

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

/// The longest `dismissed_version` that is kept.
pub const MAX_DISMISSED_VERSION_LEN: usize = 64;

/// The content of `update-state.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct UpdateState {
    /// Unix time in seconds of the last check attempt, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<u64>,
    /// Checks that failed in a row since the last success.
    pub consecutive_failures: u32,
    /// The version whose automatic notice the user dismissed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed_version: Option<String>,
}

/// Whether `v` is a plain version string: at most [`MAX_DISMISSED_VERSION_LEN`] characters of
/// `[0-9A-Za-z.+-]`.
fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= MAX_DISMISSED_VERSION_LEN
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

fn parse(text: &str) -> UpdateState {
    let Ok(table) = text.parse::<Table>() else {
        return UpdateState::default();
    };
    UpdateState {
        last_attempt: table
            .get("last_attempt")
            .and_then(Value::as_integer)
            .and_then(|n| u64::try_from(n).ok()),
        consecutive_failures: table
            .get("consecutive_failures")
            .and_then(Value::as_integer)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        dismissed_version: table
            .get("dismissed_version")
            .and_then(Value::as_str)
            .filter(|v| valid_version(v))
            .map(str::to_owned),
    }
}

impl ConfigDir {
    /// Path of `update-state.toml`.
    #[must_use]
    pub fn update_state_path(&self) -> std::path::PathBuf {
        self.root().join("update-state.toml")
    }

    /// Loads `update-state.toml` leniently; never fails. A missing or unreadable file, a
    /// syntax error and every bad field give the default.
    #[must_use]
    pub fn load_update_state(&self) -> UpdateState {
        match read_optional(&self.update_state_path()) {
            Ok(Some(text)) => parse(&text),
            _ => UpdateState::default(),
        }
    }

    /// Writes `update-state.toml`, creating the directory when missing. A
    /// `dismissed_version` that is not a plain version string is dropped.
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] or [`ConfigError::Write`].
    pub fn save_update_state(&self, state: &UpdateState) -> Result<(), ConfigError> {
        let path = self.update_state_path();
        let mut clean = state.clone();
        if clean
            .dismissed_version
            .as_deref()
            .is_some_and(|v| !valid_version(v))
        {
            clean.dismissed_version = None;
        }
        let text = toml::to_string_pretty(&clean).map_err(|e| ConfigError::Serialize {
            path: path.clone(),
            message: e.to_string(),
        })?;
        write_atomic(&path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory removed on drop.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> (Scratch, ConfigDir) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("sqler-config-{}-us-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        let d = ConfigDir::new(&p);
        (Scratch(p), d)
    }

    fn state(last: Option<u64>, failures: u32, dismissed: Option<&str>) -> UpdateState {
        UpdateState {
            last_attempt: last,
            consecutive_failures: failures,
            dismissed_version: dismissed.map(str::to_owned),
        }
    }

    #[test]
    fn missing_file_is_default() {
        let (_g, d) = dir();
        assert_eq!(d.load_update_state(), UpdateState::default());
        assert_eq!(d.update_state_path(), d.root().join("update-state.toml"));
    }

    #[test]
    fn garbage_is_default() {
        let (_g, d) = dir();
        std::fs::write(d.update_state_path(), "][ not toml \u{0}").unwrap();
        assert_eq!(d.load_update_state(), UpdateState::default());
        std::fs::write(d.update_state_path(), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(d.load_update_state(), UpdateState::default());
    }

    #[test]
    fn bad_fields_fall_back_one_by_one() {
        let s = parse("last_attempt = \"soon\"\nconsecutive_failures = 3\n");
        assert_eq!(s, state(None, 3, None));
        let s = parse("last_attempt = -4\nconsecutive_failures = -1\n");
        assert_eq!(s, UpdateState::default());
        let s = parse("last_attempt = 77\nconsecutive_failures = 99999999999\n");
        assert_eq!(s, state(Some(77), 0, None));
    }

    #[test]
    fn bad_version_string_is_dropped() {
        for bad in [
            "",
            "1.0/0",
            "a\\b",
            "1.0 0",
            "v1.0.0\n",
            "https://x",
            &"1".repeat(MAX_DISMISSED_VERSION_LEN + 1),
        ] {
            let text = format!("dismissed_version = {}\n", toml::Value::String(bad.into()));
            assert_eq!(parse(&text).dismissed_version, None, "{bad:?}");
        }
        assert_eq!(parse("dismissed_version = 5").dismissed_version, None);
        let ok = "1".repeat(MAX_DISMISSED_VERSION_LEN);
        assert_eq!(
            parse(&format!("dismissed_version = \"{ok}\"")).dismissed_version,
            Some(ok)
        );
        assert_eq!(
            parse("dismissed_version = \"1.2.3-rc.1+b5\"").dismissed_version,
            Some("1.2.3-rc.1+b5".to_owned())
        );
    }

    #[test]
    fn round_trip() {
        let (_g, d) = dir();
        let s = state(Some(1_700_000_000), 2, Some("1.1.0"));
        d.save_update_state(&s).unwrap();
        assert_eq!(d.load_update_state(), s);
        d.save_update_state(&UpdateState::default()).unwrap();
        assert_eq!(d.load_update_state(), UpdateState::default());
    }

    #[test]
    fn saved_text_has_only_the_three_keys() {
        let (_g, d) = dir();
        d.save_update_state(&state(Some(5), 1, Some("2.0.0")))
            .unwrap();
        let text = std::fs::read_to_string(d.update_state_path()).unwrap();
        let table: Table = text.parse().unwrap();
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["consecutive_failures", "dismissed_version", "last_attempt"]
        );
        assert!(!text.contains('/') && !text.contains('\\'), "{text}");
    }

    #[test]
    fn invalid_version_is_dropped_on_save() {
        let (_g, d) = dir();
        d.save_update_state(&state(None, 0, Some("a/b"))).unwrap();
        let text = std::fs::read_to_string(d.update_state_path()).unwrap();
        assert!(!text.contains("dismissed_version"), "{text}");
        assert!(!text.contains('/'), "{text}");
    }
}
