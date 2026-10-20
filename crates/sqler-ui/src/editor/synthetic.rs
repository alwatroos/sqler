// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A large synthetic SQL script for benchmarks and the "is it responsive" check.

/// One block of lines; the number of the block varies the names.
fn block(i: usize) -> Vec<String> {
    match i % 5 {
        0 => vec![format!(
            "SELECT id, name, 'it''s -- not a comment' AS note FROM table_{i} WHERE (a > 1 AND (b < 2 OR c IN (1, 2, 3)));"
        )],
        1 => vec![
            format!("CREATE TABLE IF NOT EXISTS table_{i} ("),
            "  id INTEGER PRIMARY KEY,".to_owned(),
            "  name VARCHAR(64) NOT NULL DEFAULT '(none)',".to_owned(),
            "  amount NUMERIC(10, 2) CHECK (amount >= 0)".to_owned(),
            ");".to_owned(),
        ],
        2 => vec![format!(
            "INSERT INTO table_{i} (id, name) VALUES ({i}, 'a /* b */ c'), ({}, \"quoted [name]\");",
            i + 1
        )],
        3 => vec![
            "/* a block comment with (parens, [brackets] and 'quotes'".to_owned(),
            "   that spans several lines */".to_owned(),
            format!(
                "UPDATE table_{i} SET name = 'x' WHERE id IN (SELECT id FROM other WHERE (flag = 1)); -- trailing ( comment"
            ),
        ],
        _ => vec![
            format!("SELECT t.id, COUNT(*) AS n FROM table_{i} t"),
            "  JOIN other o ON (o.id = t.id AND o.kind = 'k')".to_owned(),
            "  WHERE t.name LIKE '%x%' GROUP BY t.id ORDER BY n DESC;".to_owned(),
        ],
    }
}

/// A script of exactly `lines` lines (each ending with `\n`), mixing SELECT, INSERT, CREATE and UPDATE statements
/// with strings, comments and nested brackets. Every statement is complete.
#[must_use]
pub fn script(lines: usize) -> String {
    let mut out = String::with_capacity(lines.saturating_mul(60));
    let mut written = 0;
    let mut i = 0;
    while written < lines {
        let b = block(i);
        if written + b.len() <= lines {
            for l in &b {
                out.push_str(l);
                out.push('\n');
            }
            written += b.len();
        } else {
            // Too little room left for a multi-line block: fill with one-line statements.
            out.push_str("SELECT 1; -- filler\n");
            written += 1;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_exactly_the_requested_lines() {
        for n in [0, 1, 2, 7, 100, 10_000] {
            assert_eq!(script(n).lines().count(), n, "{n}");
        }
    }

    #[test]
    fn ends_with_a_newline_and_is_deterministic() {
        let s = script(50);
        assert!(s.ends_with('\n'));
        assert_eq!(s, script(50));
    }

    #[test]
    fn brackets_pair_up() {
        let s = script(10_000);
        let idx = super::super::BracketIndex::build(Some(sqler_sql::Dialect::Postgres), &s);
        assert!(idx.len() > 5_000, "{}", idx.len());
        // Strings and comments hide their brackets: every `(` the script writes in code pairs up.
        let code_opens = s.matches('(').count();
        assert!(idx.len() <= code_opens);
    }

    #[test]
    fn formats_without_error() {
        let s = script(500);
        sqler_sql::format(
            sqler_sql::Dialect::Postgres,
            &s,
            &sqler_sql::FormatOptions::default(),
        )
        .unwrap();
    }
}
