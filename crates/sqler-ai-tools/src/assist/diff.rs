// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A small line-based unified diff (longest common subsequence).

#![allow(clippy::many_single_char_names)] // a, b, n, m, i, j are the textbook names

use std::fmt::Write as _;

/// The most `n * m` cells the LCS table may have; above it one hunk replaces everything.
const MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().map(|l| l.trim_end_matches('\r')).collect()
}

fn script(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > MAX_CELLS {
        let mut ops = vec![Op::Delete; n];
        ops.extend(std::iter::repeat_n(Op::Insert, m));
        return ops;
    }
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ops = Vec::with_capacity(n + m);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Equal);
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push(Op::Delete);
            i += 1;
        } else {
            ops.push(Op::Insert);
            j += 1;
        }
    }
    ops.extend(std::iter::repeat_n(Op::Delete, n - i));
    ops.extend(std::iter::repeat_n(Op::Insert, m - j));
    ops
}

/// A unified diff of `original` against `fixed`, `context` unchanged lines around each change.
/// Headers are `--- original` and `+++ fixed`. Identical text gives `""`.
#[must_use]
pub fn unified_diff(original: &str, fixed: &str, context: usize) -> String {
    let a = lines(original);
    let b = lines(fixed);
    if a == b {
        return String::new();
    }
    let ops = script(&a, &b);
    // Line positions before each op.
    let mut a_pos = Vec::with_capacity(ops.len());
    let mut b_pos = Vec::with_capacity(ops.len());
    let (mut x, mut y) = (0usize, 0usize);
    for op in &ops {
        a_pos.push(x);
        b_pos.push(y);
        match op {
            Op::Equal => {
                x += 1;
                y += 1;
            }
            Op::Delete => x += 1,
            Op::Insert => y += 1,
        }
    }
    let changes: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| **op != Op::Equal)
        .map(|(i, _)| i)
        .collect();
    let mut out = String::from("--- original\n+++ fixed\n");
    let mut k = 0;
    while k < changes.len() {
        let mut end = k;
        while end + 1 < changes.len() && changes[end + 1] - changes[end] - 1 <= 2 * context {
            end += 1;
        }
        let from = changes[k].saturating_sub(context);
        let to = (changes[end] + context + 1).min(ops.len());
        let a_len = ops[from..to].iter().filter(|o| **o != Op::Insert).count();
        let b_len = ops[from..to].iter().filter(|o| **o != Op::Delete).count();
        let a_start = if a_len == 0 {
            a_pos[from]
        } else {
            a_pos[from] + 1
        };
        let b_start = if b_len == 0 {
            b_pos[from]
        } else {
            b_pos[from] + 1
        };
        let _ = writeln!(out, "@@ -{a_start},{a_len} +{b_start},{b_len} @@");
        for idx in from..to {
            let (sign, line) = match ops[idx] {
                Op::Equal => (' ', a[a_pos[idx]]),
                Op::Delete => ('-', a[a_pos[idx]]),
                Op::Insert => ('+', b[b_pos[idx]]),
            };
            out.push(sign);
            out.push_str(line);
            out.push('\n');
        }
        k = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_empty() {
        assert_eq!(unified_diff("a\nb", "a\nb", 3), "");
        assert_eq!(unified_diff("a\r\nb", "a\nb\n", 3), "");
    }

    #[test]
    fn replace_one_line_with_context() {
        let d = unified_diff("select a\nfrom b\nwhere c", "select a\nfrom b2\nwhere c", 1);
        assert_eq!(
            d,
            "--- original\n+++ fixed\n@@ -1,3 +1,3 @@\n select a\n-from b\n+from b2\n where c\n"
        );
    }

    #[test]
    fn insert_and_delete() {
        let d = unified_diff("a\nc", "a\nb\nc", 0);
        assert_eq!(d, "--- original\n+++ fixed\n@@ -1,0 +2,1 @@\n+b\n");
        let d = unified_diff("a\nb\nc", "a\nc", 0);
        assert_eq!(d, "--- original\n+++ fixed\n@@ -2,1 +1,0 @@\n-b\n");
    }

    #[test]
    fn far_changes_make_two_hunks_and_near_ones_merge() {
        let a: Vec<String> = (0..20).map(|i| format!("l{i}")).collect();
        let mut b = a.clone();
        b[2] = "x".into();
        b[17] = "y".into();
        let d = unified_diff(&a.join("\n"), &b.join("\n"), 1);
        assert_eq!(d.matches("@@ -").count(), 2);
        let mut c = a.clone();
        c[2] = "x".into();
        c[4] = "y".into();
        let d = unified_diff(&a.join("\n"), &c.join("\n"), 1);
        assert_eq!(d.matches("@@ -").count(), 1);
    }

    #[test]
    fn huge_inputs_replace_everything() {
        let a: Vec<String> = (0..2100).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..2100).map(|i| format!("b{i}")).collect();
        let d = unified_diff(&a.join("\n"), &b.join("\n"), 3);
        assert!(d.contains("@@ -1,2100 +1,2100 @@"));
        assert_eq!(d.matches("@@ -").count(), 1);
    }
}
