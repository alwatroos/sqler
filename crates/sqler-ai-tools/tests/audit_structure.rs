// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A source guard: no tool can skip redaction, the envelope or the audit (ADR-0037).
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            out.push((path, text));
        }
    }
}

/// The text of a file before its unit-test module (a lone `#[cfg(test)]` on an
/// import or a helper does not hide the rest of the file).
fn production(text: &str) -> &str {
    let cut = ["#[cfg(test)]\nmod ", "mod tests {"]
        .iter()
        .filter_map(|m| text.find(m))
        .min()
        .unwrap_or(text.len());
    &text[..cut]
}

fn all() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    assert!(out.len() > 5, "no sources found");
    out
}

fn count(needle: &str) -> Vec<(String, usize)> {
    all()
        .into_iter()
        .map(|(p, t)| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, production(&t).matches(needle).count())
        })
        .filter(|(_, n)| *n > 0)
        .collect()
}

#[test]
fn tool_results_are_built_only_in_finish() {
    for needle in ["ToolResult::ok(", "ToolResult::error("] {
        assert_eq!(count(needle), [("toolset.rs".to_owned(), 1)], "{needle}");
    }
    // Any other constructor spelling (`ToolResult::new`, `ToolResult::from`, ...).
    assert_eq!(
        count("ToolResult::"),
        [("toolset.rs".to_owned(), 2)],
        "only ok/error in finish"
    );
    let toolset = all()
        .into_iter()
        .find(|(p, _)| p.ends_with("toolset.rs"))
        .unwrap()
        .1;
    let finish = toolset
        .split("fn finish(")
        .nth(1)
        .unwrap()
        .split("\n    async fn run(")
        .next()
        .unwrap();
    assert!(finish.contains("ToolResult::ok("));
    assert!(finish.contains("ToolResult::error("));
    assert!(finish.contains("wrap_untrusted(&body)"));
    assert!(finish.contains(".apply(&mut body"));
}

#[test]
fn dispatch_is_called_once_and_call_goes_through_call_detailed() {
    assert_eq!(count("self.dispatch("), [("toolset.rs".to_owned(), 1)]);
    let toolset = all()
        .into_iter()
        .find(|(p, _)| p.ends_with("toolset.rs"))
        .unwrap()
        .1;
    let call_fn = toolset
        .split("pub async fn call(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(call_fn.contains("self.call_detailed(call)"));
    assert_eq!(count(".record(&audit)"), [("toolset.rs".to_owned(), 1)]);
    assert_eq!(count("fn finish("), [("toolset.rs".to_owned(), 1)]);
    assert_eq!(count("self.finish("), [("toolset.rs".to_owned(), 1)]);
}

#[test]
fn no_other_entry_runs_a_tool_without_finish() {
    // `self.run(` is the raw, unredacted path: it may appear only in `call_detailed`,
    // which hands the outcome to `finish`.
    assert_eq!(count("self.run("), [("toolset.rs".to_owned(), 1)]);
    let toolset = all()
        .into_iter()
        .find(|(p, _)| p.ends_with("toolset.rs"))
        .unwrap()
        .1;
    let detailed = toolset
        .split("pub async fn call_detailed(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(detailed.contains("self.run(call, &mut ran)"));
    assert!(detailed.contains("self.finish("));
    // Every public async entry of the tool set is `call` or `call_detailed`.
    let production = production(&toolset);
    let entries: Vec<&str> = production
        .lines()
        .filter(|l| l.trim_start().starts_with("pub async fn "))
        .map(str::trim)
        .collect();
    assert_eq!(
        entries.len(),
        2,
        "new public async entry point: {entries:?}"
    );
}

#[test]
fn call_detailed_arms_the_cancel_guard_before_running_and_disarms_before_finish() {
    let toolset = all()
        .into_iter()
        .find(|(p, _)| p.ends_with("toolset.rs"))
        .unwrap()
        .1;
    let detailed = toolset
        .split("pub async fn call_detailed(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    let at = |needle: &str| detailed.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("CancelGuard {") < at("self.run("));
    assert!(at("self.run(") < at("guard.disarm()"));
    assert!(at("guard.disarm()") < at("self.finish("));
    // The guard is the only other writer to the sink, and it never awaits.
    assert_eq!(count(".record(&entry)"), [("toolset.rs".to_owned(), 1)]);
    let guard = toolset
        .split("impl Drop for CancelGuard")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    assert!(!guard.contains(".await"));
}
