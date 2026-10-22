// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Build, check, clean and staging.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sqler_plugin_api::{ComponentDigest, Manifest};

use super::component::is_component;
use super::table::{PLUGINS, PluginRow};

const TARGET: &str = "wasm32-wasip2";

/// The repository root (the parent of the `xtask` crate).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn env_or(var: &str, default: &str) -> OsString {
    std::env::var_os(var).unwrap_or_else(|| OsString::from(default))
}

fn plugins_out(root: &Path) -> PathBuf {
    root.join("target").join("plugins")
}

fn cargo_target_dir(root: &Path) -> PathBuf {
    plugins_out(root).join("cargo")
}

fn guest_manifest(root: &Path) -> PathBuf {
    root.join("plugins").join("Cargo.toml")
}

fn profile_name(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}

/// Fail with the fix when the `wasm32-wasip2` target is not installed.
fn check_target() -> Result<(), String> {
    check_target_with(&env_or("RUSTC", "rustc"))
}

fn check_target_with(rustc: &OsString) -> Result<(), String> {
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|e| format!("cannot run rustc: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`rustc --print sysroot` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let installed = Path::new(&sysroot)
        .join("lib")
        .join("rustlib")
        .join(TARGET)
        .is_dir();
    if installed {
        Ok(())
    } else {
        Err(format!(
            "the {TARGET} target is not installed; run `rustup target add {TARGET}`"
        ))
    }
}

fn run_cargo(args: &[OsString]) -> Result<(), String> {
    let status = Command::new(env_or("CARGO", "cargo"))
        .args(args)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo {} failed ({status})",
            args.first()
                .map_or(String::new(), |a| a.to_string_lossy().into_owned())
        ))
    }
}

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

/// The artefact file name of a guest package: `-` becomes `_`.
fn artefact_name(package: &str) -> String {
    format!("{}.wasm", package.replace('-', "_"))
}

/// A path relative to `root`, `/`-separated.
fn display_rel(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Validate a row's manifest bytes against the row.
fn validate_manifest(row: &PluginRow, bytes: &[u8]) -> Result<Manifest, String> {
    let manifest = Manifest::parse_bytes(bytes)
        .map_err(|e| format!("{}: invalid sqler.toml: {e}", row.name))?;
    match (&manifest.entry, row.package) {
        (None, Some(_)) => Err(format!(
            "{}: sqler.toml has no [entry] but the table row has a package",
            row.name
        )),
        (Some(_), None) => Err(format!(
            "{}: declarative-only plugin must not have an [entry]",
            row.name
        )),
        _ => Ok(manifest),
    }
}

/// Stage `<out>/<id>/sqler.toml` and the component, recreating the directory. Returns the component path.
fn stage(
    out: &Path,
    manifest: &Manifest,
    manifest_bytes: &[u8],
    wasm: Option<&[u8]>,
) -> Result<Option<PathBuf>, String> {
    let dir = out.join(manifest.id.as_str());
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::write(dir.join("sqler.toml"), manifest_bytes)
        .map_err(|e| format!("cannot write sqler.toml: {e}"))?;
    let (Some(entry), Some(wasm)) = (&manifest.entry, wasm) else {
        return Ok(None);
    };
    let dest = entry.component.split('/').fold(dir, |p, c| p.join(c));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&dest, wasm).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(Some(dest))
}

/// `plugins build`.
pub fn build(
    root: &Path,
    release: bool,
    names: &[String],
    w: &mut dyn Write,
) -> Result<(), String> {
    let rows: Vec<&PluginRow> = if names.is_empty() {
        PLUGINS.iter().collect()
    } else {
        names
            .iter()
            .filter_map(|n| PLUGINS.iter().find(|r| r.name == n))
            .collect()
    };
    let profile = profile_name(release);
    let packages: Vec<&str> = rows.iter().filter_map(|r| r.package).collect();
    if !packages.is_empty() {
        check_target()?;
        let mut args = os(&["build", "--locked", "--manifest-path"]);
        args.push(guest_manifest(root).into_os_string());
        args.extend(os(&["--target", TARGET, "--target-dir"]));
        args.push(cargo_target_dir(root).into_os_string());
        if release {
            args.push(OsString::from("--release"));
        }
        for package in &packages {
            args.push(OsString::from("-p"));
            args.push(OsString::from(package));
        }
        run_cargo(&args)?;
    }
    let out = plugins_out(root).join(profile);
    for row in &rows {
        let dir = row
            .dir
            .split('/')
            .fold(root.to_path_buf(), |p, c| p.join(c));
        let manifest_path = dir.join("sqler.toml");
        let manifest_bytes = std::fs::read(&manifest_path)
            .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
        let manifest = validate_manifest(row, &manifest_bytes)?;
        let wasm = match row.package {
            Some(package) => {
                let artefact = cargo_target_dir(root)
                    .join(TARGET)
                    .join(profile)
                    .join(artefact_name(package));
                let bytes = std::fs::read(&artefact).map_err(|e| {
                    format!("{}: artefact {} missing: {e}", row.name, artefact.display())
                })?;
                if !is_component(&bytes) {
                    return Err(format!(
                        "{}: {} is not a component",
                        row.name,
                        artefact.display()
                    ));
                }
                Some(bytes)
            }
            None => None,
        };
        let staged = stage(&out, &manifest, &manifest_bytes, wasm.as_deref())?;
        if let (Some(path), Some(bytes)) = (staged, wasm.as_deref()) {
            writeln!(
                w,
                "plugin={} id={} profile={profile} wasm={} bytes={} sha256={}",
                row.name,
                manifest.id.as_str(),
                display_rel(root, &path),
                bytes.len(),
                ComponentDigest::of(bytes)
            )
            .map_err(|e| write_error(&e))?;
        } else {
            writeln!(
                w,
                "plugin={} id={} profile={profile} wasm=- bytes=0 sha256=-",
                row.name,
                manifest.id.as_str()
            )
            .map_err(|e| write_error(&e))?;
        }
    }
    writeln!(w, "plugins_built={}", rows.len()).map_err(|e| write_error(&e))?;
    Ok(())
}

/// A failed write to standard output (for example a closed pipe) is an ordinary failure, not a panic.
pub fn write_error(err: &std::io::Error) -> String {
    format!("cannot write to standard output: {err}")
}

/// `plugins check`: fmt then clippy over the guest workspace.
pub fn check(root: &Path) -> Result<(), String> {
    check_target()?;
    let mut fmt = os(&["fmt", "--all", "--check", "--manifest-path"]);
    fmt.push(guest_manifest(root).into_os_string());
    run_cargo(&fmt)?;
    let mut clippy = os(&["clippy", "--locked", "--manifest-path"]);
    clippy.push(guest_manifest(root).into_os_string());
    clippy.extend(os(&[
        "--workspace",
        "--all-targets",
        "--target",
        TARGET,
        "--target-dir",
    ]));
    clippy.push(cargo_target_dir(root).into_os_string());
    clippy.extend(os(&["--", "-D", "warnings"]));
    run_cargo(&clippy)
}

/// `plugins clean`: remove `target/plugins`.
pub fn clean(root: &Path) -> Result<(), String> {
    let dir = plugins_out(root);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove {}: {e}", dir.display())),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsString;
    use std::path::Path;

    #[cfg(unix)]
    use super::check_target_with;
    use super::{artefact_name, stage, validate_manifest};
    use crate::plugins::table::PluginRow;
    use sqler_plugin_api::Manifest;

    const MANIFEST: &str = "[plugin]\nid = \"smoke\"\nname = \"Smoke\"\nversion = \"0.1.0\"\napi = \"0.1.0\"\ncapabilities = []\n[entry]\ncomponent = \"bin/plugin.wasm\"\n";

    const ROW: PluginRow = PluginRow {
        name: "smoke",
        dir: "x",
        package: Some("p"),
    };

    #[cfg(unix)]
    #[test]
    fn failing_rustc_is_reported_not_blamed_on_the_target() {
        let err = check_target_with(&OsString::from("false")).unwrap_err();
        assert!(err.contains("`rustc --print sysroot` failed"), "{err}");
        assert!(!err.contains("rustup target add"), "{err}");
    }

    #[test]
    fn artefact_name_replaces_dashes() {
        assert_eq!(
            artefact_name("sqler-fixture-smoke"),
            "sqler_fixture_smoke.wasm"
        );
    }

    #[test]
    fn invalid_manifest_is_an_error() {
        assert!(validate_manifest(&ROW, b"not a manifest [").is_err());
        assert!(validate_manifest(&ROW, &[0xff, 0xfe]).is_err());
    }

    #[test]
    fn entry_must_match_package() {
        assert!(validate_manifest(&ROW, MANIFEST.as_bytes()).is_ok());
        let declarative = PluginRow {
            package: None,
            ..ROW
        };
        assert!(validate_manifest(&declarative, MANIFEST.as_bytes()).is_err());
    }

    #[test]
    fn stages_nested_component_path() {
        let manifest = Manifest::parse(MANIFEST).map_err(|e| e.to_string());
        let Ok(manifest) = manifest else {
            panic!("manifest must parse")
        };
        let out = std::env::temp_dir().join(format!("sqler-xtask-stage-{}", std::process::id()));
        let staged = stage(&out, &manifest, MANIFEST.as_bytes(), Some(b"wasm"));
        let expected = out.join("smoke").join("bin").join("plugin.wasm");
        assert_eq!(staged, Ok(Some(expected.clone())));
        assert!(expected.is_file());
        assert!(out.join("smoke").join("sqler.toml").is_file());
        // A second stage recreates the directory: stale files are gone.
        let stale = out.join("smoke").join("stale.txt");
        assert!(std::fs::write(&stale, b"x").is_ok());
        assert!(stage(&out, &manifest, MANIFEST.as_bytes(), Some(b"wasm")).is_ok());
        assert!(!stale.exists());
        let _ = std::fs::remove_dir_all(Path::new(&out));
    }
}
