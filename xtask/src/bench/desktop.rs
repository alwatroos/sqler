// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Size, cold start and idle RSS of one binary, in throwaway config directories.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sqler_plugin_api::Manifest;

/// The two measured variants: no plugin installed, and the SQL Server manifest installed and approved.
pub const VARIANTS: [&str; 2] = ["none", "plugin"];

const MEBIBYTE: f64 = 1_048_576.0;
const READY_LIMIT: Duration = Duration::from_mins(1);
const SETTLE: Duration = Duration::from_secs(6);
const RECHECK: Duration = Duration::from_secs(10);
const HYPERFINE_LIMIT: Duration = Duration::from_mins(20);

/// Wall-clock statistics of the cold start, in milliseconds.
#[derive(Debug, PartialEq)]
pub struct ColdStats {
    /// Mean.
    pub mean: f64,
    /// Standard deviation.
    pub stddev: f64,
    /// Fastest run.
    pub min: f64,
    /// Slowest run.
    pub max: f64,
}

/// Median, minimum and maximum of the idle RSS, in MiB.
#[derive(Debug, PartialEq)]
pub struct RssStats {
    /// Median of the readings.
    pub median: f64,
    /// Smallest reading.
    pub min: f64,
    /// Largest reading.
    pub max: f64,
}

/// Wait for `child` at most `limit`; a child still running then is killed.
pub fn wait_bounded(child: &mut Child, limit: Duration) -> Result<ExitStatus, String> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(e) => return Err(format!("cannot wait for a child process: {}", e.kind())),
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timeout after {} s", limit.as_secs()));
        }
        sleep(Duration::from_millis(100));
    }
}

/// Kills and reaps the child on every exit path.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The scratch root of a variant.
pub fn scratch_root(repo_root: &Path, variant: &str) -> PathBuf {
    repo_root.join("target").join("xtask-bench").join(variant)
}

/// The configuration directory the app resolves under `root` on `os` (`std::env::consts::OS`).
pub fn config_dir_for(os: &str, root: &Path) -> PathBuf {
    match os {
        "macos" => root
            .join("Library")
            .join("Application Support")
            .join("SQLer"),
        "windows" => root.join("appdata").join("SQLer"),
        _ => root.join("xdg").join("sqler"),
    }
}

/// The environment that points the app at the scratch root.
pub fn scratch_env(root: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("HOME", root.as_os_str().to_owned()),
        ("USERPROFILE", root.as_os_str().to_owned()),
        ("XDG_CONFIG_HOME", root.join("xdg").into_os_string()),
        ("APPDATA", root.join("appdata").into_os_string()),
        ("LOCALAPPDATA", root.join("localappdata").into_os_string()),
    ]
}

fn io_message(what: &str, e: &std::io::Error) -> String {
    format!("{what}: {}", e.kind())
}

fn connections_text() -> String {
    let mut text = "version = 1\n".to_owned();
    for i in 1..=3 {
        let _ = write!(
            text,
            "\n[[connection]]\nid = \"c{i}\"\nname = \"bench-{i}\"\ndriver = \"sqlite\"\ntarget = \"sqlite::memory:\"\n"
        );
    }
    text
}

/// The `plugin-grants.toml` text approving the declarative SQL Server manifest.
pub fn grants_text(manifest_text: &str) -> Result<String, String> {
    let manifest = Manifest::parse(manifest_text).map_err(|e| format!("bad manifest: {e}"))?;
    let key = manifest
        .grant_key(None)
        .map_err(|e| format!("no grant key: {e}"))?;
    let caps = manifest
        .capabilities()
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "version = 1\n\n[[grant]]\nkey = \"{key}\"\ndecision = \"approved\"\ncapabilities = [{caps}]\n"
    ))
}

/// Delete and recreate the scratch root of `variant` with its configuration; returns the root.
pub fn prepare_scratch(repo_root: &Path, variant: &str) -> Result<PathBuf, String> {
    let root = scratch_root(repo_root, variant);
    remove_scratch(&root);
    let config = config_dir_for(std::env::consts::OS, &root);
    fs::create_dir_all(&config).map_err(|e| io_message("cannot create the scratch config", &e))?;
    fs::write(config.join("connections.toml"), connections_text())
        .map_err(|e| io_message("cannot write the scratch connections", &e))?;
    if variant == "plugin" {
        let source = repo_root
            .join("plugins")
            .join("sqlserver")
            .join("sqler.toml");
        let bytes =
            fs::read(&source).map_err(|e| io_message("cannot read the SQL Server manifest", &e))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| "the SQL Server manifest is not UTF-8".to_owned())?;
        let grants = grants_text(&text)?;
        let plugin_dir = config.join("plugins").join("sqlserver");
        fs::create_dir_all(&plugin_dir)
            .map_err(|e| io_message("cannot create the scratch plugin directory", &e))?;
        fs::write(plugin_dir.join("sqler.toml"), bytes)
            .map_err(|e| io_message("cannot copy the manifest", &e))?;
        let grants_path = config.join("plugin-grants.toml");
        fs::write(&grants_path, grants).map_err(|e| io_message("cannot write the grants", &e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&grants_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| io_message("cannot restrict the grants file", &e))?;
        }
    }
    Ok(root)
}

/// Best-effort removal of a scratch root.
pub fn remove_scratch(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

/// The size in bytes of a file.
pub fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| io_message("cannot read the file size", &e))
}

/// Parse hyperfine's `--export-json` text; both the nested `summary.time_wall_clock` and the flat shape.
pub fn parse_hyperfine(text: &str) -> Result<ColdStats, String> {
    let json: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("hyperfine JSON is invalid: {e}"))?;
    let result = json
        .get("results")
        .and_then(|r| r.get(0))
        .ok_or_else(|| "hyperfine JSON has no results".to_owned())?;
    let field = |name: &str| -> Result<f64, String> {
        let nested = result
            .get("summary")
            .and_then(|s| s.get("time_wall_clock"))
            .and_then(|t| t.get(name))
            .and_then(serde_json::Value::as_f64);
        nested
            .or_else(|| result.get(name).and_then(serde_json::Value::as_f64))
            .map(|seconds| seconds * 1000.0)
            .ok_or_else(|| format!("hyperfine JSON has no `{name}`"))
    };
    Ok(ColdStats {
        mean: field("mean")?,
        stddev: field("stddev")?,
        min: field("min")?,
        max: field("max")?,
    })
}

/// Cold start of `bin` over `--exit-after-first-frame` with hyperfine, in the scratch config of `variant`.
pub fn cold_start(
    repo_root: &Path,
    bin: &Path,
    variant: &str,
    warmup: u32,
    runs: u32,
) -> Result<ColdStats, String> {
    let root = prepare_scratch(repo_root, variant)?;
    let result = run_hyperfine(repo_root, bin, &root, warmup, runs);
    remove_scratch(&root);
    result
}

fn run_hyperfine(
    repo_root: &Path,
    bin: &Path,
    root: &Path,
    warmup: u32,
    runs: u32,
) -> Result<ColdStats, String> {
    let json_path = root.join("cold.json");
    let command = format!("\"{}\" --exit-after-first-frame", bin.display());
    let mut cmd = Command::new("hyperfine");
    cmd.current_dir(repo_root)
        .args(["-N", "-w", &warmup.to_string(), "-r", &runs.to_string()])
        .arg("--export-json")
        .arg(&json_path)
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in scratch_env(root) {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "hyperfine is not installed".to_owned()
        } else {
            io_message("cannot start hyperfine", &e)
        }
    })?;
    let status = wait_bounded(&mut child, HYPERFINE_LIMIT)?;
    if !status.success() {
        return Err(format!("hyperfine failed ({status})"));
    }
    let text = fs::read_to_string(&json_path)
        .map_err(|e| io_message("cannot read the hyperfine result", &e))?;
    parse_hyperfine(&text)
}

/// Parse `sqler: open-sessions: N of M open, idling` anywhere in `text`.
pub fn parse_open_sessions(text: &str) -> Option<(u32, u32)> {
    text.lines().find_map(|line| {
        let rest = line.split("sqler: open-sessions: ").nth(1)?;
        let (n, rest) = rest.split_once(" of ")?;
        let (m, tail) = rest.split_once(' ')?;
        tail.starts_with("open, idling")
            .then(|| Some((n.trim().parse().ok()?, m.trim().parse().ok()?)))
            .flatten()
    })
}

/// `VmRSS` of a `/proc/<pid>/status` text, in KiB.
pub fn parse_vmrss_kib(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn read_rss_bytes(pid: u32) -> Result<u64, String> {
    let failed = |what: &str| format!("cannot read the resident memory ({what})");
    match std::env::consts::OS {
        "linux" => {
            let text = fs::read_to_string(format!("/proc/{pid}/status"))
                .map_err(|e| format!("{}: {}", failed("proc"), e.kind()))?;
            parse_vmrss_kib(&text)
                .map(|kib| kib * 1024)
                .ok_or_else(|| failed("no VmRSS"))
        }
        "macos" => {
            let out = Command::new("ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .output()
                .map_err(|e| format!("{}: {}", failed("ps"), e.kind()))?;
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u64>()
                .map(|kib| kib * 1024)
                .map_err(|_| failed("ps output"))
        }
        "windows" => {
            let out = Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!("(Get-Process -Id {pid}).WorkingSet64"),
                ])
                .output()
                .map_err(|e| format!("{}: {}", failed("powershell"), e.kind()))?;
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u64>()
                .map_err(|_| failed("powershell output"))
        }
        other => Err(format!("idle RSS is not measured on {other}")),
    }
}

/// Median, minimum and maximum of `values` (MiB); `None` when empty.
pub fn rss_stats(values: &[f64]) -> Option<RssStats> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    };
    Some(RssStats {
        median,
        min: sorted[0],
        max: sorted[sorted.len() - 1],
    })
}

/// Idle RSS of `bin --open-sessions` in the scratch config of `variant`, over `runs` runs.
pub fn idle_rss(
    repo_root: &Path,
    bin: &Path,
    variant: &str,
    runs: u32,
) -> Result<RssStats, String> {
    let root = prepare_scratch(repo_root, variant)?;
    let result = (1..=runs)
        .map(|i| rss_run(repo_root, bin, &root, i))
        .collect::<Result<Vec<f64>, String>>();
    remove_scratch(&root);
    result.and_then(|values| rss_stats(&values).ok_or_else(|| "no RSS readings".to_owned()))
}

fn rss_run(repo_root: &Path, bin: &Path, root: &Path, index: u32) -> Result<f64, String> {
    let log_path = root.join(format!("stderr-{index}.log"));
    let log = fs::File::create(&log_path).map_err(|e| io_message("cannot create the log", &e))?;
    let mut cmd = Command::new(bin);
    cmd.current_dir(repo_root)
        .arg("--open-sessions")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    for (k, v) in scratch_env(root) {
        cmd.env(k, v);
    }
    let child = cmd
        .spawn()
        .map_err(|e| io_message("cannot start the app", &e))?;
    let pid = child.id();
    let mut guard = Reaper(child);
    let start = Instant::now();
    let (open, total) = loop {
        let text = fs::read_to_string(&log_path).unwrap_or_default();
        if let Some(found) = parse_open_sessions(&text) {
            break found;
        }
        if let Ok(Some(status)) = guard.0.try_wait() {
            return Err(format!("the app exited early ({status})"));
        }
        if start.elapsed() >= READY_LIMIT {
            return Err("timeout waiting for the open-sessions line".to_owned());
        }
        sleep(Duration::from_millis(200));
    };
    if (open, total) != (3, 3) {
        return Err(format!("open-sessions was {open} of {total}, not 3 of 3"));
    }
    sleep(SETTLE);
    let first = read_rss_bytes(pid)?;
    sleep(RECHECK);
    read_rss_bytes(pid)?;
    #[allow(clippy::cast_precision_loss)]
    Ok(first as f64 / MEBIBYTE)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sqler_config::{ConfigDir, GrantsLoad};

    use super::{
        ColdStats, RssStats, config_dir_for, parse_hyperfine, parse_open_sessions, parse_vmrss_kib,
        prepare_scratch, remove_scratch, rss_stats, scratch_env,
    };

    #[test]
    fn hyperfine_nested_and_flat() {
        let nested = r#"{"results":[{"command":"x","summary":{"time_wall_clock":{"mean":0.25,"stddev":0.01,"min":0.2,"max":0.3}}}]}"#;
        let flat = r#"{"results":[{"command":"x","mean":0.25,"stddev":0.01,"min":0.2,"max":0.3}]}"#;
        let want = ColdStats {
            mean: 250.0,
            stddev: 10.0,
            min: 200.0,
            max: 300.0,
        };
        for text in [nested, flat] {
            let got = parse_hyperfine(text).unwrap_or_else(|e| panic!("{e}"));
            assert!((got.mean - want.mean).abs() < 1e-9);
            assert!((got.stddev - want.stddev).abs() < 1e-9);
            assert!((got.min - want.min).abs() < 1e-9);
            assert!((got.max - want.max).abs() < 1e-9);
        }
    }

    #[test]
    fn hyperfine_missing_field_or_shape_is_an_error() {
        let missing = r#"{"results":[{"mean":0.25,"stddev":0.01,"min":0.2}]}"#;
        assert!(parse_hyperfine(missing).is_err_and(|e| e.contains("max")));
        assert!(parse_hyperfine(r#"{"results":[]}"#).is_err());
        assert!(parse_hyperfine("not json").is_err());
    }

    #[test]
    fn open_sessions_line() {
        let log = "noise\nsqler: open-sessions: opened \"a\"\nsqler: open-sessions: 3 of 3 open, idling\n";
        assert_eq!(parse_open_sessions(log), Some((3, 3)));
        assert_eq!(
            parse_open_sessions("sqler: open-sessions: 2 of 3 open, idling"),
            Some((2, 3))
        );
        assert_eq!(
            parse_open_sessions("sqler: open-sessions: opened \"a\""),
            None
        );
        assert_eq!(parse_open_sessions(""), None);
    }

    #[test]
    fn vmrss_and_stats() {
        assert_eq!(
            parse_vmrss_kib("Name:\tsqler\nVmRSS:\t  2048 kB\nThreads:\t1\n"),
            Some(2048)
        );
        assert_eq!(parse_vmrss_kib("Name:\tsqler\n"), None);
        assert_eq!(
            rss_stats(&[3.0, 1.0, 2.0]),
            Some(RssStats {
                median: 2.0,
                min: 1.0,
                max: 3.0
            })
        );
        assert_eq!(rss_stats(&[1.0, 2.0]).map(|s| s.median), Some(1.5));
        assert_eq!(rss_stats(&[]), None);
    }

    #[test]
    fn config_dirs_per_os() {
        let root = Path::new("r");
        assert_eq!(
            config_dir_for("linux", root),
            root.join("xdg").join("sqler")
        );
        assert_eq!(
            config_dir_for("windows", root),
            root.join("appdata").join("SQLer")
        );
        assert_eq!(
            config_dir_for("macos", root),
            root.join("Library")
                .join("Application Support")
                .join("SQLer")
        );
        let env = scratch_env(root);
        assert!(
            env.iter()
                .any(|(k, v)| *k == "HOME" && Path::new(v) == root)
        );
        assert!(
            env.iter()
                .any(|(k, v)| *k == "XDG_CONFIG_HOME" && Path::new(v) == root.join("xdg"))
        );
    }

    fn temp_repo() -> std::path::PathBuf {
        let repo = super::super::repo_root();
        let fake = std::env::temp_dir().join(format!("xtask-bench-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&fake);
        let manifest = fake.join("plugins").join("sqlserver");
        assert!(std::fs::create_dir_all(&manifest).is_ok());
        assert!(
            std::fs::copy(
                repo.join("plugins").join("sqlserver").join("sqler.toml"),
                manifest.join("sqler.toml")
            )
            .is_ok()
        );
        fake
    }

    #[test]
    fn scratch_config_loads_in_both_variants() {
        let repo = temp_repo();
        for variant in ["none", "plugin"] {
            let root = prepare_scratch(&repo, variant).unwrap_or_else(|e| panic!("{e}"));
            let dir = ConfigDir::new(config_dir_for(std::env::consts::OS, &root));
            let store = dir
                .load_connection_store()
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(store.connections().len(), 3);
            match (variant, dir.load_plugin_grants()) {
                ("none", GrantsLoad::Missing) => {}
                ("plugin", GrantsLoad::Loaded(grants)) => {
                    let record = grants.for_plugin("sqlserver");
                    assert!(
                        record
                            .is_some_and(|r| r.decision() == sqler_config::GrantDecision::Approved
                                && r.capabilities() == ["driver.host:mssql"])
                    );
                    assert!(
                        dir.root()
                            .join("plugins")
                            .join("sqlserver")
                            .join("sqler.toml")
                            .is_file()
                    );
                }
                (v, other) => panic!("{v}: unexpected {other:?}"),
            }
            remove_scratch(&root);
        }
        let _ = std::fs::remove_dir_all(&repo);
    }
}
