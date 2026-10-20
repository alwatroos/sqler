// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Fuzzy palette query over a [`CommandRegistry`].

use super::registry::{Command, CommandRegistry};

/// The text a match was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchField {
    /// The command title.
    Title,
    /// The command ID.
    Id,
}

/// A command matched by a palette query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteMatch<'a> {
    /// The matched command.
    pub command: &'a Command,
    /// Higher is better.
    pub score: u32,
    /// The text that matched.
    pub field: MatchField,
    /// Matched char indices into the title or ID named by `field`, for highlighting.
    pub positions: Vec<usize>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn is_word_start(field: &[char], i: usize) -> bool {
    i == 0 || matches!(field[i - 1], ' ' | '-' | ':' | '_' | '.')
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn find_contiguous(field: &[char], query: &[char], word_start_only: bool) -> Option<usize> {
    if query.len() > field.len() {
        return None;
    }
    (0..=field.len() - query.len()).find(|&s| {
        (!word_start_only || is_word_start(field, s)) && field[s..s + query.len()] == *query
    })
}

fn find_subsequence(field: &[char], query: &[char]) -> Option<Vec<usize>> {
    let mut positions = Vec::with_capacity(query.len());
    let mut q = query.iter().peekable();
    for (i, c) in field.iter().enumerate() {
        match q.peek() {
            Some(&&qc) if qc == *c => {
                positions.push(i);
                q.next();
            }
            Some(_) => {}
            None => break,
        }
    }
    q.peek().is_none().then_some(positions)
}

fn score_field(field: &str, query: &[char]) -> Option<(u32, Vec<usize>)> {
    let chars: Vec<char> = field.chars().map(lower).collect();
    let positions: Vec<usize> = if let Some(s) =
        find_contiguous(&chars, query, true).or_else(|| find_contiguous(&chars, query, false))
    {
        (s..s + query.len()).collect()
    } else {
        find_subsequence(&chars, query)?
    };
    let (&first, &last) = (positions.first()?, positions.last()?);
    let mut score = 0usize;
    let mut prev: Option<usize> = None;
    for &p in &positions {
        score += 1;
        if is_word_start(&chars, p) {
            score += 8;
        }
        if prev.is_some_and(|q| q + 1 == p) {
            score += 4;
        }
        prev = Some(p);
    }
    if first == 0 {
        score += 16;
    }
    let gap = (last - first + 1) - query.len();
    Some((to_u32(score).saturating_sub(to_u32(gap)), positions))
}

impl CommandRegistry {
    /// Ranks commands against a fuzzy `query` matched on title and ID.
    ///
    /// Matching ignores case and whitespace in the query. Results are sorted by score
    /// descending, then by ID. An empty query returns all commands in ID order with score 0.
    #[must_use]
    pub fn query(&self, query: &str) -> Vec<PaletteMatch<'_>> {
        let q: Vec<char> = query
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(lower)
            .collect();
        if q.is_empty() {
            return self
                .iter()
                .map(|command| PaletteMatch {
                    command,
                    score: 0,
                    field: MatchField::Title,
                    positions: Vec::new(),
                })
                .collect();
        }
        let mut out: Vec<PaletteMatch<'_>> = self
            .iter()
            .filter_map(|command| {
                let title = score_field(command.title(), &q);
                let id = score_field(command.id().as_str(), &q);
                let (score, positions, field) = match (title, id) {
                    (Some((ts, tp)), Some((is, ip))) => {
                        if is > ts {
                            (is, ip, MatchField::Id)
                        } else {
                            (ts, tp, MatchField::Title)
                        }
                    }
                    (Some((s, p)), None) => (s, p, MatchField::Title),
                    (None, Some((s, p))) => (s, p, MatchField::Id),
                    (None, None) => return None,
                };
                Some(PaletteMatch {
                    command,
                    score,
                    field,
                    positions,
                })
            })
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.score));
        out
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn fixture() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        for (id, title) in [
            ("query:run-selection", "Run Selection"),
            ("query:run-all", "Run All"),
            ("query:rerun-last", "Rerun Last"),
            ("result-grid:close-results", "Close Results"),
            ("connection:open-new", "New Connection"),
        ] {
            r.register(Command::new(id, title).unwrap()).unwrap();
        }
        r
    }

    fn ids<'a>(m: &[PaletteMatch<'a>]) -> Vec<&'a str> {
        m.iter().map(|m| m.command.id().as_str()).collect()
    }

    fn rank(m: &[PaletteMatch<'_>], id: &str) -> usize {
        ids(m).iter().position(|i| *i == id).unwrap()
    }

    #[test]
    fn word_start_beats_mid_word() {
        let r = fixture();
        let m = r.query("run");
        let rerun = rank(&m, "query:rerun-last");
        assert!(rank(&m, "query:run-all") < rerun);
        assert!(rank(&m, "query:run-selection") < rerun);
    }

    #[test]
    fn initials() {
        let r = fixture();
        let m = r.query("rs");
        assert_eq!(m[0].command.id().as_str(), "query:run-selection");
        assert!(rank(&m, "query:rerun-last") > 0);
    }

    #[test]
    fn case_and_whitespace_ignored() {
        let r = fixture();
        assert_eq!(
            r.query("RUN sel")[0].command.id().as_str(),
            "query:run-selection"
        );
    }

    #[test]
    fn matches_id() {
        let r = fixture();
        let m = r.query("qrs");
        let hit = m
            .iter()
            .find(|m| m.command.id().as_str() == "query:run-selection")
            .unwrap();
        assert_eq!(hit.field, MatchField::Id);
    }

    #[test]
    fn no_match() {
        assert_eq!(fixture().query("zzz").len(), 0);
    }

    #[test]
    fn empty_query_returns_all_in_id_order() {
        let r = fixture();
        let expected = [
            "connection:open-new",
            "query:rerun-last",
            "query:run-all",
            "query:run-selection",
            "result-grid:close-results",
        ];
        for q in ["", "   "] {
            let m = r.query(q);
            assert_eq!(ids(&m), expected);
            assert!(m.iter().all(|m| m.score == 0 && m.positions.is_empty()));
        }
    }

    #[test]
    fn ties_in_id_order() {
        let mut r = CommandRegistry::new();
        r.register(Command::new("query:run-b", "Same").unwrap())
            .unwrap();
        r.register(Command::new("query:run-a", "Same").unwrap())
            .unwrap();
        let m = r.query("same");
        assert_eq!(m[0].score, m[1].score);
        assert_eq!(ids(&m), ["query:run-a", "query:run-b"]);
    }

    #[test]
    fn positions() {
        let r = fixture();
        let m = r.query("run");
        let hit = m.iter().find(|m| m.command.title() == "Run All").unwrap();
        assert_eq!(hit.field, MatchField::Title);
        assert_eq!(hit.positions, [0, 1, 2]);
    }

    #[test]
    fn thousand_commands_under_one_frame() {
        let mut r = CommandRegistry::new();
        for i in 0..1000 {
            r.register(
                Command::new(
                    &format!("cat{}:run-thing{i}", i % 10),
                    format!("Run Thing {i}"),
                )
                .unwrap(),
            )
            .unwrap();
        }
        let start = Instant::now();
        let m = r.query("rth");
        let elapsed = start.elapsed();
        assert_ne!(m.len(), 0);
        assert!(elapsed < Duration::from_millis(16), "took {elapsed:?}");
    }
}
