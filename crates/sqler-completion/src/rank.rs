// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Matching of a typed prefix against candidate labels, and ordering of candidates.
//!
//! Match classes, best first: exact (case-insensitive), prefix, word-segment prefix
//! (`uid` matches `user_id`), subsequence. A label that matches none is dropped.

use std::cmp::Ordering;

use crate::dialect_data::fold;

/// How well a label matches the prefix (lower is better).
pub(crate) type Class = u8;

pub(crate) const EXACT: Class = 0;
pub(crate) const PREFIX: Class = 1;
pub(crate) const WORD: Class = 2;
pub(crate) const FUZZY: Class = 3;

/// Matches labels against one typed prefix.
pub(crate) struct Matcher {
    folded: Vec<char>,
}

impl Matcher {
    pub(crate) fn new(prefix: &str) -> Self {
        Self {
            folded: prefix.chars().map(fold).collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.folded.is_empty()
    }

    /// The match class and, for fuzzy matches, the number of skipped label characters.
    pub(crate) fn test(&self, label: &str) -> Option<(Class, u32)> {
        let p = &self.folded;
        if p.is_empty() {
            return Some((PREFIX, 0));
        }
        let mut chars = label.chars().map(fold);
        let matched = p.iter().take_while(|&&c| chars.next() == Some(c)).count();
        if matched == p.len() {
            return Some((
                if chars.next().is_none() {
                    EXACT
                } else {
                    PREFIX
                },
                0,
            ));
        }

        // Subsequence: required for both the word and the fuzzy class.
        let mut pi = 0;
        let mut first = 0usize;
        let mut last = 0usize;
        for (idx, c) in label.chars().map(fold).enumerate() {
            if c == p[pi] {
                if pi == 0 {
                    first = idx;
                }
                last = idx;
                pi += 1;
                if pi == p.len() {
                    break;
                }
            }
        }
        if pi < p.len() {
            return None;
        }
        if word_segments_match(label, p) {
            return Some((WORD, 0));
        }
        let gap = u32::try_from(last - first + 1 - p.len()).unwrap_or(u32::MAX);
        Some((FUZZY, gap))
    }
}

/// Whether the prefix is a concatenation of prefixes of successive `_`/camelCase segments.
fn word_segments_match(label: &str, prefix: &[char]) -> bool {
    const MAX_PREFIX: usize = 24;
    const MAX_SEGMENTS: usize = 16;
    if prefix.len() > MAX_PREFIX {
        return false;
    }
    let chars: Vec<char> = label.chars().collect();
    let mut starts: Vec<usize> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, &c) in chars.iter().enumerate() {
        let start = match prev {
            None => c != '_',
            Some(p) => {
                (p == '_' && c != '_')
                    || ((p.is_lowercase() || p.is_ascii_digit()) && c.is_uppercase())
            }
        };
        if start {
            starts.push(i);
        }
        prev = Some(c);
    }
    if starts.len() < 2 || starts.len() > MAX_SEGMENTS {
        return false;
    }
    let folded: Vec<char> = chars.iter().map(|&c| fold(c)).collect();
    let segs: Vec<&[char]> = starts
        .iter()
        .enumerate()
        .map(|(k, &s)| &folded[s..starts.get(k + 1).copied().unwrap_or(folded.len())])
        .collect();
    let mut budget = 4000usize;
    seg_go(prefix, &segs, 0, &mut budget)
}

fn seg_go(prefix: &[char], segs: &[&[char]], from: usize, budget: &mut usize) -> bool {
    if prefix.is_empty() {
        return true;
    }
    for (k, seg) in segs.iter().enumerate().skip(from) {
        let max = seg.len().min(prefix.len());
        for take in (1..=max).rev() {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            if seg[..take] == prefix[..take] && seg_go(&prefix[take..], segs, k + 1, budget) {
                return true;
            }
        }
    }
    false
}

/// Case-insensitive, then case-sensitive, comparison of two labels.
pub(crate) fn cmp_labels(a: &str, b: &str) -> Ordering {
    a.chars()
        .map(fold)
        .cmp(b.chars().map(fold))
        .then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(prefix: &str, label: &str) -> Option<Class> {
        Matcher::new(prefix).test(label).map(|m| m.0)
    }

    #[test]
    fn classes() {
        assert_eq!(class("users", "Users"), Some(EXACT));
        assert_eq!(class("us", "users"), Some(PREFIX));
        assert_eq!(class("", "anything"), Some(PREFIX));
        assert_eq!(class("uid", "user_id"), Some(WORD));
        assert_eq!(class("oi", "OrderItems"), Some(WORD));
        assert_eq!(class("id", "user_id"), Some(WORD));
        assert_eq!(class("usid", "user_id"), Some(WORD));
        assert_eq!(class("urd", "user_id"), Some(FUZZY));
        assert_eq!(class("xyz", "user_id"), None);
        assert_eq!(class("idu", "user_id"), None);
    }

    #[test]
    fn fuzzy_gap_orders_tighter_first() {
        let m = Matcher::new("abc");
        assert_eq!(m.test("xabcx"), Some((FUZZY, 0)));
        let tight = m.test("xaxbc").map(|x| x.1);
        let loose = m.test("xaxxbxxc").map(|x| x.1);
        assert!(tight < loose);
    }

    #[test]
    fn unicode_and_long_input_do_not_panic() {
        assert_eq!(class("ü", "Über"), Some(PREFIX));
        let long = "a".repeat(100);
        assert!(class(&long, "a_b_c_d").is_none());
        let segs = "a_".repeat(15) + "a";
        assert_eq!(Matcher::new(&"a".repeat(16)).test(&segs), Some((WORD, 0)));
        let hard = "ab_".repeat(15) + "ab";
        assert!(
            Matcher::new(&("ab".repeat(15) + "zz"))
                .test(&hard)
                .is_none()
        );
    }

    #[test]
    fn label_order_is_case_insensitive_first() {
        assert_eq!(cmp_labels("apple", "Banana"), Ordering::Less);
        assert_eq!(cmp_labels("a", "A"), Ordering::Greater);
    }
}
