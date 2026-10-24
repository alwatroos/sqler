// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Dist plan parsing, archive hashing and `Manifest` construction.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use ring::digest::{Context, SHA256};
use serde::Deserialize;
use sqler_update::{Download, Manifest, parse_manifest};

/// The release targets, in the order of `targets` in `dist-workspace.toml`.
pub const RELEASE_TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];

const REPO: &str = "alwatroos/sqler";
const APP_NAME: &str = "sqler-app";
const ARCHIVE_KIND: &str = "executable-zip";
const CHUNK: usize = 64 * 1024;

/// Options of [`build`].
#[derive(Debug)]
pub struct BuildOptions {
    /// `--tag`, `v<version>`.
    pub tag: Option<String>,
    /// `YYYY-MM-DDTHH:MM:SSZ`.
    pub published: String,
    /// Skip archives missing on disk.
    pub partial: bool,
}

#[derive(Deserialize)]
struct Plan {
    #[serde(default)]
    announcement_tag: Option<String>,
    releases: Vec<Release>,
    artifacts: BTreeMap<String, PlanArtifact>,
}

#[derive(Deserialize)]
struct Release {
    app_name: String,
    app_version: String,
    artifacts: Vec<String>,
}

#[derive(Deserialize)]
struct PlanArtifact {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    target_triples: Vec<String>,
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a regular file (no symlink) and its length.
fn hash_file(path: &Path, name: &str) -> Result<(String, u64), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| format!("cannot stat `{name}`"))?;
    if !meta.is_file() {
        return Err(format!("`{name}` is not a regular file"));
    }
    let mut file = File::open(path).map_err(|_| format!("cannot open `{name}`"))?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0_u8; CHUNK];
    let mut size = 0_u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| format!("cannot read `{name}`"))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex(ctx.finish().as_ref()), size))
}

/// Compare the sidecar `<name>.sha256` with the computed hash.
fn check_sidecar(dir: &Path, name: &str, sha: &str, required: bool) -> Result<(), String> {
    let side = format!("{name}.sha256");
    let path = dir.join(&side);
    if std::fs::symlink_metadata(&path).is_err() {
        return if required {
            Err(format!("missing checksum file `{side}`"))
        } else {
            Ok(())
        };
    }
    let mut text = String::new();
    File::open(&path)
        .and_then(|f| f.take(4096).read_to_string(&mut text))
        .map_err(|_| format!("cannot read `{side}`"))?;
    let first = text.split_whitespace().next().unwrap_or_default();
    if first.eq_ignore_ascii_case(sha) {
        Ok(())
    } else {
        Err(format!("checksum file `{side}` does not match `{name}`"))
    }
}

/// Build the manifest from a dist plan and the final archives in `artifacts`.
///
/// # Errors
///
/// A fixed message for every plan, archive or checksum problem.
pub fn build(plan_json: &[u8], artifacts: &Path, opts: &BuildOptions) -> Result<Manifest, String> {
    let plan: Plan =
        serde_json::from_slice(plan_json).map_err(|_| "dist plan is not valid JSON".to_owned())?;
    let mut apps = plan.releases.iter().filter(|r| r.app_name == APP_NAME);
    let (Some(release), None) = (apps.next(), apps.next()) else {
        return Err(format!(
            "dist plan must hold exactly one release of `{APP_NAME}`"
        ));
    };
    let version = release.app_version.clone();
    let expected_tag = format!("v{version}");
    if let Some(tag) = &opts.tag
        && *tag != expected_tag
    {
        return Err(format!("tag must be `{expected_tag}`"));
    }
    if let Some(announced) = &plan.announcement_tag
        && *announced != expected_tag
    {
        return Err(format!(
            "dist plan announces `{announced}`, expected `{expected_tag}`"
        ));
    }
    let tag = expected_tag;

    let mut platforms = BTreeMap::new();
    for name in &release.artifacts {
        let Some(item) = plan.artifacts.get(name) else {
            continue;
        };
        if item.kind != ARCHIVE_KIND {
            continue;
        }
        let [triple] = item.target_triples.as_slice() else {
            return Err(format!("`{name}` must have exactly one target triple"));
        };
        let ext = if triple.contains("-pc-windows-") {
            "zip"
        } else {
            "tar.xz"
        };
        if *name != format!("{APP_NAME}-{triple}.{ext}") {
            return Err(format!("unexpected archive name `{name}`"));
        }
        if name.to_ascii_lowercase().contains("duckdb") {
            return Err(format!("archive `{name}` must not name DuckDB"));
        }
        if !RELEASE_TARGETS.contains(&triple.as_str()) {
            return Err(format!("`{triple}` is not a release target"));
        }
        if platforms.contains_key(triple) {
            return Err(format!("two archives for `{triple}`"));
        }
        let path = artifacts.join(name);
        if opts.partial && std::fs::symlink_metadata(&path).is_err() {
            eprintln!("skipping `{name}`: not present");
            continue;
        }
        let (sha256, size) = hash_file(&path, name)?;
        check_sidecar(artifacts, name, &sha256, !opts.partial)?;
        platforms.insert(
            triple.clone(),
            Download {
                url: format!("https://github.com/{REPO}/releases/download/{tag}/{name}"),
                sha256,
                size,
            },
        );
    }
    if opts.partial {
        if platforms.is_empty() {
            return Err("no archive found".to_owned());
        }
    } else {
        let have: BTreeSet<&str> = platforms.keys().map(String::as_str).collect();
        let want: BTreeSet<&str> = RELEASE_TARGETS.into_iter().collect();
        if let Some(missing) = want.difference(&have).next() {
            return Err(format!("no archive for `{missing}`"));
        }
    }
    let manifest = Manifest {
        schema: 1,
        product: "sqler".to_owned(),
        version,
        published: opts.published.clone(),
        notes_url: format!("https://github.com/{REPO}/releases/tag/{tag}"),
        platforms,
    };
    let payload = serde_json::to_vec(&manifest).map_err(|_| "cannot encode manifest".to_owned())?;
    parse_manifest(&payload).map_err(|e| format!("built manifest is invalid: {e}"))?;
    Ok(manifest)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod tests {
    use std::path::PathBuf;

    use super::{BuildOptions, RELEASE_TARGETS, build, hex};

    pub const PLAN: &str = include_str!("testdata/plan-dist-manifest.json");

    pub struct Dir(pub PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A temp dir with all five archives and sidecars; content is the file name.
    pub fn archives(case: &str) -> Dir {
        let dir = std::env::temp_dir().join(format!("xtask-release-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for t in RELEASE_TARGETS {
            let ext = if t.contains("-pc-windows-") {
                "zip"
            } else {
                "tar.xz"
            };
            let name = format!("sqler-app-{t}.{ext}");
            std::fs::write(dir.join(&name), name.as_bytes()).unwrap();
            let sha = hex(ring::digest::digest(&ring::digest::SHA256, name.as_bytes()).as_ref());
            std::fs::write(
                dir.join(format!("{name}.sha256")),
                format!("{sha}  {name}\n"),
            )
            .unwrap();
        }
        Dir(dir)
    }

    pub fn opts(tag: Option<&str>, partial: bool) -> BuildOptions {
        BuildOptions {
            tag: tag.map(str::to_owned),
            published: "2026-10-24T12:00:00Z".to_owned(),
            partial,
        }
    }

    #[test]
    fn builds_the_five_platforms() {
        let d = archives("five");
        let m = build(PLAN.as_bytes(), &d.0, &opts(Some("v1.2.3"), false)).unwrap();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.platforms.len(), 5);
        assert_eq!(
            m.notes_url,
            "https://github.com/alwatroos/sqler/releases/tag/v1.2.3"
        );
        let win = &m.platforms["x86_64-pc-windows-msvc"];
        assert_eq!(
            win.url,
            "https://github.com/alwatroos/sqler/releases/download/v1.2.3/sqler-app-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            win.size,
            "sqler-app-x86_64-pc-windows-msvc.zip".len() as u64
        );
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.to_ascii_lowercase().contains("duckdb"));
        assert!(!json.contains(".msi") && !json.contains("installer"));
    }

    #[test]
    fn sha256_is_pinned() {
        let d = archives("sha");
        let name = "sqler-app-aarch64-apple-darwin.tar.xz";
        std::fs::write(d.0.join(name), b"abc").unwrap();
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        std::fs::write(d.0.join(format!("{name}.sha256")), format!("{sha} {name}")).unwrap();
        let m = build(PLAN.as_bytes(), &d.0, &opts(None, false)).unwrap();
        let p = &m.platforms["aarch64-apple-darwin"];
        assert_eq!((p.sha256.as_str(), p.size), (sha, 3));
    }

    #[test]
    fn tag_defaults_to_the_version() {
        let d = archives("deftag");
        let m = build(PLAN.as_bytes(), &d.0, &opts(None, false)).unwrap();
        assert!(m.platforms["x86_64-apple-darwin"].url.contains("/v1.2.3/"));
    }

    #[test]
    fn tag_and_announcement_mismatches_fail() {
        let d = archives("tags");
        assert!(build(PLAN.as_bytes(), &d.0, &opts(Some("v1.2.4"), false)).is_err());
        let plan = PLAN.replace(
            "\"announcement_tag\": \"v1.2.3\"",
            "\"announcement_tag\": \"v1.2.4\"",
        );
        assert!(build(plan.as_bytes(), &d.0, &opts(None, false)).is_err());
    }

    #[test]
    fn missing_target_fails_unless_partial() {
        let d = archives("missing");
        std::fs::remove_file(d.0.join("sqler-app-x86_64-apple-darwin.tar.xz")).unwrap();
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, false)).is_err());
        let m = build(PLAN.as_bytes(), &d.0, &opts(None, true)).unwrap();
        assert_eq!(m.platforms.len(), 4);
        assert!(!m.platforms.contains_key("x86_64-apple-darwin"));
    }

    #[test]
    fn partial_needs_one_entry() {
        let d = archives("none");
        for t in RELEASE_TARGETS {
            let ext = if t.contains("-pc-windows-") {
                "zip"
            } else {
                "tar.xz"
            };
            std::fs::remove_file(d.0.join(format!("sqler-app-{t}.{ext}"))).unwrap();
        }
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, true)).is_err());
    }

    #[test]
    fn sidecar_mismatch_and_absence() {
        let d = archives("side");
        let side = d.0.join("sqler-app-aarch64-apple-darwin.tar.xz.sha256");
        std::fs::write(&side, format!("{}  x\n", "0".repeat(64))).unwrap();
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, false)).is_err());
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, true)).is_err());
        std::fs::remove_file(&side).unwrap();
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, false)).is_err());
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, true)).is_ok());
    }

    #[test]
    fn zip_for_a_unix_triple_fails() {
        let d = archives("zipunix");
        let plan = PLAN.replace(
            "sqler-app-x86_64-apple-darwin.tar.xz",
            "sqler-app-x86_64-apple-darwin.zip",
        );
        assert!(build(plan.as_bytes(), &d.0, &opts(None, true)).is_err());
    }

    #[test]
    fn unknown_triple_fails() {
        let d = archives("triple");
        let plan = PLAN.replace("aarch64-unknown-linux-gnu", "riscv64gc-unknown-linux-gnu");
        assert!(build(plan.as_bytes(), &d.0, &opts(None, true)).is_err());
    }

    #[test]
    fn duckdb_names_fail() {
        let d = archives("duck");
        let plan = PLAN.replace(
            "sqler-app-x86_64-apple-darwin.tar.xz",
            "sqler-app-x86_64-apple-darwin-DuckDB.tar.xz",
        );
        assert!(build(plan.as_bytes(), &d.0, &opts(None, true)).is_err());
    }

    #[test]
    fn plan_shape_errors() {
        let d = archives("shape");
        assert!(build(b"not json", &d.0, &opts(None, false)).is_err());
        let none = PLAN.replace("\"app_name\": \"sqler-app\"", "\"app_name\": \"x\"");
        assert!(build(none.as_bytes(), &d.0, &opts(None, false)).is_err());
        let two = PLAN.replace("\"app_name\": \"other-app\"", "\"app_name\": \"sqler-app\"");
        assert!(build(two.as_bytes(), &d.0, &opts(None, false)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_archive_is_refused() {
        let d = archives("link");
        let name = d.0.join("sqler-app-x86_64-apple-darwin.tar.xz");
        let target = d.0.join("elsewhere");
        std::fs::rename(&name, &target).unwrap();
        std::os::unix::fs::symlink(&target, &name).unwrap();
        assert!(build(PLAN.as_bytes(), &d.0, &opts(None, false)).is_err());
    }

    #[test]
    fn release_targets_match_dist_workspace() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        let text = std::fs::read_to_string(root.join("dist-workspace.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        let targets: Vec<&str> = value["dist"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(targets, RELEASE_TARGETS);
    }

    #[test]
    fn current_target_is_a_release_target() {
        if let Some(t) = sqler_update::current_target() {
            assert!(RELEASE_TARGETS.contains(&t));
        }
    }
}
