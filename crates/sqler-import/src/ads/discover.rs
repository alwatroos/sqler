// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Where Azure Data Studio keeps its user `settings.json`: `<user data>/User/settings.json`,
//! where the user data directory is `azuredatastudio` (and `azuredatastudio-insiders`) under
//! `%APPDATA%` on Windows, `~/Library/Application Support` on macOS and `~/.config` elsewhere.

use std::path::{Path, PathBuf};

use crate::source::{HostEnv, HostOs};

const APPS: [&str; 2] = ["azuredatastudio", "azuredatastudio-insiders"];

fn settings(base: &Path, app: &str) -> PathBuf {
    base.join(app).join("User").join("settings.json")
}

pub(super) fn default_locations(env: &HostEnv) -> Vec<PathBuf> {
    let base = match env.os {
        HostOs::Windows => env.appdata.clone(),
        HostOs::Mac => env
            .home
            .as_ref()
            .map(|h| h.join("Library").join("Application Support")),
        HostOs::Linux => env.home.as_ref().map(|h| h.join(".config")),
    };
    base.map(|b| APPS.iter().map(|a| settings(&b, a)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(os: HostOs) -> HostEnv {
        HostEnv {
            os,
            home: Some(PathBuf::from("home").join("alice")),
            appdata: Some(PathBuf::from("appdata")),
        }
    }

    fn tail(app: &str) -> PathBuf {
        PathBuf::from(app).join("User").join("settings.json")
    }

    #[test]
    fn per_os() {
        let home = PathBuf::from("home").join("alice");
        let linux = home.join(".config");
        let mac = home.join("Library").join("Application Support");
        let win = PathBuf::from("appdata");
        for (os, base) in [
            (HostOs::Linux, linux),
            (HostOs::Mac, mac),
            (HostOs::Windows, win),
        ] {
            assert_eq!(
                default_locations(&env(os)),
                vec![
                    base.join(tail("azuredatastudio")),
                    base.join(tail("azuredatastudio-insiders"))
                ],
                "{os:?}"
            );
        }
    }

    #[test]
    fn missing_dirs_give_no_candidates() {
        for os in [HostOs::Linux, HostOs::Mac, HostOs::Windows] {
            let none = HostEnv {
                os,
                home: None,
                appdata: None,
            };
            assert_eq!(default_locations(&none), Vec::<PathBuf>::new(), "{os:?}");
        }
        let win_without_appdata = HostEnv {
            appdata: None,
            ..env(HostOs::Windows)
        };
        assert_eq!(
            default_locations(&win_without_appdata),
            Vec::<PathBuf>::new()
        );
    }
}
