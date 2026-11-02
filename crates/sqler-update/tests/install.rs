// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Installing a verified artifact in temp directories. The probe reads a fake
//! program instead of running it; no real install, no network.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::unnecessary_wraps)]

mod support;

use std::path::{Path, PathBuf};

use semver::Version;
use sqler_update::{
    ArtifactKind, InstallHooks, InstallOutcome, ManualReason, STAGE_DIR_NAME, UpdateError,
    VerifiedArtifact, clean_leftovers, install,
};
use support::{
    Entry, TempDir, copy_tree, fake_app_tree, fake_probe, fake_program, sha256_hex, tar_xz_with,
    zip_with,
};

fn v() -> Version {
    Version::new(2, 0, 0)
}

fn artifact(dir: &Path, file: &str, kind: ArtifactKind, bytes: &[u8]) -> VerifiedArtifact {
    let path = dir.join(file);
    std::fs::write(&path, bytes).unwrap();
    VerifiedArtifact {
        version: v(),
        kind,
        path,
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
    }
}

fn no_extract(_: &Path, _: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::Io)
}

fn hooks<'a>(
    extract: &'a (dyn Fn(&Path, &Path) -> Result<(), UpdateError> + Sync),
    check: &'a (dyn Fn(&Path) -> bool + Sync),
) -> InstallHooks<'a> {
    InstallHooks {
        version_probe: &fake_probe,
        extract_app: extract,
        bundle_check: check,
    }
}

fn run_archive(
    case: &str,
    archive: &[u8],
    file: &str,
) -> (Result<InstallOutcome, UpdateError>, TempDir, PathBuf) {
    let dir = TempDir::new(case);
    let exe = dir.path().join("sqler-app");
    std::fs::write(&exe, fake_program("1.0.0")).unwrap();
    let art = artifact(dir.path(), file, ArtifactKind::Archive, archive);
    let result = install(&art, &exe, &[], &hooks(&no_extract, &|_| true));
    (result, dir, exe)
}

fn old_exe_untouched(exe: &Path) {
    assert_eq!(std::fs::read(exe).unwrap(), fake_program("1.0.0"));
}

fn stage(dir: &TempDir) -> PathBuf {
    dir.path().join(STAGE_DIR_NAME)
}

#[test]
fn a_tar_xz_archive_replaces_the_program() {
    let new = fake_program("2.0.0");
    let archive = tar_xz_with(&[
        Entry::Dir("sqler-app-x/"),
        Entry::File("sqler-app-x/README.md", b"readme"),
        Entry::File("sqler-app-x/sqler", &new),
    ]);
    let (r, dir, exe) = run_archive("tar-ok", &archive, "a.tar.xz");
    assert_eq!(
        r.unwrap(),
        InstallOutcome::Installed {
            launch: exe.clone()
        }
    );
    assert_eq!(std::fs::read(&exe).unwrap(), new);
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert_eq!(
        std::fs::read(stage(&dir).join("previous")).unwrap(),
        fake_program("1.0.0")
    );
    assert!(!stage(&dir).join("README.md").exists());
    clean_leftovers(&exe);
    assert!(!stage(&dir).exists());
}

#[test]
fn a_zip_archive_replaces_the_program() {
    let new = fake_program("2.0.0");
    let archive = zip_with(&[("readme.txt", b"x"), ("sqler-app-x/sqler.exe", &new)]);
    let (r, _dir, exe) = run_archive("zip-ok", &archive, "a.zip");
    assert!(matches!(r.unwrap(), InstallOutcome::Installed { .. }));
    assert_eq!(std::fs::read(&exe).unwrap(), new);
}

#[test]
fn bad_archive_layouts_are_refused_and_change_nothing() {
    let p = fake_program("2.0.0");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("none", tar_xz_with(&[Entry::File("README", b"x")])),
        ("empty", tar_xz_with(&[])),
        (
            "two",
            tar_xz_with(&[Entry::File("a/sqler", &p), Entry::File("b/sqler", &p)]),
        ),
        (
            "symlink",
            tar_xz_with(&[Entry::Symlink("sqler", "/bin/sh")]),
        ),
        ("dotdot", tar_xz_with(&[Entry::File("../sqler", &p)])),
        ("deep", tar_xz_with(&[Entry::File("a/b/sqler", &p)])),
        ("absolute", tar_xz_with(&[Entry::File("/sqler", &p)])),
    ];
    for (case, archive) in cases {
        let (r, dir, exe) = run_archive(&format!("layout-{case}"), &archive, "a.tar.xz");
        assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout, "{case}");
        old_exe_untouched(&exe);
        assert!(!stage(&dir).exists(), "{case}: stage not removed");
    }
    let (r, _d, exe) = run_archive("zip-none", &zip_with(&[("README", b"x")]), "a.zip");
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);
    old_exe_untouched(&exe);
    let (r, _d, _e) = run_archive(
        "zip-two",
        &zip_with(&[("a/sqler.exe", &p), ("b/sqler.exe", &p)]),
        "a.zip",
    );
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);
    let (r, _d, _e) = run_archive("garbage", b"not an archive", "a.tar.xz");
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);
}

#[test]
fn a_kind_mismatch_is_refused() {
    let dir = TempDir::new("kind");
    let exe = dir.path().join("sqler-app");
    std::fs::write(&exe, fake_program("1.0.0")).unwrap();
    let art = artifact(dir.path(), "a.dmg", ArtifactKind::MacBundle, b"dmg");
    let r = install(&art, &exe, &[], &hooks(&no_extract, &|_| true));
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);
    old_exe_untouched(&exe);
}

#[test]
fn a_file_changed_after_verification_is_refused() {
    let archive = tar_xz_with(&[Entry::File("sqler", &fake_program("2.0.0"))]);
    let dir = TempDir::new("changed");
    let exe = dir.path().join("sqler-app");
    std::fs::write(&exe, fake_program("1.0.0")).unwrap();
    let art = artifact(dir.path(), "a.tar.xz", ArtifactKind::Archive, &archive);
    let mut tampered = archive.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    std::fs::write(&art.path, tampered).unwrap();
    let r = install(&art, &exe, &[], &hooks(&no_extract, &|_| true));
    assert_eq!(r.unwrap_err(), UpdateError::ChangedAfterVerification);
    old_exe_untouched(&exe);
    assert!(!stage(&dir).exists());
}

#[test]
fn a_program_with_another_version_is_refused() {
    let archive = tar_xz_with(&[Entry::File("sqler", &fake_program("9.9.9"))]);
    let (r, dir, exe) = run_archive("version", &archive, "a.tar.xz");
    assert_eq!(r.unwrap_err(), UpdateError::VersionMismatch);
    old_exe_untouched(&exe);
    assert!(!stage(&dir).exists());
}

#[test]
fn a_read_only_folder_gives_a_manual_outcome() {
    use std::os::unix::fs::PermissionsExt as _;
    let archive = tar_xz_with(&[Entry::File("sqler", &fake_program("2.0.0"))]);
    let dir = TempDir::new("ro");
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let exe = bin.join("sqler-app");
    std::fs::write(&exe, fake_program("1.0.0")).unwrap();
    let art = artifact(dir.path(), "a.tar.xz", ArtifactKind::Archive, &archive);
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::File::create(bin.join("probe")).is_ok();
    let r = install(&art, &exe, &[], &hooks(&no_extract, &|_| true));
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // running as root: the folder is writable anyway
    }
    assert_eq!(
        r.unwrap(),
        InstallOutcome::Manual(ManualReason::NotWritable)
    );
    old_exe_untouched(&exe);
}

#[test]
fn package_manager_installs_are_manual() {
    let archive = tar_xz_with(&[Entry::File("sqler", &fake_program("2.0.0"))]);
    let dir = TempDir::new("pkg");
    let exe = dir.path().join("sqler-app");
    std::fs::write(&exe, fake_program("1.0.0")).unwrap();
    let art = artifact(dir.path(), "a.tar.xz", ArtifactKind::Archive, &archive);
    let r = install(
        &art,
        &exe,
        &[dir.path().to_path_buf()],
        &hooks(&no_extract, &|_| true),
    );
    assert_eq!(
        r.unwrap(),
        InstallOutcome::Manual(ManualReason::PackageManager)
    );
    old_exe_untouched(&exe);
    assert!(!stage(&dir).exists());
}

struct BundleCase {
    dir: TempDir,
    app: PathBuf,
    exe: PathBuf,
    art: VerifiedArtifact,
}

fn bundle_case(case: &str) -> BundleCase {
    let dir = TempDir::new(case);
    let app = dir.path().join("SQLer.app");
    fake_app_tree(&app, "1.0.0");
    let exe = app.join("Contents/MacOS/sqler");
    let art = artifact(dir.path(), "a.dmg", ArtifactKind::MacBundle, b"dmg bytes");
    BundleCase { dir, app, exe, art }
}

fn copy_new_tree(_dmg: &Path, dest: &Path) -> Result<(), UpdateError> {
    let src = dest.parent().unwrap().join("src.app");
    fake_app_tree(&src, "2.0.0");
    copy_tree(&src, dest);
    std::fs::remove_dir_all(src).unwrap();
    Ok(())
}

#[test]
fn a_bundle_is_swapped_as_a_whole() {
    let c = bundle_case("bundle-ok");
    let r = install(&c.art, &c.exe, &[], &hooks(&copy_new_tree, &|_| true));
    assert_eq!(
        r.unwrap(),
        InstallOutcome::Installed {
            launch: c.app.clone()
        }
    );
    assert_eq!(
        std::fs::read(c.app.join("Contents/MacOS/sqler")).unwrap(),
        fake_program("2.0.0")
    );
    assert_eq!(
        std::fs::read(stage(&c.dir).join("previous.app/Contents/MacOS/sqler")).unwrap(),
        fake_program("1.0.0")
    );
    clean_leftovers(&c.exe);
    assert!(!stage(&c.dir).exists());
}

#[test]
fn a_failed_signature_check_changes_nothing() {
    let c = bundle_case("bundle-sig");
    let r = install(&c.art, &c.exe, &[], &hooks(&copy_new_tree, &|_| false));
    assert_eq!(r.unwrap_err(), UpdateError::BundleSignature);
    assert_eq!(std::fs::read(&c.exe).unwrap(), fake_program("1.0.0"));
    assert!(!stage(&c.dir).exists());
}

#[test]
fn bundle_layout_problems_are_refused() {
    let c = bundle_case("bundle-extract");
    let r = install(&c.art, &c.exe, &[], &hooks(&no_extract, &|_| true));
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);

    let missing = |_: &Path, dest: &Path| {
        std::fs::create_dir_all(dest.join("Contents")).unwrap();
        std::fs::write(dest.join("Contents/Info.plist"), "<plist/>").unwrap();
        Ok(())
    };
    let r = install(&c.art, &c.exe, &[], &hooks(&missing, &|_| true));
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);

    let linked = |_: &Path, dest: &Path| {
        fake_app_tree(dest, "2.0.0");
        let bin = dest.join("Contents/MacOS/sqler");
        std::fs::remove_file(&bin).unwrap();
        std::os::unix::fs::symlink("/bin/sh", &bin).unwrap();
        Ok(())
    };
    let r = install(&c.art, &c.exe, &[], &hooks(&linked, &|_| true));
    assert_eq!(r.unwrap_err(), UpdateError::ArchiveLayout);
    assert_eq!(std::fs::read(&c.exe).unwrap(), fake_program("1.0.0"));
    assert!(!stage(&c.dir).exists());
}

#[test]
fn clean_leftovers_never_fails() {
    let c = bundle_case("clean-bundle");
    // The stage of a bundle sits beside the `.app`.
    std::fs::create_dir_all(stage(&c.dir).join("previous.app")).unwrap();
    clean_leftovers(&c.exe);
    assert!(!stage(&c.dir).exists());
    assert!(c.app.exists());
    clean_leftovers(&c.exe);
    clean_leftovers(Path::new("/definitely/not/here/sqler"));
}
