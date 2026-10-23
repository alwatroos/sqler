// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `SettingsModel`: the user settings held by the workspace, checked on change and saved only
//! through `sqler-config`. Plain Rust; the settings tab view is behind the `gui` feature.

#[cfg(feature = "gui")]
pub(crate) mod view;

use sqler_config::{ConfigDir, ConfigError, FONT_SIZE_RANGE, Settings, ThemePreference};

/// The settings, where they are saved and what went wrong with loading or saving them.
#[derive(Debug, Default)]
pub struct SettingsModel {
    settings: Settings,
    dir: Option<ConfigDir>,
    load_error: Option<String>,
    save_error: Option<String>,
}

impl SettingsModel {
    /// Loads the settings from the config directory.
    ///
    /// If there is no config directory or `settings.toml` cannot be loaded, the defaults are
    /// used, nothing is ever written (a file the user edited is never overwritten) and the
    /// reason is available from [`notice`](Self::notice).
    #[must_use]
    pub fn load(dir: Result<ConfigDir, ConfigError>) -> Self {
        match dir.and_then(|dir| dir.load_settings().map(|settings| (dir, settings))) {
            Ok((dir, settings)) => Self {
                settings,
                dir: Some(dir),
                load_error: None,
                save_error: None,
            },
            Err(err) => Self {
                load_error: Some(err.to_string()),
                ..Self::default()
            },
        }
    }

    /// The current settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Sets the theme preference; `true` if it changed.
    pub fn set_theme(&mut self, theme: ThemePreference) -> bool {
        if self.settings.theme == theme {
            return false;
        }
        self.settings.theme = theme;
        true
    }

    /// Sets the UI font size in points; `false` if unchanged or outside [`FONT_SIZE_RANGE`].
    pub fn set_ui_font_size(&mut self, size: u16) -> bool {
        if !FONT_SIZE_RANGE.contains(&size) || self.settings.ui_font_size == size {
            return false;
        }
        self.settings.ui_font_size = size;
        true
    }

    /// Turns the automatic update check on or off; `true` if it changed.
    pub fn set_check_for_updates(&mut self, on: bool) -> bool {
        if self.settings.check_for_updates == on {
            return false;
        }
        self.settings.check_for_updates = on;
        true
    }

    /// Saves the settings. Without a config directory this does nothing. A failure is kept as
    /// the [`notice`](Self::notice); a success clears it.
    pub fn save(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        self.save_error = dir
            .save_settings(&self.settings)
            .err()
            .map(|err| err.to_string());
    }

    /// Whether changes are saved (a usable config directory exists).
    #[must_use]
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    /// The last save error, else the load error.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.save_error.as_deref().or(self.load_error.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    /// Fresh per-test directory (not created), removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(test: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!("sqler-ui-settings-{}-{test}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            let _ = fs::remove_file(&p);
            Self(p)
        }
        fn dir(&self) -> ConfigDir {
            ConfigDir::new(&self.0)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn missing_file_gives_defaults_and_is_persistent() {
        let s = Scratch::new("missing");
        let m = SettingsModel::load(Ok(s.dir()));
        assert_eq!(m.settings(), &Settings::default());
        assert!(m.persistent());
        assert_eq!(m.notice(), None);
    }

    #[test]
    fn valid_file_is_loaded() {
        let s = Scratch::new("valid");
        fs::create_dir_all(&s.0).unwrap();
        fs::write(
            s.dir().settings_path(),
            "theme = \"dark\"\nui_font_size = 20\n",
        )
        .unwrap();
        let m = SettingsModel::load(Ok(s.dir()));
        assert_eq!(m.settings().theme, ThemePreference::Dark);
        assert_eq!(m.settings().ui_font_size, 20);
        assert!(m.persistent());
    }

    #[test]
    fn malformed_file_gives_defaults_notice_and_never_writes() {
        let s = Scratch::new("malformed");
        fs::create_dir_all(&s.0).unwrap();
        let path = s.dir().settings_path();
        fs::write(&path, "theme = [").unwrap();
        let mut m = SettingsModel::load(Ok(s.dir()));
        assert_eq!(m.settings(), &Settings::default());
        assert!(!m.persistent());
        assert!(m.notice().is_some());
        assert!(m.set_ui_font_size(20));
        m.save();
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = [");
    }

    #[test]
    fn no_config_dir_gives_defaults_and_notice() {
        let m = SettingsModel::load(Err(ConfigError::NoConfigDir));
        assert_eq!(m.settings(), &Settings::default());
        assert!(!m.persistent());
        assert!(m.notice().is_some());
    }

    #[test]
    fn set_theme_reports_change() {
        let mut m = SettingsModel::default();
        assert!(!m.set_theme(ThemePreference::System));
        assert!(m.set_theme(ThemePreference::Dark));
        assert!(!m.set_theme(ThemePreference::Dark));
        assert_eq!(m.settings().theme, ThemePreference::Dark);
    }

    #[test]
    fn set_font_size_checks_range_and_change() {
        let mut m = SettingsModel::default();
        assert!(!m.set_ui_font_size(14));
        assert!(!m.set_ui_font_size(7));
        assert!(!m.set_ui_font_size(49));
        assert_eq!(m.settings().ui_font_size, 14);
        assert!(m.set_ui_font_size(8));
        assert!(m.set_ui_font_size(48));
        assert_eq!(m.settings().ui_font_size, 48);
    }

    #[test]
    fn save_round_trips() {
        let s = Scratch::new("roundtrip");
        let mut m = SettingsModel::load(Ok(s.dir()));
        m.set_theme(ThemePreference::Light);
        m.set_ui_font_size(18);
        m.save();
        assert_eq!(m.notice(), None);
        let loaded = s.dir().load_settings().unwrap();
        assert_eq!(&loaded, m.settings());
        assert_eq!(loaded.theme, ThemePreference::Light);
        assert_eq!(loaded.ui_font_size, 18);
    }

    #[test]
    fn save_failure_sets_notice_and_good_save_clears_it() {
        let s = Scratch::new("savefail");
        // The root is a regular file, so the directory cannot be created.
        fs::write(&s.0, "x").unwrap();
        let mut m = SettingsModel {
            dir: Some(s.dir()),
            ..SettingsModel::default()
        };
        assert!(m.persistent());
        m.set_ui_font_size(20);
        m.save();
        assert!(m.notice().is_some());
        fs::remove_file(&s.0).unwrap();
        m.save();
        assert_eq!(m.notice(), None);
        assert_eq!(s.dir().load_settings().unwrap().ui_font_size, 20);
    }

    #[test]
    fn check_for_updates_is_off_by_default_and_saved() {
        let s = Scratch::new("updates");
        let mut m = SettingsModel::load(Ok(s.dir()));
        assert!(!m.settings().check_for_updates);
        assert!(!m.set_check_for_updates(false));
        assert!(m.set_check_for_updates(true));
        assert!(!m.set_check_for_updates(true));
        m.save();
        assert!(s.dir().load_settings().unwrap().check_for_updates);
    }

    #[test]
    fn non_persistent_model_writes_no_file() {
        let s = Scratch::new("nofile");
        let mut m = SettingsModel::default();
        m.set_ui_font_size(20);
        m.save();
        assert!(!s.0.exists());
        assert!(!m.persistent());
    }
}
