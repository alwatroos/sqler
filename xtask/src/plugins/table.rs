// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The plugin table: one row per plugin. P13-P16 append one row each.

/// One plugin known to the build task.
pub struct PluginRow {
    /// Name used on the command line.
    pub name: &'static str,
    /// Directory holding `sqler.toml`, relative to the repo root, `/`-separated.
    pub dir: &'static str,
    /// Cargo package of the guest crate; `None` for a declarative-only plugin.
    pub package: Option<&'static str>,
}

/// Every plugin.
pub const PLUGINS: &[PluginRow] = &[
    PluginRow {
        name: "smoke",
        dir: "plugins/fixtures/smoke",
        package: Some("sqler-fixture-smoke"),
    },
    PluginRow {
        name: "probe",
        dir: "plugins/fixtures/probe",
        package: Some("sqler-fixture-probe"),
    },
    PluginRow {
        name: "ai-probe",
        dir: "plugins/fixtures/ai-probe",
        package: Some("sqler-fixture-ai-probe"),
    },
    PluginRow {
        name: "driver",
        dir: "plugins/fixtures/driver",
        package: Some("sqler-fixture-driver"),
    },
    PluginRow {
        name: "exporter",
        dir: "plugins/fixtures/exporter",
        package: Some("sqler-fixture-exporter"),
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use sqler_plugin_api::Manifest;

    use super::PLUGINS;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn row_dir(dir: &str) -> PathBuf {
        dir.split('/').fold(root(), |p, c| p.join(c))
    }

    #[test]
    fn names_are_unique() {
        let names: HashSet<_> = PLUGINS.iter().map(|r| r.name).collect();
        assert_eq!(names.len(), PLUGINS.len());
    }

    #[test]
    fn rows_never_share_a_dir_or_a_manifest_id() {
        let dirs: HashSet<_> = PLUGINS.iter().map(|r| r.dir).collect();
        assert_eq!(dirs.len(), PLUGINS.len(), "two rows share a dir");
        let ids: HashSet<String> = PLUGINS
            .iter()
            .map(|r| {
                let path = row_dir(r.dir).join("sqler.toml");
                let bytes = std::fs::read(&path).unwrap_or_default();
                match Manifest::parse_bytes(&bytes) {
                    Ok(m) => m.id().as_str().to_owned(),
                    Err(e) => panic!("{}: {e}", r.name),
                }
            })
            .collect();
        assert_eq!(ids.len(), PLUGINS.len(), "two rows share a manifest id");
    }

    #[test]
    fn manifests_parse_and_entry_matches_package() {
        for row in PLUGINS {
            let path = row_dir(row.dir).join("sqler.toml");
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let manifest =
                Manifest::parse_bytes(&bytes).unwrap_or_else(|e| panic!("{}: {e}", row.name));
            assert_eq!(
                manifest.entry().is_some(),
                row.package.is_some(),
                "{}",
                row.name
            );
        }
    }

    #[test]
    fn package_dirs_match_guest_workspace_members() {
        let text =
            std::fs::read_to_string(root().join("plugins").join("Cargo.toml")).unwrap_or_default();
        let doc: toml::Table = text.parse().unwrap_or_default();
        let members: HashSet<String> = doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let rows: HashSet<String> = PLUGINS
            .iter()
            .filter(|r| r.package.is_some())
            .map(|r| r.dir.strip_prefix("plugins/").unwrap_or(r.dir).to_owned())
            .collect();
        assert!(
            !members.is_empty(),
            "plugins/Cargo.toml has no workspace.members"
        );
        assert_eq!(rows, members);
    }
}
