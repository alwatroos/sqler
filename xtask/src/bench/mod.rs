// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The `bench` task: the headline numbers (size, cold start, idle RSS, the five benches) of one release binary.

mod args;
mod benches;
mod desktop;
mod report;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

use args::{Group, Options};
use report::{
    BINARY_BUDGET_BYTES, COLD_START_BUDGET_MS, Header, IDLE_RSS_BUDGET_MIB, Report, civil_date,
};

const BUILD_LIMIT: Duration = Duration::from_mins(90);

/// Usage text of the task, for the `xtask` front door.
pub fn usage() -> &'static str {
    args::usage()
}

/// Run `bench <args>`; never panics. Exit codes: 0 pass, 1 breach or failure, 2 usage error.
pub fn run(args: &[String]) -> ExitCode {
    match args::parse(args) {
        Err(err) => {
            eprintln!("error: {}", err.0);
            eprintln!("{}", args::usage());
            ExitCode::from(2)
        }
        Ok(options) => match execute(&options, &mut std::io::stdout().lock()) {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::from(1)
            }
        },
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The `[dist].features` line of `dist-workspace.toml`; `driver-duckdb` is refused.
pub fn parse_features(text: &str) -> Result<Vec<String>, String> {
    let doc: toml::Table = text
        .parse()
        .map_err(|e| format!("dist-workspace.toml is invalid: {e}"))?;
    let list = doc
        .get("dist")
        .and_then(|d| d.get("features"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "dist-workspace.toml has no [dist] features line".to_owned())?;
    let features: Vec<String> = list
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    if features.len() != list.len() {
        return Err("the [dist] features line holds a non-string entry".to_owned());
    }
    if features.iter().any(|f| f == "driver-duckdb") {
        return Err(
            "the [dist] features line holds driver-duckdb; DuckDB is not benchmarked".to_owned(),
        );
    }
    Ok(features)
}

fn read_features(root: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(root.join("dist-workspace.toml"))
        .map_err(|e| format!("cannot read dist-workspace.toml: {}", e.kind()))?;
    parse_features(&text)
}

fn resolve_commit(root: &Path, given: Option<&String>) -> Result<String, String> {
    if let Some(sha) = given {
        return Ok(sha.clone());
    }
    let out = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("cannot run git: {}", e.kind()))?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success()
        && (7..=40).contains(&sha.len())
        && sha.chars().all(|c| c.is_ascii_hexdigit())
    {
        Ok(sha)
    } else {
        Err("git rev-parse HEAD failed".to_owned())
    }
}

fn build_release(root: &Path, features: &[String]) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut child = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "--release",
            "-p",
            "sqler-app",
            "--locked",
            "--features",
        ])
        .arg(features.join(","))
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .spawn()
        .map_err(|e| format!("cannot start cargo build: {}", e.kind()))?;
    let status = desktop::wait_bounded(&mut child, BUILD_LIMIT)?;
    if !status.success() {
        return Err(format!("cargo build failed ({status})"));
    }
    Ok(root
        .join("target")
        .join("release")
        .join(format!("sqler{}", std::env::consts::EXE_SUFFIX)))
}

fn write_error(err: &std::io::Error) -> String {
    format!("cannot write to standard output: {err}")
}

fn execute(options: &Options, w: &mut dyn std::io::Write) -> Result<u8, String> {
    let root = repo_root();
    let features = read_features(&root)?;
    let commit = resolve_commit(&root, options.commit.as_ref());
    let mut report = Report::new(Header {
        date: civil_date(SystemTime::now()),
        commit: commit.clone().unwrap_or_else(|_| "unknown".to_owned()),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        source: options.source.clone(),
        machine: options.machine.clone(),
        build: if options.bin.is_some() {
            "release-artifact"
        } else {
            "local-release"
        }
        .to_owned(),
        features: features.clone(),
    });
    if let Err(message) = commit {
        report.set("commit_error", message);
    }
    let skipped = |g: Group| options.skip.contains(&g);
    let needs_bin = !(skipped(Group::Size) && skipped(Group::ColdStart) && skipped(Group::Rss));
    let bin = match (&options.bin, needs_bin) {
        (_, false) => Err(String::new()),
        (Some(path), true) => {
            std::path::absolute(path).map_err(|e| format!("bad --bin path: {}", e.kind()))
        }
        (None, true) => build_release(&root, &features),
    };
    let bin = match bin {
        Ok(path) => Some(path),
        Err(message) => {
            if needs_bin {
                report.set("build_error", message);
            }
            None
        }
    };
    report.set("display", &options.display);
    if options.display == "headless-software" {
        report.set_rss_informational();
        for variant in desktop::VARIANTS {
            report.set(&format!("rss_{variant}_informational"), "true");
        }
        report.set(
            "rss_informational_reason",
            "idle RSS measured on a software-rendered headless display; not comparable with the desktop budget",
        );
    }
    if !skipped(Group::Size) {
        measure_size(&mut report, bin.as_deref(), options);
    }
    if !skipped(Group::ColdStart) {
        measure_cold_start(&mut report, &root, bin.as_deref(), options);
    }
    if !skipped(Group::Rss) {
        measure_rss(&mut report, &root, bin.as_deref(), options);
    }
    if !skipped(Group::Benches) {
        measure_benches(&mut report, &root);
    }
    let out = options
        .out
        .clone()
        .unwrap_or_else(|| root.join("target").join("bench-results"));
    let written = report.write_json(&out);
    let printed = report.write_lines(w).and_then(|()| w.flush());
    let path = written?;
    eprintln!(
        "wrote {}",
        path.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    );
    printed.map_err(|e| write_error(&e))?;
    Ok(report.exit_code())
}

fn missing_bin(report: &mut Report, group: &str) {
    if report.get("build_error").is_none() {
        report.set(&format!("{group}_error"), "no binary to measure");
    } else {
        report.set(&format!("{group}_error"), "skipped, the build failed");
    }
}

fn measure_size(report: &mut Report, bin: Option<&Path>, options: &Options) {
    let Some(bin) = bin else {
        missing_bin(report, "size");
        return;
    };
    match desktop::file_size(bin) {
        Ok(bytes) => {
            report.set("binary_bytes", bytes.to_string());
            report.set("binary_budget_bytes", BINARY_BUDGET_BYTES.to_string());
            let headroom = i128::from(BINARY_BUDGET_BYTES) - i128::from(bytes);
            report.set("binary_headroom_bytes", headroom.to_string());
        }
        Err(message) => report.add_error("size_error", message),
    }
    if let Some(archive) = &options.archive {
        match desktop::file_size(archive) {
            Ok(bytes) => report.set("archive_bytes", bytes.to_string()),
            Err(message) => report.add_error("size_error", format!("archive: {message}")),
        }
    }
}

fn measure_cold_start(report: &mut Report, root: &Path, bin: Option<&Path>, options: &Options) {
    let Some(bin) = bin else {
        missing_bin(report, "cold_start");
        return;
    };
    for variant in desktop::VARIANTS {
        match desktop::cold_start(root, bin, variant, options.warmup, options.runs) {
            Ok(s) => {
                report.set_f64(&format!("cold_start_{variant}_mean_ms"), s.mean);
                report.set_f64(&format!("cold_start_{variant}_stddev_ms"), s.stddev);
                report.set_f64(&format!("cold_start_{variant}_min_ms"), s.min);
                report.set_f64(&format!("cold_start_{variant}_max_ms"), s.max);
            }
            Err(message) => report.add_error("cold_start_error", format!("{variant}: {message}")),
        }
    }
    report.set("cold_start_budget_ms", COLD_START_BUDGET_MS.to_string());
}

fn measure_rss(report: &mut Report, root: &Path, bin: Option<&Path>, options: &Options) {
    let Some(bin) = bin else {
        missing_bin(report, "rss");
        return;
    };
    for variant in desktop::VARIANTS {
        match desktop::idle_rss(root, bin, variant, options.rss_runs) {
            Ok(s) => {
                report.set_f64(&format!("idle_rss_{variant}_median_mib"), s.median);
                report.set_f64(&format!("idle_rss_{variant}_min_mib"), s.min);
                report.set_f64(&format!("idle_rss_{variant}_max_mib"), s.max);
                report.set(&format!("idle_rss_{variant}_open"), "3/3");
            }
            Err(message) => report.add_error("rss_error", format!("{variant}: {message}")),
        }
    }
    report.set("idle_rss_budget_mib", IDLE_RSS_BUDGET_MIB.to_string());
}

fn measure_benches(report: &mut Report, root: &Path) {
    for (package, name) in benches::BENCHES {
        match benches::run_bench(root, package, name) {
            Ok((lines, code)) => {
                for (key, value) in lines {
                    report.set(&key, value);
                }
                report.set(&format!("bench_{name}_exit"), code.to_string());
            }
            Err(message) => report.add_error("benches_error", format!("{name}: {message}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_features;

    #[test]
    fn reads_the_dist_feature_line() {
        let text = "[dist]\nfeatures = [\"plugins\", \"driver-mssql\"]\n";
        assert_eq!(
            parse_features(text),
            Ok(vec!["plugins".to_owned(), "driver-mssql".to_owned()])
        );
    }

    #[test]
    fn rejects_duckdb_and_a_missing_line() {
        let duck = "[dist]\nfeatures = [\"plugins\", \"driver-duckdb\"]\n";
        assert!(parse_features(duck).is_err_and(|e| e.contains("driver-duckdb")));
        assert!(parse_features("[dist]\n").is_err());
        assert!(parse_features("[dist]\nfeatures = [1]\n").is_err());
        assert!(parse_features("not toml [").is_err());
    }

    #[test]
    fn the_repo_line_is_the_release_set() {
        let text = std::fs::read_to_string(super::repo_root().join("dist-workspace.toml"))
            .unwrap_or_default();
        let features = parse_features(&text).unwrap_or_default();
        assert!(features.contains(&"driver-mssql".to_owned()));
    }
}
