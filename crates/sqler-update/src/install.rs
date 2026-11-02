// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Replacing the running program with a verified artifact (ADR-0078).
//!
//! The new program is staged in `.sqler-update/` beside the target, so the
//! final step is two renames on one volume. The old version is moved aside, not
//! deleted, and is put back when the second rename fails.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::download::{ArtifactKind, VerifiedArtifact, verify_file};
use crate::{MAX_BINARY_BYTES, STAGE_DIR_NAME, UpdateError};

/// Why `SQLer` does not replace itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ManualReason {
    /// Installed by a package manager or an installer.
    PackageManager,
    /// The folder it runs from is not writable.
    NotWritable,
    /// A development build.
    Development,
}

impl ManualReason {
    /// A short sentence for the user.
    #[must_use]
    pub fn note(self) -> &'static str {
        match self {
            Self::PackageManager => {
                "SQLer was installed by a package manager or installer. Update it the same way."
            }
            Self::NotWritable => {
                "SQLer cannot write to the folder it runs from. Install the update by hand."
            }
            Self::Development => "This is a development build. Install the update by hand.",
        }
    }
}

/// What an install would replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// The artifact kind that fits this install.
    pub kind: ArtifactKind,
    /// The file (archive install) or the `.app` folder (bundle) to replace.
    pub target: PathBuf,
    /// Set when `SQLer` must not replace itself.
    pub manual: Option<ManualReason>,
}

/// The result of [`install`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The new version is in place; start `launch` to run it.
    Installed {
        /// The program, or the `.app`, to start.
        launch: PathBuf,
    },
    /// Nothing was changed.
    Manual(ManualReason),
}

/// Injected steps that touch the system.
pub struct InstallHooks<'a> {
    /// Runs the new program with `--version` and returns its output.
    pub version_probe: &'a (dyn Fn(&Path) -> Option<String> + Sync),
    /// Extracts `SQLer.app` from a `.dmg` (arguments: dmg, destination).
    pub extract_app: &'a (dyn Fn(&Path, &Path) -> Result<(), UpdateError> + Sync),
    /// True when the extracted `.app` passes the platform signature check.
    pub bundle_check: &'a (dyn Fn(&Path) -> bool + Sync),
}

/// Decides what an install of the program at `exe` would replace. Path rules
/// only; nothing is read from disk.
#[must_use]
pub fn plan_install(exe: &Path, package_dirs: &[PathBuf]) -> InstallPlan {
    let (kind, target) = match bundle_root(exe) {
        Some(app) => (ArtifactKind::MacBundle, app),
        None => (ArtifactKind::Archive, exe.to_path_buf()),
    };
    let manual = if is_build_output(exe) {
        Some(ManualReason::Development)
    } else if package_dirs.iter().any(|d| target.starts_with(d)) {
        Some(ManualReason::PackageManager)
    } else {
        None
    };
    InstallPlan {
        kind,
        target,
        manual,
    }
}

/// `exe` is `<B>.app/Contents/MacOS/<file>`: returns `<B>.app`.
fn bundle_root(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_app = app.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        Path::new(n).extension().is_some_and(|e| e == "app") && n.len() > ".app".len()
    });
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && is_app)
        .then(|| app.to_path_buf())
}

/// An ancestor `target/debug` or `target/release`.
fn is_build_output(exe: &Path) -> bool {
    let parts: Vec<_> = exe.components().collect();
    parts.windows(2).any(|w| {
        w[0].as_os_str() == "target"
            && (w[1].as_os_str() == "debug" || w[1].as_os_str() == "release")
    })
}

/// Folders that a package manager or an installer owns.
#[must_use]
pub fn default_package_dirs() -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        [
            "/usr/bin",
            "/usr/lib",
            "/usr/libexec",
            "/usr/share",
            "/opt/homebrew",
            "/usr/local/Cellar",
            "/nix/store",
            "/snap",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    }
    #[cfg(windows)]
    {
        ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .into_iter()
            .filter_map(std::env::var_os)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Vec::new()
    }
}

fn stage_dir(target: &Path) -> Option<PathBuf> {
    target.parent().map(|p| p.join(STAGE_DIR_NAME))
}

fn remove_path(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// Removes the stage dir beside the install target of `exe`. Never fails.
pub fn clean_leftovers(exe: &Path) {
    let plan = plan_install(exe, &[]);
    if let Some(stage) = stage_dir(&plan.target) {
        remove_path(&stage);
    }
}

/// Replaces the program with the verified artifact `a`.
///
/// # Errors
///
/// [`UpdateError::ChangedAfterVerification`] when the file no longer matches
/// its hash; [`UpdateError::ArchiveLayout`], [`UpdateError::BundleSignature`],
/// [`UpdateError::VersionMismatch`] for a bad artifact; [`UpdateError::Swap`]
/// when the rename failed (the old version is unchanged); [`UpdateError::Io`].
pub fn install(
    a: &VerifiedArtifact,
    exe: &Path,
    package_dirs: &[PathBuf],
    h: &InstallHooks<'_>,
) -> Result<InstallOutcome, UpdateError> {
    let plan = plan_install(exe, package_dirs);
    if let Some(reason) = plan.manual {
        return Ok(InstallOutcome::Manual(reason));
    }
    if a.kind != plan.kind {
        return Err(UpdateError::ArchiveLayout);
    }
    let stage = stage_dir(&plan.target).ok_or(UpdateError::Io)?;
    remove_path(&stage);
    if let Err(e) = create_stage(&stage) {
        return if matches!(
            e.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
        ) {
            Ok(InstallOutcome::Manual(ManualReason::NotWritable))
        } else {
            Err(UpdateError::Io)
        };
    }
    match stage_and_swap(a, &plan, &stage, h) {
        Ok(()) => {
            let _ = std::fs::remove_file(stage.join(copy_name(a)));
            Ok(InstallOutcome::Installed {
                launch: plan.target,
            })
        }
        Err(e) => {
            // After a failed rollback the stage holds the only old copy.
            let rollback_failed =
                e == UpdateError::Swap && std::fs::symlink_metadata(&plan.target).is_err();
            if !rollback_failed {
                remove_path(&stage);
            }
            Err(e)
        }
    }
}

fn create_stage(stage: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        b.mode(0o700);
    }
    b.create(stage)
}

fn copy_name(a: &VerifiedArtifact) -> String {
    a.path
        .file_name()
        .and_then(|n| n.to_str())
        .map_or_else(|| "artifact".to_owned(), str::to_owned)
}

fn stage_and_swap(
    a: &VerifiedArtifact,
    plan: &InstallPlan,
    stage: &Path,
    h: &InstallHooks<'_>,
) -> Result<(), UpdateError> {
    let copy = stage.join(copy_name(a));
    std::fs::copy(&a.path, &copy).map_err(|_| UpdateError::Io)?;
    verify_file(&copy, &a.sha256, a.size).map_err(|e| match e {
        UpdateError::SizeMismatch | UpdateError::HashMismatch => {
            UpdateError::ChangedAfterVerification
        }
        other => other,
    })?;
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let (new, previous, probe) = match plan.kind {
        ArtifactKind::Archive => {
            let new = stage.join(format!("new{ext}"));
            extract_program(&copy, &new)?;
            (new.clone(), stage.join(format!("previous{ext}")), new)
        }
        ArtifactKind::MacBundle => {
            let app = stage.join("SQLer.app");
            (h.extract_app)(&copy, &app).map_err(|_| UpdateError::ArchiveLayout)?;
            let contents = app.join("Contents");
            let bin = contents.join("MacOS").join("sqler");
            if !is_plain_file(&contents.join("Info.plist")) || !is_plain_file(&bin) {
                return Err(UpdateError::ArchiveLayout);
            }
            if !(h.bundle_check)(&app) {
                return Err(UpdateError::BundleSignature);
            }
            (app, stage.join("previous.app"), bin)
        }
    };
    let token = a.version.to_string();
    let reported = (h.version_probe)(&probe).ok_or(UpdateError::VersionMismatch)?;
    if !reported.split_whitespace().any(|t| t == token) {
        return Err(UpdateError::VersionMismatch);
    }
    swap(&plan.target, &new, &previous)
}

fn is_plain_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// Moves `target` to `previous`, then `new` to `target`. When the second rename
/// fails the first is undone.
pub(crate) fn swap(target: &Path, new: &Path, previous: &Path) -> Result<(), UpdateError> {
    std::fs::rename(target, previous).map_err(|_| UpdateError::Swap)?;
    if std::fs::rename(new, target).is_err() {
        let _ = std::fs::rename(previous, target);
        return Err(UpdateError::Swap);
    }
    Ok(())
}

/// Writes at most `cap` bytes, then fails.
struct Capped {
    buf: Vec<u8>,
    cap: usize,
}

impl Write for Capped {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + data.len() > self.cap {
            return Err(std::io::Error::other("decompressed archive is too large"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Room for the other entries of the archive next to the program.
const ARCHIVE_SLACK: u64 = 16 << 20;

/// True when the entry path is `name` or `<dir>/name`, with normal components
/// only. `None` when the file name is not `name`; `Some(false)` when it is but
/// the path is not acceptable.
fn match_entry(path: &Path, name: &str) -> Option<bool> {
    if path.file_name()? != name {
        return None;
    }
    let mut depth = 0_usize;
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
            _ => return Some(false),
        }
    }
    Some(depth <= 2)
}

fn write_program<R: Read>(from: R, dest: &Path, size: u64) -> Result<(), UpdateError> {
    if size > MAX_BINARY_BYTES {
        return Err(UpdateError::ArchiveLayout);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o755);
    }
    let mut out = opts.open(dest).map_err(|_| UpdateError::Io)?;
    let copied = std::io::copy(&mut from.take(MAX_BINARY_BYTES + 1), &mut out)
        .map_err(|_| UpdateError::ArchiveLayout)?;
    if copied > MAX_BINARY_BYTES {
        return Err(UpdateError::ArchiveLayout);
    }
    out.flush().map_err(|_| UpdateError::Io)?;
    drop(out);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| UpdateError::Io)?;
    }
    Ok(())
}

/// Extracts the single `sqler` (`sqler.exe` in a zip) entry to `dest`.
fn extract_program(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let is_zip = archive.extension().is_some_and(|e| e == "zip");
    if is_zip {
        extract_zip(archive, dest)
    } else {
        extract_tar_xz(archive, dest)
    }
}

fn extract_tar_xz(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(|_| UpdateError::Io)?;
    let mut tar_bytes = Capped {
        buf: Vec::new(),
        cap: usize::try_from(MAX_BINARY_BYTES + ARCHIVE_SLACK).unwrap_or(usize::MAX),
    };
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(file), &mut tar_bytes)
        .map_err(|_| UpdateError::ArchiveLayout)?;
    let mut tar = tar::Archive::new(std::io::Cursor::new(tar_bytes.buf));
    let mut found = false;
    for entry in tar.entries().map_err(|_| UpdateError::ArchiveLayout)? {
        let entry = entry.map_err(|_| UpdateError::ArchiveLayout)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        let path = entry.path().map_err(|_| UpdateError::ArchiveLayout)?;
        let Some(acceptable) = match_entry(&path, "sqler") else {
            continue;
        };
        if !acceptable || !kind.is_file() || found {
            return Err(UpdateError::ArchiveLayout);
        }
        found = true;
        let size = entry
            .header()
            .size()
            .map_err(|_| UpdateError::ArchiveLayout)?;
        write_program(entry, dest, size)?;
    }
    if found {
        Ok(())
    } else {
        Err(UpdateError::ArchiveLayout)
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(|_| UpdateError::Io)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|_| UpdateError::ArchiveLayout)?;
    let mut found = false;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|_| UpdateError::ArchiveLayout)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|_| UpdateError::ArchiveLayout)?
            .into_owned();
        let Some(acceptable) = match_entry(Path::new(&name), "sqler.exe") else {
            continue;
        };
        if !acceptable || !entry.is_file() || entry.is_symlink() || found {
            return Err(UpdateError::ArchiveLayout);
        }
        found = true;
        let size = entry.size();
        write_program(entry, dest, size)?;
    }
    if found {
        Ok(())
    } else {
        Err(UpdateError::ArchiveLayout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn plan_table() {
        let pk = vec![p("/usr/bin"), p("/nix/store"), p("/opt/homebrew")];
        let plan = |s: &str| plan_install(Path::new(s), &pk);

        let a = plan("/Applications/SQLer.app/Contents/MacOS/sqler");
        assert_eq!(a.kind, ArtifactKind::MacBundle);
        assert_eq!(a.target, p("/Applications/SQLer.app"));
        assert_eq!(a.manual, None);

        let nix = plan("/nix/store/x/SQLer.app/Contents/MacOS/sqler");
        assert_eq!(nix.manual, Some(ManualReason::PackageManager));
        assert_eq!(nix.kind, ArtifactKind::MacBundle);

        let dev = plan("/w/sqler/target/release/bundle/osx/SQLer.app/Contents/MacOS/sqler");
        assert_eq!(dev.manual, Some(ManualReason::Development));
        assert_eq!(
            plan("/w/sqler/target/debug/sqler-app").manual,
            Some(ManualReason::Development)
        );

        let usr = plan("/usr/bin/sqler");
        assert_eq!(usr.manual, Some(ManualReason::PackageManager));
        assert_eq!(usr.kind, ArtifactKind::Archive);

        let home = plan("/home/me/sqler/sqler-app");
        assert_eq!((home.manual, home.kind), (None, ArtifactKind::Archive));
        assert_eq!(home.target, p("/home/me/sqler/sqler-app"));

        // Not a bundle: wrong shape.
        let odd = plan("/home/me/Foo.app/Contents/Resources/sqler");
        assert_eq!(odd.kind, ArtifactKind::Archive);
    }

    #[test]
    fn program_files_are_package_dirs() {
        let pk = vec![p("C:\\Program Files")];
        let plan = plan_install(Path::new("C:\\Program Files\\SQLer\\sqler-app.exe"), &pk);
        // On unix the backslash path is one component, so the rule only holds
        // where the separator is understood.
        if cfg!(windows) {
            assert_eq!(plan.manual, Some(ManualReason::PackageManager));
        }
    }

    #[test]
    fn manual_notes_are_fixed() {
        assert!(ManualReason::NotWritable.note().contains("by hand"));
        assert!(
            ManualReason::PackageManager
                .note()
                .contains("package manager")
        );
        assert!(ManualReason::Development.note().contains("development"));
    }

    #[test]
    fn entry_matching() {
        assert_eq!(match_entry(Path::new("sqler"), "sqler"), Some(true));
        assert_eq!(match_entry(Path::new("./sqler"), "sqler"), Some(true));
        assert_eq!(match_entry(Path::new("a/sqler"), "sqler"), Some(true));
        assert_eq!(match_entry(Path::new("a/b/sqler"), "sqler"), Some(false));
        assert_eq!(match_entry(Path::new("../sqler"), "sqler"), Some(false));
        assert_eq!(match_entry(Path::new("a/README"), "sqler"), None);
    }

    fn tmp(case: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("sqler-update-swap-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn swap_a_file_and_restore_on_failure() {
        let d = tmp("file");
        let (t, n, pv) = (d.join("t"), d.join("n"), d.join("prev"));
        std::fs::write(&t, "old").unwrap();
        std::fs::write(&n, "new").unwrap();
        swap(&t, &n, &pv).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&pv).unwrap(), "old");

        let d2 = tmp("file-missing");
        let (t, pv) = (d2.join("t"), d2.join("prev"));
        std::fs::write(&t, "old").unwrap();
        assert_eq!(swap(&t, &d2.join("absent"), &pv), Err(UpdateError::Swap));
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "old");
        assert!(!pv.exists());
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(d2);
    }

    #[test]
    fn swap_a_dir_and_restore_on_failure() {
        let d = tmp("dir");
        let (t, n, pv) = (d.join("T.app"), d.join("N.app"), d.join("prev.app"));
        std::fs::create_dir_all(t.join("x")).unwrap();
        std::fs::create_dir_all(&n).unwrap();
        swap(&t, &n, &pv).unwrap();
        assert!(!t.join("x").exists() && pv.join("x").exists());

        let d2 = tmp("dir-missing");
        let (t, pv) = (d2.join("T.app"), d2.join("prev.app"));
        std::fs::create_dir_all(t.join("x")).unwrap();
        assert_eq!(
            swap(&t, &d2.join("absent.app"), &pv),
            Err(UpdateError::Swap)
        );
        assert!(t.join("x").exists() && !pv.exists());
        let _ = std::fs::remove_dir_all(d);
        let _ = std::fs::remove_dir_all(d2);
    }
}
