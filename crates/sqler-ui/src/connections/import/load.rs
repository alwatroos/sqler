// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The blocking half of the import wizard: finding the default files and reading one. No GPUI; the
//! view runs these on `SessionBridge::blocking`.

use std::path::{Path, PathBuf};

use sqler_import::{HostEnv, ImportError, SourceReport, discover, read_source, source, sources};

/// For every source, the files that exist at its default locations (in `sources()` order).
#[must_use]
pub fn discover_all(env: &HostEnv) -> Vec<(&'static str, Vec<PathBuf>)> {
    sources()
        .iter()
        .map(|s| (s.id(), discover(s.as_ref(), env)))
        .collect()
}

/// Reads `path` within the size limit and parses it with the source `source_id`.
///
/// # Errors
/// The [`ImportError`] of the read or the parse; an unknown source id is
/// `UnrecognisedFormat("unknown source")`.
pub fn load(source_id: &str, path: &Path) -> Result<SourceReport, ImportError> {
    let bytes = read_source(path)?;
    match source(source_id) {
        Some(s) => s.parse(&bytes),
        None => Err(ImportError::UnrecognisedFormat("unknown source")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_reads_and_parses_a_file() {
        let dir = std::env::temp_dir().join(format!("sqler-import-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("export.json");
        std::fs::write(&path, super::super::fixtures::BEEKEEPER).unwrap();
        let report = load("beekeeper", &path);
        let unknown = load("nope", &path);
        let missing = load("beekeeper", &dir.join("missing.json"));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.unwrap().entries.len() > 3);
        assert_eq!(
            unknown,
            Err(ImportError::UnrecognisedFormat("unknown source"))
        );
        assert!(matches!(missing, Err(ImportError::Io { .. })));
    }

    #[test]
    fn discover_all_lists_every_source() {
        let env = HostEnv {
            os: sqler_import::HostOs::Linux,
            home: Some(PathBuf::from("/nonexistent-home-for-test")),
            appdata: None,
        };
        let found = discover_all(&env);
        let ids: Vec<&str> = found.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["pgadmin", "beekeeper", "azure-data-studio"]);
        assert!(found.iter().all(|(_, paths)| paths.is_empty()));
    }
}
