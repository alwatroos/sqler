// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The ordered key/value report: budgets, breaches, stdout lines and the JSON file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Number, Value, json};

/// The release binary budget (80 MB), in bytes.
pub const BINARY_BUDGET_BYTES: u64 = 83_886_080;
/// The cold-start budget, in milliseconds.
pub const COLD_START_BUDGET_MS: u64 = 500;
/// The idle RSS budget, in MiB.
pub const IDLE_RSS_BUDGET_MIB: u64 = 150;

/// The keys under which a group records that it failed.
const ERROR_KEYS: [&str; 6] = [
    "size_error",
    "cold_start_error",
    "rss_error",
    "benches_error",
    "build_error",
    "commit_error",
];

/// The measurements of one run, in insertion order.
#[derive(Debug)]
pub struct Report {
    header: Vec<(String, String)>,
    features: Vec<String>,
    metrics: Vec<(String, String)>,
}

/// The identifying fields printed before any measurement.
#[derive(Debug)]
pub struct Header {
    /// UTC date, `YYYY-MM-DD`.
    pub date: String,
    /// The commit as given or resolved.
    pub commit: String,
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// `desktop` or `ci-runner`.
    pub source: String,
    /// Machine class text.
    pub machine: String,
    /// `release-artifact` or `local-release`.
    pub build: String,
    /// The dist feature line.
    pub features: Vec<String>,
}

/// One line per value: control characters would break the `key=value` format.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Whether `value` breaches the budget of `key`; `None` when the key has no budget.
fn breached(key: &str, value: &str) -> Option<bool> {
    let over = |budget: u64| {
        let v = value.parse::<f64>().ok()?;
        #[allow(clippy::cast_precision_loss)]
        Some(v > budget as f64)
    };
    if key == "binary_bytes" {
        over(BINARY_BUDGET_BYTES).or(Some(false))
    } else if key.starts_with("cold_start_") && key.ends_with("_mean_ms") {
        over(COLD_START_BUDGET_MS).or(Some(false))
    } else if key.starts_with("idle_rss_") && key.ends_with("_median_mib") {
        over(IDLE_RSS_BUDGET_MIB).or(Some(false))
    } else if key.starts_with("bench_") && key.ends_with("_exit") {
        Some(value != "0")
    } else {
        None
    }
}

impl Report {
    /// A report holding only the header fields.
    pub fn new(h: Header) -> Self {
        let line = |k: &str, v: &str| (k.to_owned(), one_line(v));
        Self {
            header: vec![
                line("date", &h.date),
                line("commit", &h.commit),
                line("os", &h.os),
                line("arch", &h.arch),
                line("source", &h.source),
                line("machine", &h.machine),
                line("build", &h.build),
                line("features", &h.features.join(",")),
            ],
            features: h.features,
            metrics: Vec::new(),
        }
    }

    /// Set a value; a repeated key keeps its first position.
    pub fn set(&mut self, key: &str, value: impl AsRef<str>) {
        let value = one_line(value.as_ref());
        if let Some(slot) = self.metrics.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.metrics.push((key.to_owned(), value));
        }
    }

    /// Record a group error; a second error of the same group is appended after `; `.
    pub fn add_error(&mut self, key: &str, message: impl AsRef<str>) {
        let joined = match self.get(key) {
            Some(first) => format!("{first}; {}", message.as_ref()),
            None => message.as_ref().to_owned(),
        };
        self.set(key, joined);
    }

    /// Set a float with one decimal place.
    pub fn set_f64(&mut self, key: &str, value: f64) {
        self.set(key, format!("{value:.1}"));
    }

    /// A metric value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.metrics
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The keys that are over budget, in report order.
    pub fn breaches(&self) -> Vec<String> {
        self.metrics
            .iter()
            .filter(|(k, v)| breached(k, v) == Some(true))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Whether a group recorded an error. Only the group error keys count: a bench metric whose name
    /// happens to end in `_error` is a measurement, not a failure.
    pub fn has_errors(&self) -> bool {
        self.metrics
            .iter()
            .any(|(k, _)| ERROR_KEYS.contains(&k.as_str()))
    }

    /// `pass`, `breach` or `failure`.
    pub fn result(&self) -> &'static str {
        if self.has_errors() {
            "failure"
        } else if self.breaches().is_empty() {
            "pass"
        } else {
            "breach"
        }
    }

    /// The process exit code: 0 for a pass, 1 otherwise.
    pub fn exit_code(&self) -> u8 {
        u8::from(self.result() != "pass")
    }

    /// Write the `key=value` lines, the breaches and the result.
    ///
    /// # Errors
    /// Returns the write error (a closed pipe, for example).
    pub fn write_lines(&self, w: &mut dyn Write) -> std::io::Result<()> {
        for (k, v) in self.header.iter().chain(&self.metrics) {
            writeln!(w, "{k}={v}")?;
        }
        for key in self.breaches() {
            writeln!(w, "breach={key}")?;
        }
        writeln!(w, "result={}", self.result())
    }

    /// The JSON document of the report.
    pub fn to_json(&self) -> Value {
        let field = |k: &str| {
            self.header
                .iter()
                .find(|(h, _)| h == k)
                .map_or_else(String::new, |(_, v)| v.clone())
        };
        let mut metrics = Map::new();
        for (k, v) in &self.metrics {
            metrics.insert(k.clone(), number_or_text(v));
        }
        json!({
            "schema": 1,
            "date": field("date"),
            "commit": field("commit"),
            "os": field("os"),
            "arch": field("arch"),
            "source": field("source"),
            "machine": field("machine"),
            "build": field("build"),
            "features": self.features,
            "metrics": metrics,
            "breaches": self.breaches(),
            "result": self.result(),
        })
    }

    /// The file name `bench-<date>-<commit7>-<os>.json`.
    pub fn file_name(&self) -> String {
        let get = |k: &str| {
            self.header
                .iter()
                .find(|(h, _)| h == k)
                .map_or("", |(_, v)| v.as_str())
        };
        let commit: String = get("commit").chars().take(7).collect();
        format!("bench-{}-{commit}-{}.json", get("date"), get("os"))
    }

    /// Write the JSON file into `dir` (created when missing) and return its path.
    ///
    /// # Errors
    /// A message without any path when the directory or the file cannot be written.
    pub fn write_json(&self, dir: &Path) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create the output directory: {}", e.kind()))?;
        let path = dir.join(self.file_name());
        let mut text = serde_json::to_string_pretty(&self.to_json())
            .map_err(|e| format!("cannot encode the report: {e}"))?;
        text.push('\n');
        std::fs::write(&path, text)
            .map_err(|e| format!("cannot write the report file: {}", e.kind()))?;
        Ok(path)
    }
}

fn number_or_text(value: &str) -> Value {
    if let Ok(n) = value.parse::<i64>() {
        return Value::Number(n.into());
    }
    if let Ok(f) = value.parse::<f64>()
        && let Some(n) = Number::from_f64(f)
    {
        return Value::Number(n);
    }
    Value::String(value.to_owned())
}

/// The UTC date of `time` as `YYYY-MM-DD` (days-from-civil inverse; no date crate).
pub fn civil_date(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::{Header, Report, breached, civil_date};

    fn sample() -> Report {
        let mut r = Report::new(Header {
            date: "2026-10-24".to_owned(),
            commit: "abcdef1234".to_owned(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            source: "ci-runner".to_owned(),
            machine: "GitHub-hosted ubuntu-22.04".to_owned(),
            build: "release-artifact".to_owned(),
            features: vec!["plugins".to_owned(), "driver-mssql".to_owned()],
        });
        r.set("binary_bytes", "1000");
        r.set_f64("cold_start_none_mean_ms", 123.456);
        r.set("idle_rss_none_open", "3/3");
        r.set("bench_completion_exit", "0");
        r
    }

    #[test]
    fn dates() {
        let day = |d: u64, extra: u64| UNIX_EPOCH + Duration::from_secs(d * 86_400 + extra);
        assert_eq!(civil_date(UNIX_EPOCH), "1970-01-01");
        assert_eq!(civil_date(day(11_016, 43_200)), "2000-02-29");
        assert_eq!(civil_date(day(20_750, 86_399)), "2026-10-24");
    }

    #[test]
    fn breach_rule_boundaries() {
        assert_eq!(breached("binary_bytes", "83886080"), Some(false));
        assert_eq!(breached("binary_bytes", "83886081"), Some(true));
        assert_eq!(breached("cold_start_none_mean_ms", "500.0"), Some(false));
        assert_eq!(breached("cold_start_plugin_mean_ms", "500.1"), Some(true));
        assert_eq!(breached("idle_rss_none_median_mib", "150.0"), Some(false));
        assert_eq!(breached("idle_rss_plugin_median_mib", "150.1"), Some(true));
        assert_eq!(breached("bench_grid_sort_exit", "0"), Some(false));
        assert_eq!(breached("bench_grid_sort_exit", "1"), Some(true));
        assert_eq!(breached("cold_start_none_max_ms", "9999.0"), None);
        assert_eq!(breached("binary_budget_bytes", "1"), None);
    }

    #[test]
    fn lines_follow_the_contract_order() {
        let mut r = sample();
        r.set("binary_bytes", "83886081");
        let mut out = Vec::new();
        assert!(r.write_lines(&mut out).is_ok());
        let text = String::from_utf8_lossy(&out).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "date=2026-10-24");
        assert_eq!(lines[7], "features=plugins,driver-mssql");
        assert_eq!(lines[8], "binary_bytes=83886081");
        assert_eq!(lines[9], "cold_start_none_mean_ms=123.5");
        assert_eq!(lines[lines.len() - 2], "breach=binary_bytes");
        assert_eq!(lines[lines.len() - 1], "result=breach");
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn errors_make_a_failure_and_a_clean_run_passes() {
        let mut r = sample();
        assert_eq!(r.result(), "pass");
        assert_eq!(r.exit_code(), 0);
        r.set("rss_error", "timeout\nsecond line");
        assert_eq!(r.get("rss_error"), Some("timeout second line"));
        assert_eq!(r.result(), "failure");
        r.add_error("rss_error", "again");
        assert_eq!(r.get("rss_error"), Some("timeout second line; again"));
    }

    #[test]
    fn only_group_error_keys_make_a_failure() {
        let mut r = sample();
        r.set("bench_x_max_error", "0.1");
        assert!(!r.has_errors());
        assert_eq!(r.result(), "pass");
        for key in super::ERROR_KEYS {
            let mut r = sample();
            r.set(key, "boom");
            assert!(r.has_errors(), "{key}");
            assert_eq!(r.result(), "failure");
        }
    }

    #[test]
    fn json_has_numbers_text_and_no_path_separator() {
        let mut r = sample();
        r.set("rss_error", "open-sessions were not 3 of 3");
        let json = r.to_json();
        assert_eq!(json["schema"], 1);
        assert_eq!(json["metrics"]["binary_bytes"], 1000);
        assert_eq!(json["metrics"]["idle_rss_none_open"], "3/3");
        assert_eq!(json["features"][1], "driver-mssql");
        assert_eq!(json["result"], "failure");
        let mut strings = Vec::new();
        collect(&json, &mut strings);
        for s in strings {
            if s != "3/3" {
                assert!(!s.contains('/') && !s.contains('\\'), "{s}");
            }
        }
        assert_eq!(r.file_name(), "bench-2026-10-24-abcdef1-linux.json");
    }

    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| collect(x, out)),
            _ => {}
        }
    }

    #[test]
    fn json_file_is_written() {
        let dir = std::env::temp_dir().join(format!("xtask-bench-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = sample().write_json(&dir);
        assert!(path.as_ref().is_ok_and(|p| p.is_file()), "{path:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
