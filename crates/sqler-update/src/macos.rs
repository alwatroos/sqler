// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! macOS helpers for the bundle install (ADR-0078): `hdiutil`, `ditto` and
//! `codesign`, always by absolute path, each killed after 120 s.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::UpdateError;

const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

fn run(tool: &str, args: &[&OsStr]) -> Result<(), UpdateError> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| UpdateError::Io)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) | Err(_) => return Err(UpdateError::Io),
            Ok(None) if start.elapsed() > TOOL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(UpdateError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Mounts `dmg` read-only beside `dest`, copies `SQLer.app` to `dest` with
/// `ditto` (signature, stapled ticket and extended attributes are kept), and
/// detaches the image again.
///
/// # Errors
///
/// [`UpdateError::Io`] or [`UpdateError::Timeout`] when a tool fails;
/// [`UpdateError::ArchiveLayout`] when the image has no `SQLer.app` folder.
pub fn hdiutil_extract_app(dmg: &Path, dest: &Path) -> Result<(), UpdateError> {
    let stage = dest.parent().ok_or(UpdateError::Io)?;
    let mnt = stage.join("mnt");
    std::fs::create_dir(&mnt).map_err(|_| UpdateError::Io)?;
    let attached = run(
        "/usr/bin/hdiutil",
        &[
            OsStr::new("attach"),
            OsStr::new("-nobrowse"),
            OsStr::new("-readonly"),
            OsStr::new("-noautoopen"),
            OsStr::new("-mountpoint"),
            mnt.as_os_str(),
            dmg.as_os_str(),
        ],
    );
    let result = attached.and_then(|()| {
        let app = mnt.join("SQLer.app");
        if !std::fs::symlink_metadata(&app).is_ok_and(|m| m.is_dir()) {
            return Err(UpdateError::ArchiveLayout);
        }
        run("/usr/bin/ditto", &[app.as_os_str(), dest.as_os_str()])
    });
    let _ = run(
        "/usr/bin/hdiutil",
        &[OsStr::new("detach"), OsStr::new("-force"), mnt.as_os_str()],
    );
    let _ = std::fs::remove_dir(&mnt);
    result
}

/// True when `codesign --verify --deep --strict` accepts `app`.
#[must_use]
pub fn codesign_check(app: &Path) -> bool {
    run(
        "/usr/bin/codesign",
        &[
            OsStr::new("--verify"),
            OsStr::new("--deep"),
            OsStr::new("--strict"),
            app.as_os_str(),
        ],
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a real macOS with `hdiutil`; run with `--ignored`.
    #[test]
    #[ignore = "mounts a real disk image"]
    #[allow(clippy::unwrap_used)]
    fn extracts_sqler_app_from_a_real_dmg() {
        let root = std::env::temp_dir().join(format!("sqler-update-dmg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(src.join("SQLer.app/Contents/MacOS")).unwrap();
        std::fs::write(src.join("SQLer.app/Contents/MacOS/sqler"), "x").unwrap();
        let dmg = root.join("t.dmg");
        run(
            "/usr/bin/hdiutil",
            &[
                OsStr::new("create"),
                OsStr::new("-srcfolder"),
                src.as_os_str(),
                OsStr::new("-format"),
                OsStr::new("UDZO"),
                dmg.as_os_str(),
            ],
        )
        .unwrap();
        let stage = root.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        let dest = stage.join("SQLer.app");
        hdiutil_extract_app(&dmg, &dest).unwrap();
        assert!(dest.join("Contents/MacOS/sqler").is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
