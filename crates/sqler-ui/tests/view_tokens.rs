// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Source guard for the M4.5 visual system: the restyled views take spacing, type sizes, radii
//! and colours from the theme tokens and `workspace::chrome`, never from literals. The scan
//! reads each file up to its first `#[cfg(test)]` and fails with `file:line` on a literal
//! spacing helper, type-size helper, radius or shadow helper, literal colour or text glyph.

use std::fs;
use std::path::Path;

/// The scanned view files, relative to `src/`, each with the marker the scan starts at (`""`
/// scans from the top). The sidebar is scanned from `fn node_icon(` on: the container above it
/// belongs to the workbench shell.
const SCANNED: &[(&str, &str)] = &[
    ("palette/view.rs", ""),
    ("query/view/dialogs.rs", ""),
    ("grid/edit/view.rs", ""),
    ("sidebar/view.rs", "fn node_icon("),
    ("connections/view.rs", ""),
    ("workspace/drop_dialog.rs", ""),
    ("grid/view.rs", ""),
    ("grid/detail_view.rs", ""),
    ("export/view.rs", ""),
    ("settings/view.rs", ""),
    ("history/view.rs", ""),
    ("workspace/view/audit.rs", ""),
    ("query/view/toolbar.rs", ""),
    ("query/view/mod.rs", ""),
    ("tabs/view.rs", "pub(crate) fn breadcrumbs_element("),
    ("query/view/results.rs", ""),
    ("ai/hub/view.rs", ""),
    ("ai/settings/view.rs", ""),
    ("ai/session/view.rs", ""),
    ("ai/approval/view.rs", ""),
    ("workspace/view/ai_session.rs", ""),
    ("ai/editor/view.rs", ""),
    ("workspace/view/assist.rs", ""),
    ("workspace/view/agent.rs", ""),
    ("workspace/view/shell/agent.rs", ""),
    ("plugins/grant/view.rs", ""),
    ("plugins/manager/view.rs", ""),
    ("workspace/view/plugins.rs", ""),
    ("plugins/approval/view.rs", ""),
    ("workspace/view/plugin_writes.rs", ""),
    ("plugins/widget/view.rs", ""),
    ("workspace/view/plugin_ui.rs", ""),
];

/// Spacing helper prefixes: `.p_2()`, `.gap_1()` and friends.
const SPACING: &[&str] = &[
    "p", "px", "py", "pl", "pr", "pt", "pb", "m", "mx", "my", "ml", "mr", "mt", "mb", "gap",
];

/// Substrings that are never allowed in a scanned line.
const FORBIDDEN: &[&str] = &[
    ".text_xs()",
    ".text_sm()",
    ".text_base()",
    ".text_lg()",
    ".text_xl()",
    "rounded_sm()",
    "rounded_md()",
    "rounded_lg()",
    "shadow_sm()",
    "shadow_md()",
    "shadow_lg()",
    "gpui::black()",
    "gpui::white()",
    "rgb(0x",
    "\"▸\"",
    "\"▾\"",
    "\"×\"",
    "\"[x]\"",
    "\"[ ]\"",
    "\"(•)\"",
    "\"( )\"",
];

/// The first problem on `line`, if any.
fn problem(line: &str) -> Option<String> {
    let code = line.trim_start();
    if code.starts_with("//") {
        return None;
    }
    if let Some(bad) = FORBIDDEN.iter().find(|bad| line.contains(**bad)) {
        return Some(format!("`{bad}`"));
    }
    if line
        .match_indices("px(")
        .any(|(at, _)| line[at + 3..].starts_with(|c: char| c.is_ascii_digit()))
    {
        return Some("a literal `px(<number>)`".to_owned());
    }
    for prefix in SPACING {
        let needle = format!(".{prefix}_");
        if line
            .match_indices(&needle)
            .any(|(at, _)| line[at + needle.len()..].starts_with(|c: char| c.is_ascii_digit()))
        {
            return Some(format!("a spacing helper `{needle}<n>()`"));
        }
    }
    None
}

#[test]
fn views_use_tokens_not_literals() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for (file, marker) in SCANNED {
        let text = fs::read_to_string(src.join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        let skip = if marker.is_empty() {
            0
        } else {
            text.lines()
                .position(|l| l.contains(marker))
                .unwrap_or_else(|| panic!("{file}: marker `{marker}` not found"))
        };
        for (ix, line) in text.lines().enumerate().skip(skip) {
            if line.contains("#[cfg(test)]") {
                break;
            }
            if let Some(what) = problem(line) {
                found.push(format!("{file}:{}: {what}", ix + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "hard-coded view styling:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scan_catches_what_it_should() {
    for bad in [
        "div().px_2()",
        ".gap_1()",
        ".py_0p5()",
        ".w(px(80.))",
        ".text_sm()",
        ".rounded_md()",
        ".shadow_lg()",
        "gpui::black().opacity(0.4)",
        "rgb(0x112233)",
        "child(\"×\")",
    ] {
        assert!(problem(bad).is_some(), "{bad}");
    }
    for good in [
        ".px(px(SPACE[3]))",
        ".w(px(LABEL_WIDTH))",
        ".gap(px(SPACE[2]))",
        "// .px_2() in a comment",
        ".text_size(px(scale.sm))",
        ".pt(px(OVERLAY_TOP))",
    ] {
        assert!(problem(good).is_none(), "{good}");
    }
}
