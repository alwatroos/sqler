// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Where a `servers.json` export is likely to be. `pgAdmin` writes the file only on request, so
//! these are guesses; an explicit path is the primary input.

use std::path::PathBuf;

use crate::source::{HostEnv, HostOs};

const FILE: &str = "servers.json";

pub(super) fn default_locations(env: &HostEnv) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match env.os {
        HostOs::Linux | HostOs::Mac => {
            if let Some(home) = &env.home {
                out.push(home.join(".pgadmin").join(FILE));
            }
        }
        HostOs::Windows => {
            if let Some(appdata) = &env.appdata {
                out.push(appdata.join("pgAdmin").join(FILE));
            }
        }
    }
    if let Some(home) = &env.home {
        out.push(home.join("Downloads").join(FILE));
    }
    out
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

    #[test]
    fn per_os() {
        let home = PathBuf::from("home").join("alice");
        for os in [HostOs::Linux, HostOs::Mac] {
            assert_eq!(
                default_locations(&env(os)),
                vec![
                    home.join(".pgadmin").join(FILE),
                    home.join("Downloads").join(FILE)
                ]
            );
        }
        assert_eq!(
            default_locations(&env(HostOs::Windows)),
            vec![
                PathBuf::from("appdata").join("pgAdmin").join(FILE),
                home.join("Downloads").join(FILE)
            ]
        );
    }

    #[test]
    fn missing_dirs_give_fewer_candidates() {
        let none = HostEnv {
            os: HostOs::Linux,
            home: None,
            appdata: None,
        };
        assert_eq!(default_locations(&none), Vec::<PathBuf>::new());
        let win = HostEnv {
            os: HostOs::Windows,
            home: None,
            appdata: None,
        };
        assert_eq!(default_locations(&win), Vec::<PathBuf>::new());
    }
}
