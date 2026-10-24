// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Argument parsing for `bench`.

use std::path::PathBuf;

/// A measurement group that `--skip` can switch off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Hyperfine cold start.
    ColdStart,
    /// Idle resident memory with open sessions.
    Rss,
    /// Binary (and archive) size.
    Size,
    /// The five `cargo bench` targets.
    Benches,
}

/// The parsed options of `bench`.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    /// A release binary to measure; `None` means a local release build.
    pub bin: Option<PathBuf>,
    /// A release archive whose size is reported.
    pub archive: Option<PathBuf>,
    /// The commit to record; `None` means `git rev-parse HEAD`.
    pub commit: Option<String>,
    /// Output directory; `None` means `target/bench-results`.
    pub out: Option<PathBuf>,
    /// `desktop` or `ci-runner`.
    pub source: String,
    /// Free text describing the machine class.
    pub machine: String,
    /// `native` or `headless-software` (a software-rendered headless display; idle RSS is informational).
    pub display: String,
    /// Hyperfine runs.
    pub runs: u32,
    /// Hyperfine warmup runs.
    pub warmup: u32,
    /// Idle RSS runs.
    pub rss_runs: u32,
    /// Skipped groups.
    pub skip: Vec<Group>,
}

/// A usage error (exit code 2).
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

const USAGE_HINT: &str = "usage: cargo run -p xtask -- bench [--bin PATH] [--archive PATH] [--commit SHA] [--out DIR] \
[--source desktop|ci-runner] [--machine TEXT] [--display native|headless-software] [--runs N] [--warmup N] [--rss-runs N] \
[--skip cold-start|rss|size|benches]...";

/// The usage text of the task.
pub fn usage() -> &'static str {
    USAGE_HINT
}

fn count(flag: &str, value: &str) -> Result<u32, UsageError> {
    match value.parse::<u32>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(UsageError(format!(
            "`{flag}` needs a whole number of at least 1, got `{value}`"
        ))),
    }
}

fn commit(value: &str) -> Result<String, UsageError> {
    let ok = (7..=40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit());
    if ok {
        Ok(value.to_owned())
    } else {
        Err(UsageError(format!(
            "`--commit` needs 7 to 40 hex characters, got `{value}`"
        )))
    }
}

fn group(value: &str) -> Result<Group, UsageError> {
    match value {
        "cold-start" => Ok(Group::ColdStart),
        "rss" => Ok(Group::Rss),
        "size" => Ok(Group::Size),
        "benches" => Ok(Group::Benches),
        other => Err(UsageError(format!(
            "unknown group `{other}` for `--skip`, one of: cold-start, rss, size, benches"
        ))),
    }
}

/// Parse the arguments after `bench`.
pub fn parse(args: &[String]) -> Result<Options, UsageError> {
    let mut opts = Options {
        bin: None,
        archive: None,
        commit: None,
        out: None,
        source: "desktop".to_owned(),
        machine: "unspecified".to_owned(),
        display: "native".to_owned(),
        runs: 20,
        warmup: 3,
        rss_runs: 3,
        skip: Vec::new(),
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if !flag.starts_with("--") {
            return Err(UsageError(format!("unexpected argument `{flag}`")));
        }
        let known = [
            "--bin",
            "--archive",
            "--commit",
            "--out",
            "--source",
            "--machine",
            "--display",
            "--runs",
            "--warmup",
            "--rss-runs",
            "--skip",
        ];
        if !known.contains(&flag.as_str()) {
            return Err(UsageError(format!("unknown flag `{flag}`")));
        }
        if flag != "--skip" && seen.contains(&flag.as_str()) {
            return Err(UsageError(format!("`{flag}` given more than once")));
        }
        seen.push(flag.as_str());
        let Some(value) = iter.next() else {
            return Err(UsageError(format!("`{flag}` needs a value")));
        };
        match flag.as_str() {
            "--bin" => opts.bin = Some(PathBuf::from(value)),
            "--archive" => opts.archive = Some(PathBuf::from(value)),
            "--commit" => opts.commit = Some(commit(value)?),
            "--out" => opts.out = Some(PathBuf::from(value)),
            "--source" => {
                if value != "desktop" && value != "ci-runner" {
                    return Err(UsageError(format!(
                        "`--source` is `desktop` or `ci-runner`, got `{value}`"
                    )));
                }
                value.clone_into(&mut opts.source);
            }
            "--machine" => value.clone_into(&mut opts.machine),
            "--display" => {
                if value != "native" && value != "headless-software" {
                    return Err(UsageError(format!(
                        "`--display` is `native` or `headless-software`, got `{value}`"
                    )));
                }
                value.clone_into(&mut opts.display);
            }
            "--runs" => opts.runs = count(flag, value)?,
            "--warmup" => opts.warmup = count(flag, value)?,
            "--rss-runs" => opts.rss_runs = count(flag, value)?,
            _ => {
                let g = group(value)?;
                if !opts.skip.contains(&g) {
                    opts.skip.push(g);
                }
            }
        }
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::{Group, Options, UsageError, parse};

    fn p(args: &[&str]) -> Result<Options, UsageError> {
        parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn defaults() {
        let o = p(&[]).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(o.bin, None);
        assert_eq!(o.archive, None);
        assert_eq!(o.commit, None);
        assert_eq!(o.out, None);
        assert_eq!(o.source, "desktop");
        assert_eq!(o.machine, "unspecified");
        assert_eq!(o.display, "native");
        assert_eq!((o.runs, o.warmup, o.rss_runs), (20, 3, 3));
        assert_eq!(o.skip.len(), 0);
    }

    #[test]
    fn every_option() {
        let o = p(&[
            "--bin",
            "a",
            "--archive",
            "b",
            "--commit",
            "abcdef1",
            "--out",
            "o",
            "--source",
            "ci-runner",
            "--machine",
            "GitHub-hosted x",
            "--display",
            "headless-software",
            "--runs",
            "5",
            "--warmup",
            "1",
            "--rss-runs",
            "2",
            "--skip",
            "rss",
            "--skip",
            "benches",
            "--skip",
            "rss",
        ])
        .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(o.commit.as_deref(), Some("abcdef1"));
        assert_eq!(o.source, "ci-runner");
        assert_eq!(o.machine, "GitHub-hosted x");
        assert_eq!(o.display, "headless-software");
        assert_eq!((o.runs, o.warmup, o.rss_runs), (5, 1, 2));
        assert_eq!(o.skip, vec![Group::Rss, Group::Benches]);
    }

    #[test]
    fn usage_errors() {
        assert!(p(&["--nope", "x"]).is_err());
        assert!(p(&["extra"]).is_err());
        assert!(p(&["--bin"]).is_err());
        assert!(p(&["--bin", "a", "--bin", "b"]).is_err());
        assert!(p(&["--runs", "0"]).is_err());
        assert!(p(&["--warmup", "0"]).is_err());
        assert!(p(&["--rss-runs", "x"]).is_err());
        assert!(p(&["--runs", "-1"]).is_err());
        assert!(p(&["--source", "laptop"]).is_err());
        assert!(p(&["--skip", "everything"]).is_err());
        assert!(p(&["--display", "gpu"]).is_err());
        assert!(p(&["--commit", "abc123"]).is_err());
        assert!(p(&["--commit", "zzzzzzz"]).is_err());
        assert!(p(&["--commit", &"a".repeat(41)]).is_err());
        assert!(p(&["--commit", &"a".repeat(40)]).is_ok());
        assert!(p(&["--bench", "x"]).is_err());
    }
}
