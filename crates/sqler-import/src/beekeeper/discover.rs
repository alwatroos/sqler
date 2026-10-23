// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Where a connection export is likely to be. Beekeeper writes the file where the user chooses
//! (`File > Export Connections`), so these are guesses; an explicit path is the primary input. The
//! application's own SQLite store (`app.db`) is not read: it is a live database and holds the
//! passwords.

use std::path::PathBuf;

use crate::source::HostEnv;

const FILE: &str = "beekeeper-connections.json";
const ALT_FILE: &str = "connections.json";

pub(super) fn default_locations(env: &HostEnv) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = &env.home {
        for dir in ["Downloads", "Documents"] {
            out.push(home.join(dir).join(FILE));
            out.push(home.join(dir).join(ALT_FILE));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::HostOs;

    #[test]
    fn per_os_is_the_same_and_home_based() {
        let home = PathBuf::from("home").join("alice");
        for os in [HostOs::Linux, HostOs::Mac, HostOs::Windows] {
            let env = HostEnv {
                os,
                home: Some(home.clone()),
                appdata: Some(PathBuf::from("appdata")),
            };
            let got = default_locations(&env);
            assert_eq!(got.len(), 4);
            assert!(got.iter().all(|p| p.starts_with(&home)));
            assert_eq!(got[0], home.join("Downloads").join(FILE));
        }
    }

    #[test]
    fn no_home_gives_no_candidates() {
        let none = HostEnv {
            os: HostOs::Linux,
            home: None,
            appdata: None,
        };
        assert_eq!(default_locations(&none), Vec::<PathBuf>::new());
    }
}
