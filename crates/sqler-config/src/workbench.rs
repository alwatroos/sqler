// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `workbench.toml`: panel sizes and visibility, the last window size and the recently used
//! commands and connections.
//!
//! The file is read leniently: a field of the wrong type, an unknown value or a size out of
//! range falls back to its default (or is clamped) and never discards the rest of the file.
//! Only a TOML syntax error, a non-table document or a bad `version` makes the file unusable.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

/// The format version this build reads and writes.
pub const WORKBENCH_VERSION: u32 = 1;

/// Accepted side panel widths in logical pixels.
pub const SIDE_PANEL_SIZE_RANGE: RangeInclusive<f32> = 180.0..=600.0;

/// Accepted bottom panel heights in logical pixels.
pub const BOTTOM_PANEL_SIZE_RANGE: RangeInclusive<f32> = 120.0..=4096.0;

/// Accepted remembered window width and height in logical pixels.
pub const WINDOW_SIZE_RANGE: RangeInclusive<f32> = 400.0..=16384.0;

/// How many recently used commands are kept.
pub const MAX_RECENT_COMMANDS: usize = 10;

/// How many recently opened connections are kept.
pub const MAX_RECENT_CONNECTIONS: usize = 5;

/// The known views of the side panel.
pub const SIDE_VIEWS: [&str; 3] = ["connections", "saved-queries", "ai"];

/// The known views of the bottom panel.
pub const BOTTOM_VIEWS: [&str; 4] = ["results", "messages", "history", "ai-audit"];

const DEFAULT_SIDE_SIZE: f32 = 260.0;
const DEFAULT_BOTTOM_SIZE: f32 = 380.0;

/// One collapsible panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelState {
    /// Whether the panel is shown.
    pub visible: bool,
    /// Width (side panel) or height (bottom panel) in logical pixels.
    pub size: f32,
    /// The view shown: one of [`SIDE_VIEWS`] or [`BOTTOM_VIEWS`].
    pub view: String,
}

impl PanelState {
    fn side() -> Self {
        Self {
            visible: true,
            size: DEFAULT_SIDE_SIZE,
            view: SIDE_VIEWS[0].to_owned(),
        }
    }

    fn bottom() -> Self {
        Self {
            visible: false,
            size: DEFAULT_BOTTOM_SIZE,
            view: BOTTOM_VIEWS[0].to_owned(),
        }
    }
}

impl Default for PanelState {
    /// The side panel default; the bottom panel default is in [`WorkbenchState::default`].
    fn default() -> Self {
        Self::side()
    }
}

/// The remembered window size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowState {
    /// Width in logical pixels.
    pub width: f32,
    /// Height in logical pixels.
    pub height: f32,
}

/// The content of `workbench.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkbenchState {
    /// Format version; always [`WORKBENCH_VERSION`] when written.
    pub version: u32,
    /// The side panel.
    pub side_panel: PanelState,
    /// The bottom panel.
    pub bottom_panel: PanelState,
    /// The window size at the last exit, if it was sane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
    /// Command IDs, most recent first.
    pub recent_commands: Vec<String>,
    /// Connection IDs, most recent first.
    pub recent_connections: Vec<String>,
}

impl Default for WorkbenchState {
    fn default() -> Self {
        Self {
            version: WORKBENCH_VERSION,
            side_panel: PanelState::side(),
            bottom_panel: PanelState::bottom(),
            window: None,
            recent_commands: Vec::new(),
            recent_connections: Vec::new(),
        }
    }
}

/// The outcome of reading `workbench.toml`.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkbenchLoad {
    /// There is no file.
    Missing,
    /// The file was read (fields may have fallen back to defaults).
    Loaded(WorkbenchState),
    /// The file was written by a newer `SQLer`; it must not be overwritten.
    Newer(u32),
    /// The file is unusable; the message says why.
    Invalid(String),
}

fn number(v: &Value) -> Option<f32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    match v {
        Value::Float(f) => Some(*f as f32),
        Value::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

fn size_of(t: &Table, range: &RangeInclusive<f32>, default: f32) -> f32 {
    match t.get("size").and_then(number) {
        Some(s) if s.is_finite() => s.clamp(*range.start(), *range.end()),
        _ => default,
    }
}

fn panel_of(
    v: Option<&Value>,
    mut base: PanelState,
    range: &RangeInclusive<f32>,
    views: &[&str],
) -> PanelState {
    let Some(t) = v.and_then(Value::as_table) else {
        return base;
    };
    if let Some(b) = t.get("visible").and_then(Value::as_bool) {
        base.visible = b;
    }
    base.size = size_of(t, range, base.size);
    if let Some(s) = t.get("view").and_then(Value::as_str)
        && views.contains(&s)
    {
        s.clone_into(&mut base.view);
    }
    base
}

fn window_of(v: Option<&Value>) -> Option<WindowState> {
    let t = v?.as_table()?;
    let width = number(t.get("width")?)?;
    let height = number(t.get("height")?)?;
    (WINDOW_SIZE_RANGE.contains(&width) && WINDOW_SIZE_RANGE.contains(&height))
        .then_some(WindowState { width, height })
}

fn strings_of(v: Option<&Value>, cap: usize) -> Vec<String> {
    let Some(a) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    a.iter()
        .filter_map(Value::as_str)
        .filter(|s| seen.insert(*s))
        .take(cap)
        .map(str::to_owned)
        .collect()
}

fn parse(text: &str) -> WorkbenchLoad {
    let table: Table = match text.parse() {
        Ok(t) => t,
        Err(e) => return WorkbenchLoad::Invalid(e.to_string()),
    };
    match table.get("version") {
        None => {}
        Some(Value::Integer(n)) if *n >= 1 => {
            if *n > i64::from(WORKBENCH_VERSION) {
                return WorkbenchLoad::Newer(u32::try_from(*n).unwrap_or(u32::MAX));
            }
        }
        Some(_) => {
            return WorkbenchLoad::Invalid("version must be an integer of at least 1".to_owned());
        }
    }
    WorkbenchLoad::Loaded(WorkbenchState {
        version: WORKBENCH_VERSION,
        side_panel: panel_of(
            table.get("side_panel"),
            PanelState::side(),
            &SIDE_PANEL_SIZE_RANGE,
            &SIDE_VIEWS,
        ),
        bottom_panel: panel_of(
            table.get("bottom_panel"),
            PanelState::bottom(),
            &BOTTOM_PANEL_SIZE_RANGE,
            &BOTTOM_VIEWS,
        ),
        window: window_of(table.get("window")),
        recent_commands: strings_of(table.get("recent_commands"), MAX_RECENT_COMMANDS),
        recent_connections: strings_of(table.get("recent_connections"), MAX_RECENT_CONNECTIONS),
    })
}

impl ConfigDir {
    /// Path of `workbench.toml`.
    #[must_use]
    pub fn workbench_path(&self) -> std::path::PathBuf {
        self.root().join("workbench.toml")
    }

    /// Loads `workbench.toml` leniently; never fails.
    ///
    /// An I/O error, a TOML syntax error, a non-table document and a bad `version` give
    /// [`WorkbenchLoad::Invalid`]. A `version` above [`WORKBENCH_VERSION`] gives
    /// [`WorkbenchLoad::Newer`]. Every other field falls back to its default.
    #[must_use]
    pub fn load_workbench_state(&self) -> WorkbenchLoad {
        match read_optional(&self.workbench_path()) {
            Ok(None) => WorkbenchLoad::Missing,
            Ok(Some(text)) => parse(&text),
            Err(e) => WorkbenchLoad::Invalid(e.to_string()),
        }
    }

    /// Path of the backup `workbench.toml.bak`.
    #[must_use]
    pub fn workbench_backup_path(&self) -> std::path::PathBuf {
        self.root().join("workbench.toml.bak")
    }

    /// Writes `workbench.toml`, creating the directory when missing.
    ///
    /// A file that does not load ([`WorkbenchLoad::Invalid`]) is never destroyed silently: it
    /// is copied to `workbench.toml.bak` (one backup, replaced by a later one) before it is
    /// overwritten. When the copy fails, nothing is overwritten and the error is returned.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] when `state.version` is not [`WORKBENCH_VERSION`] (a newer
    /// file is never overwritten), [`ConfigError::Serialize`] or [`ConfigError::Write`] (also
    /// when the backup of an invalid file cannot be made).
    pub fn save_workbench_state(&self, state: &WorkbenchState) -> Result<(), ConfigError> {
        let path = self.workbench_path();
        if state.version != WORKBENCH_VERSION {
            return Err(ConfigError::Invalid {
                path,
                message: format!(
                    "cannot write version {} (this build writes version {WORKBENCH_VERSION})",
                    state.version
                ),
            });
        }
        let text = toml::to_string_pretty(state).map_err(|e| ConfigError::Serialize {
            path: path.clone(),
            message: e.to_string(),
        })?;
        if matches!(self.load_workbench_state(), WorkbenchLoad::Invalid(_)) {
            let backup = self.workbench_backup_path();
            std::fs::copy(&path, &backup).map_err(|source| ConfigError::Write {
                path: backup,
                source,
            })?;
        }
        write_atomic(&path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(text: &str) -> WorkbenchState {
        match parse(text) {
            WorkbenchLoad::Loaded(s) => s,
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

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
        let p = std::env::temp_dir().join(format!("sqler-config-{}-wb-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        let d = ConfigDir::new(&p);
        (Scratch(p), d)
    }

    #[test]
    fn defaults() {
        let s = WorkbenchState::default();
        assert_eq!(s.version, 1);
        assert_eq!(
            s.side_panel,
            PanelState {
                visible: true,
                size: 260.0,
                view: "connections".into()
            }
        );
        assert_eq!(
            s.bottom_panel,
            PanelState {
                visible: false,
                size: 380.0,
                view: "results".into()
            }
        );
        assert_eq!(s.window, None);
        assert_eq!(loaded(""), s);
    }

    #[test]
    fn missing_file() {
        let (_t, d) = dir();
        assert_eq!(d.load_workbench_state(), WorkbenchLoad::Missing);
        assert_eq!(d.workbench_path(), d.root().join("workbench.toml"));
    }

    #[test]
    fn round_trip() {
        let (_t, d) = dir();
        let s = WorkbenchState {
            version: 1,
            side_panel: PanelState {
                visible: false,
                size: 300.5,
                view: "saved-queries".into(),
            },
            bottom_panel: PanelState {
                visible: true,
                size: 333.0,
                view: "history".into(),
            },
            window: Some(WindowState {
                width: 1280.0,
                height: 800.0,
            }),
            recent_commands: vec!["tab:close".into(), "app:quit".into()],
            recent_connections: vec!["local".into()],
        };
        d.save_workbench_state(&s).unwrap();
        assert_eq!(d.load_workbench_state(), WorkbenchLoad::Loaded(s));
    }

    #[test]
    fn default_round_trip_without_window() {
        let (_t, d) = dir();
        let s = WorkbenchState::default();
        d.save_workbench_state(&s).unwrap();
        assert_eq!(d.load_workbench_state(), WorkbenchLoad::Loaded(s));
    }

    #[test]
    fn newer_and_invalid_versions() {
        assert_eq!(parse("version = 2"), WorkbenchLoad::Newer(2));
        assert_eq!(
            parse("version = 99999999999"),
            WorkbenchLoad::Newer(u32::MAX)
        );
        for bad in [
            "version = 0",
            "version = -1",
            "version = \"1\"",
            "version = 1.0",
        ] {
            assert!(matches!(parse(bad), WorkbenchLoad::Invalid(_)), "{bad}");
        }
        assert_eq!(loaded("version = 1").version, 1);
    }

    #[test]
    fn syntax_error_is_invalid() {
        assert!(matches!(parse("side_panel = ["), WorkbenchLoad::Invalid(_)));
    }

    #[test]
    fn unknown_keys_ignored() {
        let s = loaded("future = 1\n[side_panel]\nextra = true\nsize = 300\n");
        assert_eq!(s.side_panel.size, 300.0);
    }

    #[test]
    fn sizes_clamp_and_fall_back() {
        assert_eq!(loaded("[side_panel]\nsize = 50").side_panel.size, 180.0);
        assert_eq!(loaded("[side_panel]\nsize = 9999.0").side_panel.size, 600.0);
        assert_eq!(loaded("[side_panel]\nsize = \"x\"").side_panel.size, 260.0);
        assert_eq!(loaded("[side_panel]\nsize = nan").side_panel.size, 260.0);
        assert_eq!(loaded("[side_panel]\nsize = inf").side_panel.size, 260.0);
        assert_eq!(loaded("[bottom_panel]\nsize = 1").bottom_panel.size, 120.0);
        assert_eq!(
            loaded("[bottom_panel]\nsize = 1e9").bottom_panel.size,
            4096.0
        );
    }

    #[test]
    fn views_and_wrong_types_fall_back() {
        let s = loaded("[side_panel]\nview = \"foo\"\nvisible = \"yes\"\n[bottom_panel]\nview = 3");
        assert_eq!(s.side_panel.view, "connections");
        assert!(s.side_panel.visible);
        assert_eq!(s.bottom_panel.view, "results");
        let s = loaded("side_panel = 5\n[bottom_panel]\nview = \"messages\"\nvisible = true");
        assert_eq!(s.side_panel, PanelState::side());
        assert_eq!(s.bottom_panel.view, "messages");
        assert!(s.bottom_panel.visible);
    }

    #[test]
    fn the_audit_bottom_view_loads_and_unknown_values_fall_back() {
        assert_eq!(
            loaded("[bottom_panel]\nview = \"ai-audit\"")
                .bottom_panel
                .view,
            "ai-audit"
        );
        assert_eq!(
            loaded("[bottom_panel]\nview = \"audit\"").bottom_panel.view,
            "results"
        );
        let (_t, d) = dir();
        let mut s = WorkbenchState::default();
        s.bottom_panel.view = BOTTOM_VIEWS[3].into();
        d.save_workbench_state(&s).unwrap();
        assert_eq!(d.load_workbench_state(), WorkbenchLoad::Loaded(s));
    }

    #[test]
    fn the_ai_side_view_loads_and_older_files_are_unchanged() {
        assert_eq!(loaded("[side_panel]\nview = \"ai\"").side_panel.view, "ai");
        let old = loaded("[side_panel]\nview = \"saved-queries\"\nsize = 300");
        assert_eq!(old.side_panel.view, "saved-queries");
        assert_eq!(old.side_panel.size, 300.0);
    }

    #[test]
    fn window_validation() {
        assert_eq!(loaded("[window]\nwidth = 10\nheight = 10").window, None);
        assert_eq!(loaded("[window]\nwidth = 800").window, None);
        assert_eq!(loaded("[window]\nwidth = nan\nheight = 800").window, None);
        assert_eq!(loaded("window = 1").window, None);
        assert_eq!(
            loaded("[window]\nwidth = 800\nheight = 600.5").window,
            Some(WindowState {
                width: 800.0,
                height: 600.5
            })
        );
    }

    #[test]
    fn recent_lists_filtered_deduped_capped() {
        let many: Vec<String> = (0..12).map(|i| format!("\"c{i}\"")).collect();
        let text = format!(
            "recent_commands = [{}]\nrecent_connections = [\"a\", 1, \"a\", \"b\", \"c\", \"d\", \"e\", \"f\"]",
            many.join(", ")
        );
        let s = loaded(&text);
        assert_eq!(s.recent_commands.len(), 10);
        assert_eq!(s.recent_commands[0], "c0");
        assert_eq!(s.recent_connections, ["a", "b", "c", "d", "e"]);
        assert_eq!(
            loaded("recent_commands = \"x\"").recent_commands,
            Vec::<String>::new()
        );
    }

    #[test]
    fn save_refuses_other_versions() {
        let (_t, d) = dir();
        let s = WorkbenchState {
            version: 2,
            ..WorkbenchState::default()
        };
        assert!(matches!(
            d.save_workbench_state(&s),
            Err(ConfigError::Invalid { .. })
        ));
        assert!(!d.workbench_path().exists());
    }

    #[test]
    fn save_over_an_invalid_file_keeps_a_backup() {
        let (_t, d) = dir();
        let bad = "side_panel = [oops\n";
        std::fs::write(d.workbench_path(), bad).unwrap();
        assert!(matches!(
            d.load_workbench_state(),
            WorkbenchLoad::Invalid(_)
        ));
        let s = WorkbenchState::default();
        d.save_workbench_state(&s).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.workbench_backup_path()).unwrap(),
            bad
        );
        assert_eq!(d.load_workbench_state(), WorkbenchLoad::Loaded(s.clone()));
        // A later save over a valid file leaves the backup alone.
        d.save_workbench_state(&s).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.workbench_backup_path()).unwrap(),
            bad
        );
    }

    #[test]
    fn save_does_not_overwrite_when_the_backup_fails() {
        let (_t, d) = dir();
        let bad = "version = 0\n";
        std::fs::write(d.workbench_path(), bad).unwrap();
        // A directory where the backup goes makes the copy fail.
        std::fs::create_dir(d.workbench_backup_path()).unwrap();
        assert!(matches!(
            d.save_workbench_state(&WorkbenchState::default()),
            Err(ConfigError::Write { .. })
        ));
        assert_eq!(std::fs::read_to_string(d.workbench_path()).unwrap(), bad);
    }

    #[test]
    fn save_over_an_unreadable_file_fails_and_keeps_it() {
        let (_t, d) = dir();
        // A directory in place of the file cannot be read or copied.
        std::fs::create_dir(d.workbench_path()).unwrap();
        assert!(d.save_workbench_state(&WorkbenchState::default()).is_err());
        assert!(d.workbench_path().is_dir());
    }

    #[test]
    fn io_error_is_invalid() {
        let (_t, d) = dir();
        std::fs::create_dir(d.workbench_path()).unwrap();
        assert!(matches!(
            d.load_workbench_state(),
            WorkbenchLoad::Invalid(_)
        ));
    }

    #[test]
    #[ignore = "timing probe: run with --release --ignored --nocapture"]
    fn load_timing() {
        let (_t, d) = dir();
        let state = WorkbenchState {
            window: Some(WindowState {
                width: 1280.0,
                height: 800.0,
            }),
            recent_commands: (0..10).map(|i| format!("tab:cmd-{i}")).collect(),
            recent_connections: (0..5).map(|i| format!("conn-{i}")).collect(),
            ..WorkbenchState::default()
        };
        d.save_workbench_state(&state).unwrap();
        let mut times: Vec<std::time::Duration> = (0..1000)
            .map(|_| {
                let start = std::time::Instant::now();
                let load = d.load_workbench_state();
                let took = start.elapsed();
                assert!(matches!(load, WorkbenchLoad::Loaded(_)));
                took
            })
            .collect();
        times.sort();
        println!("workbench load p50 {:?} max {:?}", times[500], times[999]);
    }
}
