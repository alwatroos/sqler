// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The completion popup as plain data: what the editor lists, where it inserts, and what to load.

use std::ops::Range;

use sqler_catalog::CacheKey;
use sqler_completion::{CompletionRequest, InsertFormat, ItemKind, complete};
use sqler_sql::Dialect;

use super::COMPLETION_LIMIT;
use super::keys::keys_for;
use super::schema::CatalogSchema;

/// One row of the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupItem {
    /// The text shown and filtered on.
    pub label: String,
    /// What it is.
    pub kind: ItemKind,
    /// The text inserted in place of [`Popup::replace`]; snippets are flattened.
    pub insert_text: String,
    /// Short extra information.
    pub detail: Option<String>,
    /// Sorts by the engine's order, as text.
    pub sort_text: String,
}

/// What a completion request answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    /// The items, best first.
    pub items: Vec<PopupItem>,
    /// The byte range of the text that the chosen item replaces.
    pub replace: Range<usize>,
    /// The catalog loads that would complete the answer; request the popup again when they are done.
    pub loads: Vec<CacheKey>,
}

/// Completes `text` at the byte `offset` for a connection of `dialect`, reading `schema`.
#[must_use]
pub fn complete_popup(
    text: &str,
    offset: usize,
    dialect: Dialect,
    schema: &CatalogSchema,
) -> Popup {
    let mut request = CompletionRequest::new(text, offset, dialect);
    request.limit = COMPLETION_LIMIT;
    let done = complete(&request, schema);
    let items = done
        .items
        .into_iter()
        .enumerate()
        .map(|(i, item)| PopupItem {
            insert_text: match item.format {
                InsertFormat::Snippet => flatten_snippet(&item.insert_text),
                InsertFormat::Plain => item.insert_text,
            },
            label: item.label,
            kind: item.kind,
            detail: item.detail,
            sort_text: format!("{i:05}"),
        })
        .collect();
    Popup {
        items,
        replace: done.replace,
        loads: keys_for(&done.requests, schema),
    }
}

/// Whether typing `new_text` should open the popup: exactly one character that is alphanumeric, `_`, `$` or `.`.
#[must_use]
pub fn is_completion_trigger(new_text: &str) -> bool {
    let mut chars = new_text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_alphanumeric() || matches!(c, '_' | '$' | '.'),
        _ => false,
    }
}

fn fold_eq(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// The byte ranges of `label` to highlight for the typed `query`.
///
/// Only the part of the query after its last `.` counts, without leading quote characters. The match is a
/// case-insensitive subsequence in order; adjacent hits merge into one range. Empty unless the whole
/// query matches.
#[must_use]
pub fn highlight_ranges(query: &str, label: &str) -> Vec<Range<usize>> {
    let tail = query.rsplit('.').next().unwrap_or(query);
    let tail = tail.trim_start_matches(['"', '`', '[']);
    let mut wanted = tail.chars().peekable();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, c) in label.char_indices() {
        let Some(&q) = wanted.peek() else { break };
        if !fold_eq(c, q) {
            continue;
        }
        wanted.next();
        let end = start + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    if wanted.peek().is_some() {
        return Vec::new();
    }
    ranges
}

/// Reduces an LSP snippet to plain text: `${n:x}` becomes `x`, `$n` and `$0` vanish, `\$`, `\}` and `\\` unescape.
///
/// The editor has no snippet engine, so tab stops are dropped.
#[must_use]
pub fn flatten_snippet(snippet: &str) -> String {
    let mut out = String::with_capacity(snippet.len());
    let mut chars = snippet.chars().peekable();
    // Nesting depth of `${n:` placeholders we are inside.
    let mut depth = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(&next) if matches!(next, '$' | '}' | '\\') => {
                    out.push(next);
                    chars.next();
                }
                _ => out.push('\\'),
            },
            '$' => match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    while chars.next_if(char::is_ascii_digit).is_some() {}
                }
                Some('{') => {
                    chars.next();
                    let mut number = false;
                    while chars.next_if(char::is_ascii_digit).is_some() {
                        number = true;
                    }
                    if number && chars.next_if_eq(&':').is_some() {
                        depth += 1;
                    } else if number && chars.next_if_eq(&'}').is_some() {
                        // `${1}`: a bare tab stop.
                    } else {
                        // Not a tab stop: keep what was read as text.
                        out.push_str("${");
                    }
                }
                _ => out.push('$'),
            },
            '}' if depth > 0 => depth -= 1,
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_flatten_to_plain_text() {
        assert_eq!(flatten_snippet("COUNT(${1:*})$0"), "COUNT(*)");
        assert_eq!(
            flatten_snippet("SELECT $1 FROM ${2:table} WHERE $0"),
            "SELECT  FROM table WHERE "
        );
        assert_eq!(flatten_snippet("a${1}b$12c"), "abc");
        assert_eq!(flatten_snippet("plain"), "plain");
        assert_eq!(flatten_snippet(r"price \$5 \\ \} x"), r"price $5 \ } x");
        assert_eq!(flatten_snippet("${1:a ${2:b}}"), "a b");
        assert_eq!(flatten_snippet("cost$"), "cost$");
        assert_eq!(flatten_snippet("é${1:ü}"), "éü");
    }

    #[test]
    fn triggers_are_single_word_characters_or_dot() {
        for yes in ["a", "Z", "7", "_", "$", ".", "é"] {
            assert!(is_completion_trigger(yes), "{yes}");
        }
        for no in ["", " ", "(", ",", "ab", "..", "\n", "-"] {
            assert!(!is_completion_trigger(no), "{no:?}");
        }
    }

    #[test]
    fn highlights_a_case_insensitive_subsequence() {
        assert_eq!(highlight_ranges("na", "name"), vec![0..2]);
        assert_eq!(highlight_ranges("u.na", "name"), vec![0..2]);
        assert_eq!(highlight_ranges("NE", "name"), [0..1, 3..4]);
        assert_eq!(highlight_ranges("\"na", "name"), vec![0..2]);
        assert_eq!(highlight_ranges("`n", "name"), vec![0..1]);
        assert_eq!(highlight_ranges("[a", "name"), vec![1..2]);
        assert_eq!(highlight_ranges("", "name"), Vec::<Range<usize>>::new());
        assert_eq!(highlight_ranges("u.", "name"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn highlights_nothing_without_a_full_match() {
        assert_eq!(highlight_ranges("xyz", "name").len(), 0);
        assert_eq!(highlight_ranges("nax", "name").len(), 0);
        assert_eq!(highlight_ranges("mn", "name").len(), 0);
    }

    #[test]
    fn highlights_multibyte_labels_in_bytes() {
        assert_eq!(highlight_ranges("ü", "Über"), vec![0..2]);
        assert_eq!(highlight_ranges("be", "Über"), vec![2..4]);
        assert_eq!(highlight_ranges("ér", "café_r"), [3..5, 6..7]);
    }

    #[test]
    fn the_popup_keeps_the_engine_order() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("runtime");
        let (catalog, worker) = super::super::synthetic::synthetic_catalog(4, 3);
        rt.spawn(worker.run());
        rt.block_on(super::super::keys::load(
            &catalog,
            super::super::keys::bootstrap_keys(),
        ));
        let schema = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        let text = "SELECT * FROM t_0";
        let popup = complete_popup(text, text.len(), Dialect::Postgres, &schema);
        assert_eq!(&text[popup.replace.clone()], "t_0");
        let tables: Vec<_> = popup
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Table)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(tables, ["t_00000", "t_00001", "t_00002", "t_00003"]);
        let sorts: Vec<_> = popup.items.iter().map(|i| i.sort_text.clone()).collect();
        let mut sorted = sorts.clone();
        sorted.sort();
        assert_eq!(sorts, sorted);
        assert_eq!(sorts.first().map(String::as_str), Some("00000"));
        assert!(popup.items.iter().all(|i| !i.insert_text.contains('$')));
    }
}
