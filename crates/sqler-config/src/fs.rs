// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fs;
use std::io;
use std::path::Path;

use crate::ConfigError;

/// Reads a UTF-8 file, returning `None` when it does not exist.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Writes `text` to a sibling `<name>.tmp` file, then renames it over `path`.
/// Creates the parent directory when missing.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    let werr = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(werr)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    fs::write(&tmp, text).map_err(werr)?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        werr(e)
    })
}
