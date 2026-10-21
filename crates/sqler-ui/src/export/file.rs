// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The partial file of an export.
//!
//! The rows go to a hidden sibling of the target, `.<name>.<pid>-<job>.partial`, which is synced
//! and renamed over the target only after the writer has finished. Dropping the guard without a
//! commit (a failure, a cancel, a panic) deletes the partial file. An existing target is never
//! touched before the rename. Same protocol as `sqler-cli export`.
//!
//! No text made here contains the target path.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// A hidden sibling of the target: it becomes the target on [`commit`](Self::commit) and is
/// deleted on drop otherwise.
#[derive(Debug)]
pub struct PartialFile {
    path: PathBuf,
    target: PathBuf,
    keep: Option<File>,
    committed: bool,
}

impl PartialFile {
    /// Creates the partial file of `target` for export job `job`; returns the guard with the
    /// handle to write to.
    ///
    /// # Errors
    ///
    /// An [`io::Error`] if `target` is a directory or has no file name, or the file cannot be
    /// created (a missing folder, no permission, a leftover with the same name). Its text never
    /// holds the path.
    pub fn create(target: &Path, job: u64) -> io::Result<(Self, File)> {
        if target.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the target is a folder",
            ));
        }
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "the target has no file name")
        })?;
        let dir = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut partial_name = OsString::from(".");
        partial_name.push(name);
        partial_name.push(format!(".{}-{job}.partial", std::process::id()));
        let path = dir.join(partial_name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let keep = file.try_clone()?;
        Ok((
            Self {
                path,
                target: target.to_owned(),
                keep: Some(keep),
                committed: false,
            },
            file,
        ))
    }

    /// The path of the partial file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Syncs the file to disk, then renames it over the target.
    ///
    /// The rename replaces the directory entry. A target that is a symlink is replaced by a
    /// regular file (the file it pointed to is not touched). On Unix, replacing a regular file
    /// gives the new file the permissions of the old one (best effort); any other new file has
    /// the default permissions of the process (its umask).
    ///
    /// # Errors
    ///
    /// An [`io::Error`] if the sync or the rename fails; the partial file is deleted then.
    pub fn commit(mut self) -> io::Result<()> {
        if let Some(file) = self.keep.take() {
            #[cfg(unix)]
            if let Ok(old) = fs::symlink_metadata(&self.target)
                && old.is_file()
            {
                // Best effort: a failure leaves the default permissions.
                let _ = file.set_permissions(old.permissions());
            }
            file.sync_all()?;
        }
        fs::rename(&self.path, &self.target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            // The handle must be closed first (Windows cannot delete an open file).
            self.keep = None;
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
pub(crate) mod scratch {
    use std::fs;
    use std::path::PathBuf;

    /// A scratch folder that is removed with the guard.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sqler-ui-export-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(crate) fn assert_empty(&self) {
            assert_eq!(self.names(), Vec::<String>::new());
        }

        pub(crate) fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::scratch::Scratch;
    use super::*;

    #[test]
    fn drop_removes_the_partial_file() {
        let dir = Scratch::new("file-drop");
        let target = dir.0.join("out.csv");
        let (partial, mut file) = PartialFile::create(&target, 7).unwrap();
        file.write_all(b"abc").unwrap();
        let name = format!(".out.csv.{}-7.partial", std::process::id());
        assert_eq!(dir.names(), [name]);
        assert!(
            partial
                .path()
                .ends_with(format!(".out.csv.{}-7.partial", std::process::id()))
        );
        drop(file);
        drop(partial);
        dir.assert_empty();
    }

    #[test]
    fn commit_replaces_the_target() {
        let dir = Scratch::new("file-commit");
        let target = dir.0.join("out.csv");
        fs::write(&target, b"old").unwrap();
        let (partial, mut file) = PartialFile::create(&target, 1).unwrap();
        file.write_all(b"new").unwrap();
        // The target is untouched until the commit.
        assert_eq!(fs::read(&target).unwrap(), b"old");
        drop(file);
        partial.commit().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(dir.names(), ["out.csv"]);
    }

    #[test]
    fn a_failed_run_leaves_an_existing_target_intact() {
        let dir = Scratch::new("file-failed");
        let target = dir.0.join("out.csv");
        fs::write(&target, b"old").unwrap();
        {
            let (_partial, mut file) = PartialFile::create(&target, 2).unwrap();
            file.write_all(b"half").unwrap();
            // Dropped without a commit.
        }
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(dir.names(), ["out.csv"]);
    }

    #[test]
    fn a_folder_and_a_missing_folder_are_refused_without_the_path() {
        let dir = Scratch::new("file-refuse");
        let err = PartialFile::create(&dir.0, 1).unwrap_err();
        assert!(!err.to_string().contains("sqler-ui-export"), "{err}");
        let missing = dir.0.join("nope").join("out.csv");
        let err = PartialFile::create(&missing, 1).unwrap_err();
        assert!(!err.to_string().contains("nope"), "{err}");
        assert!(PartialFile::create(Path::new("/"), 1).is_err());
        dir.assert_empty();
    }

    #[test]
    fn a_leftover_with_the_same_name_is_not_overwritten() {
        let dir = Scratch::new("file-leftover");
        let target = dir.0.join("out.csv");
        let (first, file) = PartialFile::create(&target, 5).unwrap();
        assert!(PartialFile::create(&target, 5).is_err());
        drop(file);
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = Scratch::new("file-mode");
        let target = dir.0.join("out.csv");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        let (partial, mut file) = PartialFile::create(&target, 1).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        partial.commit().unwrap();
        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_replaced_by_a_regular_file_and_its_file_is_untouched() {
        let dir = Scratch::new("file-link");
        let real = dir.0.join("real.csv");
        let link = dir.0.join("link.csv");
        fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (partial, mut file) = PartialFile::create(&link, 1).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        partial.commit().unwrap();
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&link).unwrap(), b"new");
        assert_eq!(fs::read(&real).unwrap(), b"old");
    }
}
