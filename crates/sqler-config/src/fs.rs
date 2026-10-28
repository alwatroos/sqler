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

/// Writes `contents` to a uniquely named sibling `.<name>.….tmp` file, flushes it to disk, then renames
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

/// Like [`write_atomic`], but the file is private to the user: mode `0600` on Unix. On Windows the profile ACL applies, as for
/// [`write_atomic`].
pub(crate) fn write_atomic_private(
    path: &Path,
    contents: impl AsRef<[u8]>,
) -> Result<(), ConfigError> {
    write_atomic_with(path, contents.as_ref(), true)
}

/// Makes sure `path` exists and, on Unix, is private to the user (mode `0600`). Creates the parent
/// directory when missing. A new file is created exclusively with that mode; an existing file keeps
/// its content and only has its mode repaired. Elsewhere the profile ACL applies.
pub(crate) fn create_private_file(path: &Path) -> Result<(), ConfigError> {
    let werr = |source| ConfigError::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(werr)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(werr(e)),
    }
    set_private(path).map_err(werr)
}

/// Sets mode `0600` on an existing file (Unix); a no-op elsewhere.
pub(crate) fn set_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
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
    let tmp = unique_tmp_path(path);
    let write_tmp = || -> io::Result<()> {
        // A fresh, unpredictable name per write, created exclusively: concurrent writers never
        // share a temporary file and a planted file or symlink is never followed or truncated.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
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

/// A temporary path beside `path`, unique per process, call and moment:
/// `.<name>.<pid>.<counter>.<nanos>.tmp`.
fn unique_tmp_path(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(
        ".{}.{}.{nanos:08x}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
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

    fn leftovers(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count()
    }

    #[test]
    fn tmp_names_are_unique_and_beside_the_target() {
        let path = Path::new("/x/y/f.toml");
        let (a, b) = (unique_tmp_path(path), unique_tmp_path(path));
        assert_ne!(a, b);
        assert_eq!(a.parent(), path.parent());
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".f.toml.")
        );
    }

    #[test]
    fn concurrent_writers_do_not_share_a_tmp_file() {
        let dir = scratch("concurrent");
        let path = dir.join("f.toml");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        write_atomic_private(&path, format!("writer {i} round {j}\n")).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(fs::read_to_string(&path).unwrap().starts_with("writer "));
        assert_eq!(leftovers(&dir), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_creates_replaces_and_leaves_no_tmp() {
        let dir = scratch("atomic");
        let path = dir.join("nested").join("f.toml");
        write_atomic(&path, "one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        write_atomic(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(leftovers(&dir.join("nested")), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_private_is_0600_and_ignores_an_old_tmp_file() {
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
        assert_eq!(fs::read_to_string(&tmp).unwrap(), "stale");
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
        assert_eq!(leftovers(&dir), 0);
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
