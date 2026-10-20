// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::fs;
use std::io::{self, Write};
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

/// Writes `text` to a sibling `<name>.tmp` file, flushes it to disk, then renames
/// it over `path`. Creates the parent directory when missing.
///
/// The temporary file is flushed (`fsync`) before the rename, so a process crash
/// leaves either the old or the new file. On Unix the parent directory is also
/// flushed after the rename, on a best-effort basis: a failure there is ignored
/// because the new file is already in place. Windows skips the directory flush.
/// This does not promise power-loss durability: on macOS `fsync` does not force
/// the drive cache to flush (that needs `F_FULLFSYNC`).
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    let werr = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(werr)?;
    }
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let write_tmp = || -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()
    };
    if let Err(e) = write_tmp() {
        let _ = fs::remove_file(&tmp);
        return Err(werr(e));
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        werr(e)
    })?;
    #[cfg(unix)]
    let _ = sync_dir(parent.unwrap_or_else(|| Path::new(".")));
    Ok(())
}

/// Flushes a directory entry change (the rename) to disk.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("sqler-config-{}-fs-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn write_atomic_creates_replaces_and_leaves_no_tmp() {
        let dir = scratch("atomic");
        let path = dir.join("nested").join("f.toml");
        write_atomic(&path, "one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        write_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_file_name("f.toml.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_failure_cleans_tmp_and_keeps_target() {
        let dir = scratch("fail");
        let path = dir.join("f.toml");
        write_atomic(&path, "keep").unwrap();
        // A directory in the way of the rename target makes the rename fail.
        let blocked = dir.join("blocked");
        fs::create_dir_all(blocked.join("sub")).unwrap();
        assert!(write_atomic(&blocked, "x").is_err());
        assert!(!dir.join("blocked.tmp").exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn sync_dir_works_on_a_directory() {
        let dir = scratch("syncdir");
        fs::create_dir_all(&dir).unwrap();
        sync_dir(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
