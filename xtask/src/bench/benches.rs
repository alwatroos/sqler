// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The five existing benches, run through `cargo bench` with their default arguments.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::desktop::wait_bounded;

/// `(package, bench)` pairs, in report order.
pub const BENCHES: [(&str, &str); 5] = [
    ("sqler-ui", "grid_viewport"),
    ("sqler-ui", "grid_sort"),
    ("sqler-ui", "editor_buffer"),
    ("sqler-ui", "editor_intellisense"),
    ("sqler-completion", "completion"),
];

const BENCH_LIMIT: Duration = Duration::from_mins(45);

/// Whether `line` matches `^[a-z0-9_]+=\S+$`.
fn is_metric_line(line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !value.is_empty()
        && !value.chars().any(char::is_whitespace)
}

/// The `key=value` lines of a bench's stdout, keys prefixed with `bench_<name>_`.
pub fn filter_lines(name: &str, stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| is_metric_line(l))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (format!("bench_{name}_{k}"), v.to_owned()))
        .collect()
}

/// Run one bench; returns its prefixed lines and the exit code.
pub fn run_bench(
    root: &Path,
    package: &str,
    name: &str,
) -> Result<(Vec<(String, String)>, i32), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut child = Command::new(cargo)
        .current_dir(root)
        .args(["bench", "-p", package, "--bench", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start cargo bench: {}", e.kind()))?;
    let reader = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = out.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        })
    });
    let status = wait_bounded(&mut child, BENCH_LIMIT);
    let stdout = reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let status = status.map_err(|e| format!("bench {name}: {e}"))?;
    Ok((filter_lines(name, &stdout), status.code().unwrap_or(-1)))
}

#[cfg(test)]
mod tests {
    use super::{BENCHES, filter_lines, is_metric_line};

    #[test]
    fn keeps_only_key_value_lines_and_prefixes_them() {
        let out = "running 1 bench\nrows=1000\np50_ms=1.25\r\nbad key=1\nUPPER=1\nempty=\n=x\nnote: a=b c\nok_2=3\n";
        assert_eq!(
            filter_lines("grid_sort", out),
            vec![
                ("bench_grid_sort_rows".to_owned(), "1000".to_owned()),
                ("bench_grid_sort_p50_ms".to_owned(), "1.25".to_owned()),
                ("bench_grid_sort_ok_2".to_owned(), "3".to_owned()),
            ]
        );
    }

    #[test]
    fn line_filter_edges() {
        assert!(is_metric_line("a=b=c"));
        assert!(!is_metric_line("a = b"));
        assert!(!is_metric_line("a=b c"));
        assert!(!is_metric_line("no equals"));
    }

    #[test]
    fn the_five_benches() {
        assert_eq!(BENCHES.len(), 5);
        assert_eq!(BENCHES[4], ("sqler-completion", "completion"));
    }
}
