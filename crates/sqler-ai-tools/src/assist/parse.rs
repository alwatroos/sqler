// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Pulls the SQL and the rationale out of a model's answer.

use serde_json::Value;

use super::AssistError;

/// The longest rationale kept, in characters.
const MAX_RATIONALE_CHARS: usize = 600;

/// The first words that start bare SQL.
const KEYWORDS: [&str; 17] = [
    "select", "with", "insert", "update", "delete", "create", "alter", "drop", "explain", "values",
    "merge", "truncate", "replace", "pragma", "show", "describe", "call",
];

/// The SQL and the explanation found in an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParsedAnswer {
    /// The SQL text, trimmed.
    pub sql: String,
    /// The text around the SQL, trimmed and cut to 600 characters.
    pub rationale: String,
}

impl ParsedAnswer {
    fn new(sql: &str, rationale: &str) -> Option<Self> {
        let sql = sql.trim();
        if sql.is_empty() {
            return None;
        }
        Some(Self {
            sql: sql.to_owned(),
            rationale: cap_chars(rationale.trim(), MAX_RATIONALE_CHARS),
        })
    }
}

fn cap_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// Finds the SQL in `text`: a JSON object with `sql` (or `query`), else a fenced block, else
/// bare SQL starting at the first line that begins with a SQL keyword.
///
/// # Errors
/// [`AssistError::NoSql`] if none of these yields a statement.
pub fn parse_sql_answer(text: &str) -> Result<ParsedAnswer, AssistError> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    from_json(&text)
        .or_else(|| from_fence(&text))
        .or_else(|| from_bare(&text))
        .ok_or(AssistError::NoSql)
}

/// The SQL of an answer, if it has any. See [`parse_sql_answer`].
#[must_use]
pub fn extract_sql(text: &str) -> Option<String> {
    parse_sql_answer(text).ok().map(|p| p.sql)
}

fn json_object(text: &str) -> Option<serde_json::Map<String, Value>> {
    let whole = text.trim();
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(whole) {
        return Some(map);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    match serde_json::from_str::<Value>(text.get(start..=end)?) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn from_json(text: &str) -> Option<ParsedAnswer> {
    let map = json_object(text)?;
    let field = |keys: [&str; 2]| {
        keys.iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .unwrap_or_default()
    };
    ParsedAnswer::new(field(["sql", "query"]), field(["rationale", "explanation"]))
}

struct Block {
    info: String,
    first: usize,
    last: usize,
    body: String,
}

/// The run length of `ch` at the start of `line` (after indentation), if at least 3.
fn fence_run(line: &str, ch: char) -> Option<usize> {
    let run = line.trim_start().chars().take_while(|c| *c == ch).count();
    (run >= 3).then_some(run)
}

fn blocks(lines: &[&str]) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let opener = ['`', '~']
            .into_iter()
            .find_map(|ch| fence_run(line, ch).map(|n| (ch, n)));
        let Some((ch, run)) = opener else {
            i += 1;
            continue;
        };
        let info = line.trim_start().trim_start_matches(ch).trim().to_owned();
        let mut body = Vec::new();
        let mut j = i + 1;
        let mut closed = false;
        while j < lines.len() {
            let candidate = lines[j];
            if fence_run(candidate, ch).is_some_and(|n| n >= run)
                && candidate
                    .trim_start()
                    .trim_start_matches(ch)
                    .trim()
                    .is_empty()
            {
                closed = true;
                break;
            }
            body.push(candidate);
            j += 1;
        }
        let last = if closed {
            j
        } else {
            lines.len().saturating_sub(1)
        };
        out.push(Block {
            info,
            first: i,
            last,
            body: body.join("\n"),
        });
        i = last + 1;
    }
    out
}

fn around(lines: &[&str], first: usize, last: usize) -> String {
    let before = lines.get(..first).unwrap_or_default();
    let after = lines.get(last + 1..).unwrap_or_default();
    before
        .iter()
        .chain(after.iter())
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn from_fence(text: &str) -> Option<ParsedAnswer> {
    let lines: Vec<&str> = text.lines().collect();
    let found = blocks(&lines);
    let usable = |b: &&Block| !b.body.trim().is_empty();
    let chosen = found
        .iter()
        .filter(usable)
        .find(|b| b.info.to_ascii_lowercase().contains("sql"))
        .or_else(|| found.iter().filter(usable).find(|b| b.info.is_empty()))?;
    ParsedAnswer::new(&chosen.body, &around(&lines, chosen.first, chosen.last))
}

/// The line without a leading `SQL:` label, if it starts SQL.
fn sql_start(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    if rest.len() >= 4
        && rest
            .get(..4)
            .is_some_and(|l| l.eq_ignore_ascii_case("sql:"))
    {
        rest = rest[4..].trim_start();
    }
    let word: String = rest
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    let boundary = rest[word.len()..]
        .chars()
        .next()
        .is_none_or(|c| c.is_whitespace() || c == '(' || c == '*');
    (KEYWORDS.contains(&word.as_str()) && boundary).then_some(rest)
}

fn from_bare(text: &str) -> Option<ParsedAnswer> {
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.iter().position(|l| sql_start(l).is_some())?;
    let mut last = first;
    while lines.get(last + 1).is_some_and(|l| !l.trim().is_empty()) {
        last += 1;
    }
    let mut sql = vec![sql_start(lines[first])?];
    sql.extend(lines.get(first + 1..=last).unwrap_or_default());
    ParsedAnswer::new(&sql.join("\n"), &around(&lines, first, last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_whole_and_embedded() {
        let a = parse_sql_answer(r#"{"sql":"SELECT 1","rationale":"one"}"#).unwrap();
        assert_eq!((a.sql.as_str(), a.rationale.as_str()), ("SELECT 1", "one"));
        let a = parse_sql_answer("Here:\n{\"query\":\"SELECT 2\",\"explanation\":\"two\"}\nbye")
            .unwrap();
        assert_eq!((a.sql.as_str(), a.rationale.as_str()), ("SELECT 2", "two"));
    }

    #[test]
    fn json_in_a_fence() {
        let a = parse_sql_answer("```json\n{\"sql\": \"SELECT 3\"}\n```").unwrap();
        assert_eq!(a.sql, "SELECT 3");
    }

    #[test]
    fn fence_with_prose_before_and_after() {
        let a = parse_sql_answer("Sure.\n```sql\nSELECT id\nFROM t;\n```\nIt lists ids.").unwrap();
        assert_eq!(a.sql, "SELECT id\nFROM t;");
        assert_eq!(a.rationale, "Sure.\nIt lists ids.");
    }

    #[test]
    fn crlf_is_normalised() {
        let a = parse_sql_answer("```sql\r\nSELECT 1\r\n```\r\ndone").unwrap();
        assert_eq!(a.sql, "SELECT 1");
        assert_eq!(a.rationale, "done");
    }

    #[test]
    fn two_fences_prefer_the_sql_one() {
        let a = parse_sql_answer("```text\nnot sql\n```\n~~~postgresql\nSELECT 4\n~~~").unwrap();
        assert_eq!(a.sql, "SELECT 4");
        let a = parse_sql_answer("```\nSELECT 5\n```\n```sql\nSELECT 6\n```").unwrap();
        assert_eq!(a.sql, "SELECT 6");
    }

    #[test]
    fn unclosed_fence_runs_to_the_end() {
        let a = parse_sql_answer("```sql\nSELECT 7\nFROM t").unwrap();
        assert_eq!(a.sql, "SELECT 7\nFROM t");
    }

    #[test]
    fn bare_sql_up_to_a_blank_line() {
        let a = parse_sql_answer("The query:\nSQL: select a\nfrom b\n\nIt reads b.").unwrap();
        assert_eq!(a.sql, "select a\nfrom b");
        assert_eq!(a.rationale, "The query:\n\nIt reads b.");
    }

    #[test]
    fn no_sql() {
        assert!(matches!(
            parse_sql_answer("I cannot help with that."),
            Err(AssistError::NoSql)
        ));
        assert!(matches!(
            parse_sql_answer("```sql\n\n```"),
            Err(AssistError::NoSql)
        ));
        assert!(matches!(
            parse_sql_answer(r#"{"sql":""}"#),
            Err(AssistError::NoSql)
        ));
        assert_eq!(extract_sql(""), None);
        assert_eq!(extract_sql("selection of things"), None);
    }

    #[test]
    fn rationale_is_capped_on_a_char_boundary() {
        let long = "é".repeat(700);
        let a = parse_sql_answer(&format!("```sql\nSELECT 1\n```\n{long}")).unwrap();
        assert_eq!(a.rationale.chars().count(), MAX_RATIONALE_CHARS);
    }
}
