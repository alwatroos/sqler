// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The name index: sorted, case-folded names with prefix and fuzzy matching, plus the public
//! query and hit types.
//!
//! A [`NameIndex`] is built once per cached listing or table, by the worker, before the data is
//! published. A prefix lookup is a binary search; a fuzzy lookup is one linear scan that stops
//! early inside each name. Matching runs on case-folded `char`s (per-`char` lowercase).

use std::cmp::Ordering;

use sqler_driver_api::sqler_core::{DbObject, ObjectKind, ObjectPath};

/// The number of hits a [`NameQuery`] returns unless told otherwise.
pub const DEFAULT_LIMIT: usize = 50;

/// What a name names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum NameKind {
    /// A database object of the given kind.
    Object(ObjectKind),
    /// A column of a loaded table.
    Column,
}

/// How well a name matches the query text. Better qualities sort first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MatchQuality {
    /// The name equals the text, ignoring case.
    Exact,
    /// The name starts with the text.
    Prefix,
    /// The text occurs contiguously elsewhere in the name.
    Substring,
    /// The text's characters occur in order in the name, with gaps.
    Fuzzy,
}

/// How a query text is matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchMode {
    /// The name must start with the text.
    Prefix,
    /// The text must be a case-insensitive subsequence of the name.
    Fuzzy,
}

/// A name lookup: text, scope, kinds, mode and a bound on the result size.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NameQuery {
    /// The text to match, compared ignoring case. Empty matches everything.
    pub text: String,
    /// Only names whose parent path lies within this path are returned.
    pub scope: ObjectPath,
    /// The kinds to return; empty means all.
    pub kinds: Vec<NameKind>,
    /// How the text is matched.
    pub mode: MatchMode,
    /// The most hits to return.
    pub limit: usize,
}

impl NameQuery {
    /// A prefix query over the whole catalog, returning at most [`DEFAULT_LIMIT`] hits.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            scope: ObjectPath::root(),
            kinds: Vec::new(),
            mode: MatchMode::Prefix,
            limit: DEFAULT_LIMIT,
        }
    }

    /// Restricts the lookup to names within `scope`.
    #[must_use]
    pub fn with_scope(mut self, scope: ObjectPath) -> Self {
        self.scope = scope;
        self
    }

    /// Restricts the lookup to these kinds; an empty list means all.
    #[must_use]
    pub fn with_kinds(mut self, kinds: Vec<NameKind>) -> Self {
        self.kinds = kinds;
        self
    }

    /// Matches the text as a case-insensitive subsequence instead of a prefix.
    #[must_use]
    pub fn fuzzy(mut self) -> Self {
        self.mode = MatchMode::Fuzzy;
        self
    }

    /// Sets the most hits to return.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// One result of a [`NameQuery`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NameHit {
    /// The name as cached, with its original case.
    pub name: String,
    /// What the name names.
    pub kind: NameKind,
    /// The path of the node that contains it: the listing's path, or the table's child path for a column.
    pub parent: ObjectPath,
    /// How well it matched.
    pub quality: MatchQuality,
}

impl NameHit {
    /// The object this hit names, or `None` for a column.
    #[must_use]
    pub fn object(&self) -> Option<DbObject> {
        match self.kind {
            NameKind::Object(kind) => Some(DbObject::new(kind, self.parent.clone(), &self.name)),
            NameKind::Column => None,
        }
    }
}

/// Case-folds text for matching.
///
/// Lowercases per `char` so the result of a prefix never depends on what follows it
/// (`str::to_lowercase` maps a word-final `Σ` to `ς` but a mid-word one to `σ`).
pub(crate) fn fold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars().flat_map(char::to_lowercase).collect()
}

#[derive(Debug, Clone)]
struct IndexEntry {
    lower: Box<str>,
    slot: u32,
}

/// Sorted lowercase names, each pointing back to its position in the source slice.
#[derive(Debug, Clone, Default)]
pub(crate) struct NameIndex {
    entries: Box<[IndexEntry]>,
}

impl NameIndex {
    /// Indexes `names`; a name's slot is its position in the iterator.
    pub(crate) fn build<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut entries: Vec<IndexEntry> = names
            .into_iter()
            .enumerate()
            .map(|(slot, name)| IndexEntry {
                lower: fold(name).into_boxed_str(),
                // A catalog with more than `u32::MAX` names in one node is not realistic.
                slot: u32::try_from(slot).unwrap_or(u32::MAX),
            })
            .collect();
        entries.sort_unstable_by(|a, b| a.lower.cmp(&b.lower).then(a.slot.cmp(&b.slot)));
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    /// Heap memory held: entry array plus name bytes.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.entries.len() * size_of::<IndexEntry>()
            + self.entries.iter().map(|e| e.lower.len()).sum::<usize>()
    }

    /// Number of indexed names.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Slots whose folded name equals `lower`, in slot order.
    pub(crate) fn exact(&self, lower: &str) -> impl Iterator<Item = u32> + '_ {
        let start = self.entries.partition_point(|e| &*e.lower < lower);
        let end = self.entries.partition_point(|e| &*e.lower <= lower);
        self.entries
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|e| e.slot)
    }

    /// Calls `f(slot, quality, penalty)` for every name matching `needle_lower`.
    ///
    /// Prefix mode reports `Exact` and `Prefix` only, found by binary search. Fuzzy mode scans
    /// every name and reports all four qualities. An empty needle matches everything as `Prefix`.
    /// The penalty is 0 for `Exact` and `Prefix`, the start offset for `Substring`, and the start
    /// offset plus the total gaps for `Fuzzy`, all in `char`s. Call order is unspecified.
    pub(crate) fn search(
        &self,
        needle_lower: &str,
        mode: MatchMode,
        f: &mut dyn FnMut(u32, MatchQuality, u32),
    ) {
        match mode {
            MatchMode::Prefix => {
                let start = self.entries.partition_point(|e| &*e.lower < needle_lower);
                for e in self.entries.get(start..).unwrap_or_default() {
                    if !e.lower.starts_with(needle_lower) {
                        break;
                    }
                    f(e.slot, prefix_quality(&e.lower, needle_lower), 0);
                }
            }
            MatchMode::Fuzzy => {
                let needle_len = needle_lower.chars().count();
                for e in &self.entries {
                    if let Some((quality, penalty)) = classify(&e.lower, needle_lower, needle_len) {
                        f(e.slot, quality, penalty);
                    }
                }
            }
        }
    }
}

fn prefix_quality(name: &str, needle: &str) -> MatchQuality {
    if !needle.is_empty() && name.len() == needle.len() {
        MatchQuality::Exact
    } else {
        MatchQuality::Prefix
    }
}

/// Matches a folded name against a folded needle in fuzzy mode.
fn classify(name: &str, needle: &str, needle_len: usize) -> Option<(MatchQuality, u32)> {
    if name.starts_with(needle) {
        return Some((prefix_quality(name, needle), 0));
    }
    if let Some(byte) = name.find(needle) {
        let offset = name.get(..byte).map_or(0, |head| head.chars().count());
        return Some((MatchQuality::Substring, to_u32(offset)));
    }
    subsequence_penalty(name, needle, needle_len).map(|p| (MatchQuality::Fuzzy, p))
}

/// Greedy leftmost subsequence match; penalty is the start offset plus the gaps between matches.
fn subsequence_penalty(name: &str, needle: &str, needle_len: usize) -> Option<u32> {
    let mut wanted = needle.chars();
    let mut next = wanted.next()?;
    for (pos, c) in name.chars().enumerate() {
        if c != next {
            continue;
        }
        if let Some(n) = wanted.next() {
            next = n;
        } else {
            // start offset + gaps = (last matched position + 1) - needle length.
            return Some(to_u32((pos + 1).saturating_sub(needle_len)));
        }
    }
    None
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The fields a hit is ranked by.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ranked<'a> {
    pub(crate) quality: MatchQuality,
    pub(crate) penalty: u32,
    /// Length of the name in `char`s.
    pub(crate) name_chars: usize,
    /// The folded name.
    pub(crate) folded: &'a str,
    pub(crate) kind: NameKind,
    pub(crate) parent: &'a ObjectPath,
}

pub(crate) fn path_key(p: &ObjectPath) -> (Option<&str>, Option<&str>, Option<&str>) {
    (
        p.database.as_deref(),
        p.schema.as_deref(),
        p.table.as_deref(),
    )
}

/// Order of hits: quality, penalty, name length, folded name, kind, parent. Names that differ
/// only in case tie here; the caller breaks the tie by the original name, then the path.
pub(crate) fn rank(a: &Ranked<'_>, b: &Ranked<'_>) -> Ordering {
    a.quality
        .cmp(&b.quality)
        .then(a.penalty.cmp(&b.penalty))
        .then(a.name_chars.cmp(&b.name_chars))
        .then_with(|| a.folded.cmp(b.folded))
        .then(a.kind.cmp(&b.kind))
        .then_with(|| path_key(a.parent).cmp(&path_key(b.parent)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(index: &NameIndex, needle: &str, mode: MatchMode) -> Vec<(u32, MatchQuality, u32)> {
        let mut out = Vec::new();
        index.search(&fold(needle), mode, &mut |s, q, p| out.push((s, q, p)));
        out.sort_unstable();
        out
    }

    #[test]
    fn build_sorts_and_reports_size() {
        let index = NameIndex::build(["b", "A", "c"]);
        assert_eq!(index.len(), 3);
        assert!(index.heap_bytes() >= 3 * size_of::<IndexEntry>() + 3);
        assert_eq!(NameIndex::build([]).heap_bytes(), 0);
    }

    #[test]
    fn exact_is_case_insensitive_and_returns_duplicates() {
        let index = NameIndex::build(["Users", "orders", "USERS", "user"]);
        assert_eq!(index.exact("users").collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(index.exact("nope").count(), 0);
        assert_eq!(index.exact("zzz").count(), 0);
    }

    #[test]
    fn prefix_mode_exact_and_prefix() {
        let index = NameIndex::build(["user", "users", "xuser", "Us", "order"]);
        assert_eq!(
            hits(&index, "USER", MatchMode::Prefix),
            vec![(0, MatchQuality::Exact, 0), (1, MatchQuality::Prefix, 0)]
        );
        assert_eq!(hits(&index, "zz", MatchMode::Prefix).len(), 0);
    }

    #[test]
    fn empty_text_matches_everything_as_prefix() {
        let index = NameIndex::build(["a", "b", ""]);
        for mode in [MatchMode::Prefix, MatchMode::Fuzzy] {
            let all = hits(&index, "", mode);
            assert_eq!(all.len(), 3);
            assert!(all.iter().all(|h| h.1 == MatchQuality::Prefix && h.2 == 0));
        }
    }

    #[test]
    fn fuzzy_qualities_and_penalties() {
        let index = NameIndex::build(["user", "users", "my_user", "u_s_e_r", "xyz", "usr"]);
        let got = hits(&index, "user", MatchMode::Fuzzy);
        assert_eq!(
            got,
            vec![
                (0, MatchQuality::Exact, 0),
                (1, MatchQuality::Prefix, 0),
                (2, MatchQuality::Substring, 3),
                (3, MatchQuality::Fuzzy, 3),
            ]
        );
    }

    #[test]
    fn fuzzy_penalty_counts_start_and_gaps() {
        let index = NameIndex::build(["xaxbxc", "abc", "axbxc"]);
        let got = hits(&index, "abc", MatchMode::Fuzzy);
        assert_eq!(
            got,
            vec![
                (0, MatchQuality::Fuzzy, 3),
                (1, MatchQuality::Exact, 0),
                (2, MatchQuality::Fuzzy, 2),
            ]
        );
    }

    #[test]
    fn fuzzy_needs_every_char_in_order() {
        let index = NameIndex::build(["abc"]);
        assert_eq!(hits(&index, "cba", MatchMode::Fuzzy).len(), 0);
        assert_eq!(hits(&index, "abcd", MatchMode::Fuzzy).len(), 0);
    }

    #[test]
    fn unicode_case_fold() {
        let index = NameIndex::build(["été", "Être", "ete"]);
        assert_eq!(fold("ÉTÉ"), "été");
        assert_eq!(index.exact(&fold("ÉTÉ")).collect::<Vec<_>>(), vec![0]);
        assert_eq!(
            hits(&index, "ÉT", MatchMode::Prefix),
            vec![(0, MatchQuality::Prefix, 0)]
        );
        // Penalty counts chars, not bytes.
        let got = hits(&NameIndex::build(["éé_x"]), "x", MatchMode::Fuzzy);
        assert_eq!(got, vec![(0, MatchQuality::Substring, 3)]);
    }

    #[test]
    fn greek_sigma_folds_consistently() {
        assert_eq!(
            fold("ΟΔΟΣ"),
            fold("ΟΔΟΣΑ").chars().take(4).collect::<String>()
        );
        let index = NameIndex::build(["ΟΔΟΣΑ", "οδοσα"]);
        assert_eq!(
            hits(&index, "ΟΔΟΣ", MatchMode::Prefix),
            vec![(0, MatchQuality::Prefix, 0), (1, MatchQuality::Prefix, 0)]
        );
        let got = hits(&index, "ΟΔΣΑ", MatchMode::Fuzzy);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|h| h.1 == MatchQuality::Fuzzy));
    }

    fn ranked<'a>(
        quality: MatchQuality,
        penalty: u32,
        folded: &'a str,
        parent: &'a ObjectPath,
    ) -> Ranked<'a> {
        Ranked {
            quality,
            penalty,
            name_chars: folded.chars().count(),
            folded,
            kind: NameKind::Column,
            parent,
        }
    }

    #[test]
    fn rank_orders_by_quality_penalty_length_name_kind_parent() {
        let root = ObjectPath::root();
        let in_a = ObjectPath::root().in_schema("a");
        let mut list = [
            ranked(MatchQuality::Fuzzy, 0, "a", &root),
            ranked(MatchQuality::Substring, 2, "zz", &root),
            ranked(MatchQuality::Substring, 1, "zzzz", &root),
            ranked(MatchQuality::Prefix, 0, "abc", &root),
            ranked(MatchQuality::Prefix, 0, "ab", &root),
            ranked(MatchQuality::Prefix, 0, "ac", &root),
            ranked(MatchQuality::Exact, 0, "q", &in_a),
            ranked(MatchQuality::Exact, 0, "q", &root),
        ];
        list.reverse();
        list.sort_by(rank);
        let order: Vec<_> = list
            .iter()
            .map(|r| (r.quality, r.folded, r.parent.schema.is_some()))
            .collect();
        assert_eq!(
            order,
            vec![
                (MatchQuality::Exact, "q", false),
                (MatchQuality::Exact, "q", true),
                (MatchQuality::Prefix, "ab", false),
                (MatchQuality::Prefix, "ac", false),
                (MatchQuality::Prefix, "abc", false),
                (MatchQuality::Substring, "zzzz", false),
                (MatchQuality::Substring, "zz", false),
                (MatchQuality::Fuzzy, "a", false),
            ]
        );
    }

    #[test]
    fn rank_is_stable_under_input_order() {
        let root = ObjectPath::root();
        let a = ranked(MatchQuality::Prefix, 0, "ab", &root);
        let b = ranked(MatchQuality::Prefix, 0, "ac", &root);
        let c = ranked(MatchQuality::Prefix, 0, "ad", &root);
        let mut one = [c, a, b];
        let mut two = [b, c, a];
        one.sort_by(rank);
        two.sort_by(rank);
        let one: Vec<_> = one.iter().map(|r| r.folded).collect();
        let two: Vec<_> = two.iter().map(|r| r.folded).collect();
        assert_eq!(one, two);
        assert_eq!(rank(&a, &a), Ordering::Equal);
    }

    #[test]
    fn query_builders_and_hit_object() {
        let q = NameQuery::new("ab");
        assert_eq!(q.limit, DEFAULT_LIMIT);
        assert_eq!(q.mode, MatchMode::Prefix);
        assert_eq!(q.kinds.len(), 0);
        let q = q
            .with_scope(ObjectPath::root().in_schema("main"))
            .with_kinds(vec![NameKind::Column])
            .fuzzy()
            .with_limit(3);
        assert_eq!(q.mode, MatchMode::Fuzzy);
        assert_eq!(q.limit, 3);
        assert_eq!(q.kinds, vec![NameKind::Column]);

        let parent = ObjectPath::root().in_schema("main");
        let hit = NameHit {
            name: "T".into(),
            kind: NameKind::Object(ObjectKind::Table),
            parent: parent.clone(),
            quality: MatchQuality::Exact,
        };
        assert_eq!(
            hit.object(),
            Some(DbObject::new(ObjectKind::Table, parent.clone(), "T"))
        );
        let col = NameHit {
            kind: NameKind::Column,
            ..hit
        };
        assert_eq!(col.object(), None);
    }
}
