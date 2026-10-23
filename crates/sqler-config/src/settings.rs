// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::fs::{read_optional, write_atomic};
use crate::{ConfigDir, ConfigError};

/// Accepted UI font sizes in points.
pub const FONT_SIZE_RANGE: RangeInclusive<u16> = 8..=48;

/// Accepted values of `result_page_rows`: rows the results grid loads per page.
pub const RESULT_PAGE_ROWS_RANGE: RangeInclusive<u32> = 1_000..=10_000_000;

/// Default of `result_page_rows`.
pub const DEFAULT_RESULT_PAGE_ROWS: u32 = 100_000;

/// Colour theme selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ThemePreference {
    /// Always light.
    Light,
    /// Always dark.
    Dark,
    /// Follow the operating system.
    #[default]
    System,
}

/// User settings stored in `settings.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Settings {
    /// Colour theme.
    pub theme: ThemePreference,
    /// UI font size in points; must be within [`FONT_SIZE_RANGE`].
    pub ui_font_size: u16,
    /// Rows per result page; the grid stops loading when a page is full and "Fetch more"
    /// continues. Must be within [`RESULT_PAGE_ROWS_RANGE`].
    pub result_page_rows: u32,
    /// Whether `SQLer` may look for a newer version about once a day (ADR-0066). Off by default;
    /// while it is off no update request is ever made, including the manual command.
    pub check_for_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            ui_font_size: 14,
            result_page_rows: DEFAULT_RESULT_PAGE_ROWS,
            check_for_updates: false,
        }
    }
}

fn check_font_size(path: &std::path::Path, size: u16) -> Result<(), ConfigError> {
    if FONT_SIZE_RANGE.contains(&size) {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            path: path.to_owned(),
            message: format!(
                "ui_font_size {size} is out of range ({}..={})",
                FONT_SIZE_RANGE.start(),
                FONT_SIZE_RANGE.end()
            ),
        })
    }
}

fn check_result_page_rows(path: &std::path::Path, rows: u32) -> Result<(), ConfigError> {
    if RESULT_PAGE_ROWS_RANGE.contains(&rows) {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            path: path.to_owned(),
            message: format!(
                "result_page_rows {rows} is out of range ({}..={})",
                RESULT_PAGE_ROWS_RANGE.start(),
                RESULT_PAGE_ROWS_RANGE.end()
            ),
        })
    }
}

impl ConfigDir {
    /// Loads `settings.toml`; a missing file yields [`Settings::default`].
    ///
    /// # Errors
    /// [`ConfigError::Read`], [`ConfigError::Parse`] for malformed TOML, wrong
    /// types or unknown keys, and [`ConfigError::Invalid`] for an out-of-range
    /// font size or page size.
    pub fn load_settings(&self) -> Result<Settings, ConfigError> {
        let path = self.settings_path();
        let Some(text) = read_optional(&path)? else {
            return Ok(Settings::default());
        };
        let settings: Settings = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: e.to_string(),
        })?;
        check_font_size(&path, settings.ui_font_size)?;
        check_result_page_rows(&path, settings.result_page_rows)?;
        Ok(settings)
    }

    /// Writes `settings.toml`, creating the directory when missing.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] for an out-of-range font size or page size,
    /// [`ConfigError::Serialize`] or [`ConfigError::Write`].
    pub fn save_settings(&self, settings: &Settings) -> Result<(), ConfigError> {
        let path = self.settings_path();
        check_font_size(&path, settings.ui_font_size)?;
        check_result_page_rows(&path, settings.result_page_rows)?;
        let text = toml::to_string_pretty(settings).map_err(|e| ConfigError::Serialize {
            path: path.clone(),
            message: e.to_string(),
        })?;
        write_atomic(&path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Settings, toml::de::Error> {
        toml::from_str(text)
    }

    #[test]
    fn empty_is_default() {
        let s = parse("").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.theme, ThemePreference::System);
        assert_eq!(s.ui_font_size, 14);
        assert_eq!(s.result_page_rows, 100_000);
        assert!(!s.check_for_updates);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let s = parse("theme = \"dark\"").unwrap();
        assert_eq!((s.theme, s.ui_font_size), (ThemePreference::Dark, 14));
        let s = parse("ui_font_size = 20").unwrap();
        assert_eq!((s.theme, s.ui_font_size), (ThemePreference::System, 20));
        let s = parse("result_page_rows = 5000").unwrap();
        assert_eq!((s.result_page_rows, s.ui_font_size), (5000, 14));
    }

    #[test]
    fn check_for_updates_parses() {
        assert!(parse("check_for_updates = true").unwrap().check_for_updates);
        assert!(
            !parse("check_for_updates = false")
                .unwrap()
                .check_for_updates
        );
        assert!(!parse("theme = \"dark\"").unwrap().check_for_updates);
    }

    #[test]
    fn theme_values() {
        for (t, v) in [
            ("light", ThemePreference::Light),
            ("dark", ThemePreference::Dark),
            ("system", ThemePreference::System),
        ] {
            assert_eq!(parse(&format!("theme = \"{t}\"")).unwrap().theme, v);
        }
        assert!(parse("theme = \"Dark\"").is_err());
        assert!(parse("theme = \"blue\"").is_err());
    }

    #[test]
    fn unknown_key_and_wrong_type_rejected() {
        assert!(parse("them = \"dark\"").is_err());
        assert!(parse("ui_font_size = \"big\"").is_err());
        assert!(parse("ui_font_size = -1").is_err());
        assert!(parse("result_page_rows = \"many\"").is_err());
        assert!(parse("result_page_rows = -5").is_err());
        assert!(parse("result_page_rows = 1.5").is_err());
        assert!(parse("check_for_updates = \"yes\"").is_err());
        assert!(parse("check_for_updates = 1").is_err());
    }

    #[test]
    fn font_range_checked() {
        let p = std::path::Path::new("settings.toml");
        assert!(check_font_size(p, 8).is_ok());
        assert!(check_font_size(p, 48).is_ok());
        for bad in [0, 7, 49] {
            let e = check_font_size(p, bad).unwrap_err().to_string();
            assert!(e.contains("8..=48"), "{e}");
        }
    }

    #[test]
    fn result_page_rows_range_checked() {
        let p = std::path::Path::new("settings.toml");
        assert!(check_result_page_rows(p, 1_000).is_ok());
        assert!(check_result_page_rows(p, 10_000_000).is_ok());
        for bad in [0, 999, 10_000_001] {
            let e = check_result_page_rows(p, bad).unwrap_err().to_string();
            assert!(e.contains("1000..=10000000"), "{e}");
            assert!(
                e.contains(&format!("result_page_rows {bad} is out of range")),
                "{e}"
            );
        }
    }
}
