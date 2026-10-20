// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! "Add next occurrence": word lookup, wrapping search and the chain that remembers what was added.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The identifier at `offset` (also when the cursor sits right after it), if any.
///
/// Identifier characters are alphanumeric characters, `_` and `$`.
#[must_use]
pub fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let (before, after) = (text.get(..offset)?, text.get(offset..)?);
    let on_word = after.chars().next().is_some_and(is_word)
        || before.chars().next_back().is_some_and(is_word);
    if !on_word {
        return None;
    }
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(offset, |(i, _)| i);
    let end = after
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(i, _)| offset + i);
    Some(start..end)
}

/// The first occurrence of `needle` that starts at or after `after`, wrapping to the start of the text.
///
/// `None` for an empty needle, no match, or an `after` that is not on a char boundary.
#[must_use]
pub fn next_occurrence(text: &str, needle: &str, after: usize) -> Option<Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    let tail = text.get(after.min(text.len())..)?;
    let start = match tail.find(needle) {
        Some(i) => text.len() - tail.len() + i,
        None => text.find(needle)?,
    };
    Some(start..start + needle.len())
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The ranges added by consecutive "add next occurrence" commands.
///
/// The chain is only continued while the needle and the text are unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceChain {
    needle: String,
    text_hash: u64,
    added: Vec<Range<usize>>,
}

impl OccurrenceChain {
    /// A chain that starts with the already selected `first`.
    #[must_use]
    pub fn new(text: &str, needle: &str, first: Range<usize>) -> Self {
        Self {
            needle: needle.to_owned(),
            text_hash: hash_of(text),
            added: vec![first],
        }
    }

    /// Whether the chain still applies to this needle and text.
    #[must_use]
    pub fn continues(&self, text: &str, needle: &str) -> bool {
        self.needle == needle && self.text_hash == hash_of(text)
    }

    /// The last range that was added.
    #[must_use]
    pub fn last(&self) -> Option<&Range<usize>> {
        self.added.last()
    }

    /// Whether `range` is already part of the chain.
    #[must_use]
    pub fn contains(&self, range: &Range<usize>) -> bool {
        self.added.contains(range)
    }

    /// Records an added range.
    pub fn push(&mut self, range: Range<usize>) {
        self.added.push(range);
    }

    /// How many ranges the chain holds, the first selection included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.added.len()
    }

    /// Whether the chain holds no range (never true for a chain from [`OccurrenceChain::new`]).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
    }
}

/// What "add next occurrence" asks the view to do.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OccurrenceStep {
    /// Select this range (the word under an empty selection).
    Select(Range<usize>),
    /// Add a selection on this range.
    Add(Range<usize>),
    /// Nothing to do: no word, no further match, or the search wrapped onto a selected range.
    Nothing,
}

/// One step of "add next occurrence" on `chain`.
pub(super) fn step(
    chain: &mut Option<OccurrenceChain>,
    text: &str,
    selection: &Range<usize>,
) -> OccurrenceStep {
    let Some(needle) = (!selection.is_empty())
        .then(|| text.get(selection.clone()))
        .flatten()
    else {
        *chain = None;
        return match word_at(text, selection.start) {
            Some(w) => OccurrenceStep::Select(w),
            None => OccurrenceStep::Nothing,
        };
    };
    let live = chain.as_ref().is_some_and(|c| c.continues(text, needle));
    if !live {
        *chain = Some(OccurrenceChain::new(text, needle, selection.clone()));
    }
    let Some(c) = chain.as_mut() else {
        return OccurrenceStep::Nothing;
    };
    let from = c.last().map_or(selection.end, |r| r.end);
    match next_occurrence(text, needle, from) {
        Some(next) if !c.contains(&next) => {
            c.push(next.clone());
            OccurrenceStep::Add(next)
        }
        _ => OccurrenceStep::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_at_finds_the_identifier() {
        let t = "select user_id, $x from t";
        assert_eq!(word_at(t, 9), Some(7..14));
        assert_eq!(word_at(t, 7), Some(7..14), "at the start");
        assert_eq!(word_at(t, 14), Some(7..14), "right after the word");
        assert_eq!(word_at(t, 16), Some(16..18), "dollar is a word char");
    }

    #[test]
    fn word_at_none_between_non_words() {
        assert_eq!(word_at("a  b", 2), None);
        assert_eq!(word_at("", 0), None);
        assert_eq!(word_at("abc", 9), None);
        assert_eq!(word_at("(a)", 0), None);
    }

    #[test]
    fn word_at_multibyte() {
        let t = "select żółć from t";
        let start = 7;
        let end = start + "żółć".len();
        assert_eq!(word_at(t, start + 2), Some(start..end));
        assert_eq!(word_at(t, start + 1), None, "inside a char");
    }

    #[test]
    fn next_occurrence_finds_and_wraps() {
        let t = "ab cd ab ef ab";
        assert_eq!(next_occurrence(t, "ab", 2), Some(6..8));
        assert_eq!(next_occurrence(t, "ab", 9), Some(12..14));
        assert_eq!(next_occurrence(t, "ab", 13), Some(0..2), "wraps");
        assert_eq!(next_occurrence(t, "ab", 6), Some(6..8), "at or after");
    }

    #[test]
    fn next_occurrence_no_match() {
        assert_eq!(next_occurrence("abc", "zz", 0), None);
        assert_eq!(next_occurrence("abc", "", 0), None);
        assert_eq!(next_occurrence("żabc", "abc", 1), None, "not a boundary");
        assert_eq!(next_occurrence("abc", "abc", 99), Some(0..3));
    }

    #[test]
    fn step_selects_the_word_when_the_selection_is_empty() {
        let mut chain = None;
        let t = "foo bar foo";
        assert_eq!(step(&mut chain, t, &(1..1)), OccurrenceStep::Select(0..3));
        assert_eq!(step(&mut chain, t, &(3..3)), OccurrenceStep::Select(0..3));
        assert_eq!(step(&mut chain, t, &(4..4)), OccurrenceStep::Select(4..7));
        assert_eq!(step(&mut chain, "a  b", &(2..2)), OccurrenceStep::Nothing);
    }

    #[test]
    fn step_walks_the_matches_and_stops_at_the_wrap() {
        let mut chain = None;
        let t = "foo bar foo baz foo";
        assert_eq!(step(&mut chain, t, &(0..3)), OccurrenceStep::Add(8..11));
        // The editor's active selection is now the added range.
        assert_eq!(step(&mut chain, t, &(8..11)), OccurrenceStep::Add(16..19));
        assert_eq!(step(&mut chain, t, &(16..19)), OccurrenceStep::Nothing);
        assert_eq!(chain.as_ref().map(OccurrenceChain::len), Some(3));
    }

    #[test]
    fn step_restarts_when_the_text_or_needle_changes() {
        let mut chain = None;
        assert_eq!(
            step(&mut chain, "ab ab", &(0..2)),
            OccurrenceStep::Add(3..5)
        );
        // The text changed: a fresh chain from the new selection.
        assert_eq!(
            step(&mut chain, "ab ab ab", &(0..2)),
            OccurrenceStep::Add(3..5)
        );
        assert_eq!(chain.as_ref().map(OccurrenceChain::len), Some(2));
        // Another needle.
        assert_eq!(
            step(&mut chain, "ab ab ab", &(0..1)),
            OccurrenceStep::Add(3..4)
        );
    }

    #[test]
    fn step_with_a_single_match_does_nothing() {
        let mut chain = None;
        assert_eq!(
            step(&mut chain, "only one", &(0..4)),
            OccurrenceStep::Nothing
        );
    }

    #[test]
    fn step_resets_the_chain_on_an_empty_selection() {
        let mut chain = None;
        let _ = step(&mut chain, "ab ab", &(0..2));
        assert!(chain.is_some());
        let _ = step(&mut chain, "ab ab", &(0..0));
        assert!(chain.is_none());
    }
}
