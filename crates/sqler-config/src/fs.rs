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

/// Reads a file as raw bytes, returning `None` when it does not exist.
pub(crate) fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Writes `contents` to a sibling `<name>.tmp` file, flushes it to disk, then renames
/// it over `path`. Creates the parent directory when missing.
///
/// The temporary file is flushed (`fsync`) before the rename, so a process crash
/// leaves either the old or the new file. On Unix the parent directory is also
/// flushed after the rename, on a best-effort basis: a failure there is ignored
/// because the new file is already in place. Windows skips the directory flush.
/// This does not promise power-loss durability: on macOS `fsync` does not force
/// the drive cache to flush (that needs `F_FULLFSYNC`).
pub(crate) fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<(), ConfigError> {
    write_atomic_with(path, contents.as_ref(), false)
}

/// Like [`write_atomic`], but the file is private to the user: mode `0600` on Unix, also for
/// a stale temporary file that already existed. On Windows the profile ACL applies, as for
/// [`write_atomic`].
pub(crate) fn write_atomic_private(
    path: &Path,
    contents: impl AsRef<[u8]>,
) -> Result<(), ConfigError> {
    write_atomic_with(path, contents.as_ref(), true)
}

fn write_atomic_with(path: &Path, contents: &[u8], private: bool) -> Result<(), ConfigError> {
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
        let mut options = fs::OpenOptions::new();
        if private {
            // A planted file or symlink at the tmp path must not be followed or truncated:
            // remove it (a symlink is removed, not its target), then create exclusively.
            match fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            options.write(true).create_new(true);
        } else {
            options.write(true).create(true).truncate(true);
        }
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        let _ = private;
        file.write_all(contents)?;
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

    #[cfg(unix)]
    #[test]
    fn write_atomic_private_is_0600_even_over_an_old_tmp_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.toml");
        let tmp = dir.join("f.toml.tmp");
        fs::write(&tmp, "stale").unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();
        write_atomic_private(&path, "secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_private_does_not_follow_a_symlink_at_the_tmp_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private-link");
        fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim.txt");
        fs::write(&victim, "keep me").unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.join("f.toml");
        std::os::unix::fs::symlink(&victim, dir.join("f.toml.tmp")).unwrap();
        write_atomic_private(&path, "secret").unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret");
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

    #[test]
    fn read_optional_bytes_missing_and_present() {
        let dir = scratch("bytes");
        let path = dir.join("f.bin");
        assert_eq!(read_optional_bytes(&path).unwrap(), None);
        write_atomic(&path, [0xff_u8, 0x00, b'\n']).unwrap();
        assert_eq!(
            read_optional_bytes(&path).unwrap(),
            Some(vec![0xff, 0x00, b'\n'])
        );
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
