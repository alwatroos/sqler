// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Heuristic removal of credentials from SQL text before it is stored.

use std::borrow::Cow;

const PLACEHOLDER: &str = "'<redacted>'";
/// Bare words that mark a statement as carrying a credential.
const SENSITIVE_WORDS: [&str; 9] = [
    "PASSWORD",
    "IDENTIFIED",
    "SECRET",
    "SECRET_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "API_KEY",
    "TOKEN",
    "AUTHORIZATION",
    "BEARER",
];
/// Whitespace-free, lowercase fragments of a literal that mark a credential.
const LITERAL_MARKERS: [&str; 10] = [
    "password=",
    "pwd=",
    "secret=",
    "secret_key=",
    "aws_secret_access_key=",
    "api_key=",
    "apikey=",
    "token=",
    "authorization:",
    "bearer",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Plain SQL, including bare words.
    Text,
    /// A `'...'`, `"..."` or dollar-quoted literal.
    Literal,
    /// A backtick identifier.
    Skipped,
    /// A comment.
    Comment,
}

/// One reading of the ambiguous parts of SQL syntax. The redactor lexes the
/// statement under every combination and acts on the union.
#[derive(Clone, Copy)]
struct Mode {
    /// `\` escapes the next character inside quotes (`MySQL`, `E'..'`).
    backslash: bool,
    /// `/* */` comments nest (`PostgreSQL`).
    nesting: bool,
    /// `#` starts a line comment (`MySQL`).
    hash: bool,
}

fn modes() -> impl Iterator<Item = Mode> {
    (0..8_u8).map(|n| Mode {
        backslash: n & 1 != 0,
        nesting: n & 2 != 0,
        hash: n & 4 != 0,
    })
}

/// Splits `sql` into `(kind, start, end)` segments covering the whole text.
fn lex(sql: &str, mode: Mode) -> Vec<(Kind, usize, usize)> {
    let b = sql.as_bytes();
    let mut segs = Vec::new();
    let mut text_start = 0;
    let mut i = 0;
    let mut prev_word = false;
    while i < b.len() {
        let c = b[i];
        let found = match c {
            b'-' if b.get(i + 1) == Some(&b'-') => Some((Kind::Comment, line_end(sql, i))),
            b'#' if mode.hash => Some((Kind::Comment, line_end(sql, i))),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                Some((Kind::Comment, block_comment_end(b, i, mode.nesting)))
            }
            b'\'' | b'"' => Some((Kind::Literal, quoted_end(sql, i, c, mode.backslash))),
            b'`' => Some((Kind::Skipped, quoted_end(sql, i, c, false))),
            b'$' if !prev_word => dollar_end(sql, i).map(|end| (Kind::Literal, end)),
            _ => None,
        };
        if let Some((kind, end)) = found {
            if text_start < i {
                segs.push((Kind::Text, text_start, i));
            }
            segs.push((kind, i, end));
            text_start = end;
            i = end;
            prev_word = false;
        } else {
            prev_word = is_word_byte(c);
            i += 1;
        }
    }
    if text_start < b.len() {
        segs.push((Kind::Text, text_start, b.len()));
    }
    segs
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn line_end(sql: &str, start: usize) -> usize {
    sql[start..].find('\n').map_or(sql.len(), |n| start + n)
}

/// End (exclusive) of a `/* ... */` comment starting at `start`.
fn block_comment_end(b: &[u8], start: usize, nesting: bool) -> usize {
    let mut depth = 0_usize;
    let mut i = start;
    while i < b.len() {
        if nesting && b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if !nesting && i == start {
            depth = 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// End (exclusive) of a quoted run starting at `start`, with `q` doubled as an
/// escape and, when `backslash` is set, `\` escaping the next character.
/// An unterminated run ends at the end of the text.
fn quoted_end(sql: &str, start: usize, q: u8, backslash: bool) -> usize {
    let b = sql.as_bytes();
    let mut i = start + 1;
    while i < b.len() {
        if backslash && b[i] == b'\\' {
            i += 2;
            while i < b.len() && !sql.is_char_boundary(i) {
                i += 1;
            }
            continue;
        }
        if b[i] == q {
            if b.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    b.len()
}

/// End (exclusive) of a `$tag$ ... $tag$` literal starting at `start`, or
/// `None` when `$` does not open one (for example `$1`).
fn dollar_end(sql: &str, start: usize) -> Option<usize> {
    let rest = &sql[start + 1..];
    let tag_len = rest.find('$')?;
    let tag = &rest[..tag_len];
    let mut chars = tag.chars();
    let tag_ok = match chars.next() {
        None => true,
        Some(f) => {
            (f.is_ascii_alphabetic() || f == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
    };
    if !tag_ok {
        return None;
    }
    let open_end = start + 1 + tag_len + 1;
    let delim = &sql[start..open_end];
    Some(
        sql[open_end..]
            .find(delim)
            .map_or(sql.len(), |n| open_end + n + delim.len()),
    )
}

fn has_sensitive_word(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .any(|w| SENSITIVE_WORDS.iter().any(|s| w.eq_ignore_ascii_case(s)))
}

/// Whether the literal text looks like a connection string or URL with a credential.
fn literal_has_credential(lit: &str) -> bool {
    let squashed: String = lit
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if LITERAL_MARKERS.iter().any(|m| squashed.contains(m)) {
        return true;
    }
    let mut rest = lit;
    while let Some(n) = rest.find("://") {
        rest = &rest[n + 3..];
        let authority = rest
            .split(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
            .next()
            .unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@')
            && let Some((_, pw)) = userinfo.split_once(':')
            && !pw.is_empty()
        {
            return true;
        }
    }
    false
}

/// Removes literal values from a statement that appears to carry a credential.
///
/// A statement is sensitive when a bare word outside quotes and comments is
/// `PASSWORD`, `IDENTIFIED`, `SECRET`, `SECRET_KEY`, `AWS_SECRET_ACCESS_KEY`,
/// `API_KEY`, `TOKEN`, `AUTHORIZATION` or `BEARER` (any case), or when a
/// literal holds `password=`, `pwd=`, a similar key or a URL with a password.
/// Then every `'...'`, `"..."` and dollar-quoted literal and every comment
/// becomes `'<redacted>'`; the rest of the text is kept.
///
/// The statement is lexed under every combination of the readings that differ
/// between dialects (backslash escapes, nesting block comments, `#` comments).
/// It is sensitive if any reading says so, and the union of the literal and
/// comment ranges of all readings is redacted, so an ambiguous quote hides
/// everything up to where any reading ends it (to the end of the statement
/// when unterminated). Other statements are returned unchanged
/// (`Cow::Borrowed`).
///
/// This is a heuristic: it can over-redact, and a credential it does not
/// recognise stays in the text.
#[must_use]
pub fn redact_credentials(sql: &str) -> Cow<'_, str> {
    let mut sensitive = false;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for mode in modes() {
        for (kind, s, e) in lex(sql, mode) {
            match kind {
                Kind::Text => sensitive |= has_sensitive_word(&sql[s..e]),
                Kind::Literal => {
                    sensitive |= literal_has_credential(&sql[s..e]);
                    ranges.push((s, e));
                }
                Kind::Comment => ranges.push((s, e)),
                Kind::Skipped => {}
            }
        }
    }
    if !sensitive {
        return Cow::Borrowed(sql);
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(sql.len());
    let mut pos = 0;
    let mut cur: Option<(usize, usize)> = None;
    for (s, e) in ranges {
        match cur {
            Some((cs, ce)) if s <= ce => cur = Some((cs, ce.max(e))),
            Some((cs, ce)) => {
                out.push_str(&sql[pos..cs]);
                out.push_str(PLACEHOLDER);
                pos = ce;
                cur = Some((s, e));
            }
            None => cur = Some((s, e)),
        }
    }
    if let Some((cs, ce)) = cur {
        out.push_str(&sql[pos..cs]);
        out.push_str(PLACEHOLDER);
        pos = ce;
    }
    out.push_str(&sql[pos..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact_credentials(s).into_owned()
    }

    #[test]
    fn alter_user_password() {
        assert_eq!(
            r("ALTER USER x PASSWORD 'hunter2'"),
            "ALTER USER x PASSWORD '<redacted>'"
        );
    }

    #[test]
    fn identified_by() {
        assert_eq!(
            r("create user u identified by 'p'"),
            "create user u identified by '<redacted>'"
        );
    }

    #[test]
    fn mssql_with_password() {
        assert_eq!(
            r("CREATE LOGIN a WITH PASSWORD = 'p', DEFAULT_DATABASE = 'd'"),
            "CREATE LOGIN a WITH PASSWORD = '<redacted>', DEFAULT_DATABASE = '<redacted>'"
        );
    }

    #[test]
    fn duckdb_secret() {
        assert_eq!(
            r("CREATE SECRET s (TYPE s3, KEY_ID 'k', SECRET 'y')"),
            "CREATE SECRET s (TYPE s3, KEY_ID '<redacted>', SECRET '<redacted>')"
        );
    }

    #[test]
    fn url_userinfo_password() {
        assert_eq!(
            r("ATTACH 'postgres://u:pw@h/d' AS x"),
            "ATTACH '<redacted>' AS x"
        );
        assert!(matches!(
            redact_credentials("ATTACH 'postgres://u@h/d'"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn connection_string_password() {
        assert_eq!(
            r("ATTACH 'host=h Password = x dbname=d'"),
            "ATTACH '<redacted>'"
        );
        assert_eq!(r("select 'PWD=1'"), "select '<redacted>'");
    }

    #[test]
    fn keyword_in_comment_or_literal_or_identifier_is_ignored() {
        for s in [
            "select 1 -- password\n, 'a'",
            "select /* secret */ 'a'",
            "select 'password is secret'",
            "select \"password\", `secret` from t where a = 'x'",
            "select passwords, secret1 from t",
        ] {
            assert!(matches!(redact_credentials(s), Cow::Borrowed(_)), "{s}");
        }
    }

    #[test]
    fn doubled_quote_escape() {
        assert_eq!(
            r("alter user a password 'it''s secret'"),
            "alter user a password '<redacted>'"
        );
    }

    #[test]
    fn dollar_quoted_literal() {
        assert_eq!(
            r("alter role a password $x$pw'$x$ ; select $$a$$, $1"),
            "alter role a password '<redacted>' ; select '<redacted>', $1"
        );
    }

    #[test]
    fn keyword_inside_dollar_quote_is_not_a_word() {
        assert!(matches!(
            redact_credentials("select $$ password $$"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn unchanged_statement_is_borrowed() {
        let s = "select 'x' from t where a = $1";
        assert!(matches!(redact_credentials(s), Cow::Borrowed(b) if b == s));
    }

    #[test]
    fn unterminated_literal_is_redacted_to_the_end() {
        assert_eq!(
            r("alter user a password 'abc"),
            "alter user a password '<redacted>'"
        );
    }

    #[test]
    fn non_ascii_text_is_kept() {
        assert_eq!(
            r("alter user é password 'ü'"),
            "alter user é password '<redacted>'"
        );
    }

    fn leaks(out: &str) -> bool {
        out.contains("hunter2")
    }

    #[test]
    fn double_quoted_strings_are_redacted() {
        assert!(!leaks(&r(r#"CREATE USER u IDENTIFIED BY "hunter2""#)));
        assert!(!leaks(&r(r#"SET PASSWORD FOR u = "hunter2""#)));
    }

    #[test]
    fn backslash_escaped_quote_does_not_leak() {
        assert!(!leaks(&r(r"ALTER USER u IDENTIFIED BY 'abc\'hunter2'")));
        assert!(!leaks(&r(r"ALTER USER u PASSWORD 'abc\'hunter2' ")));
    }

    #[test]
    fn credential_after_escaped_quote_is_found() {
        let out = r(r"SELECT 'a\'', 'pwd=hunter2'");
        assert!(!leaks(&out), "{out}");
    }

    #[test]
    fn non_nesting_comment_does_not_hide_a_password() {
        let out = r("/* a /* b */ ALTER USER u IDENTIFIED BY 'hunter2'");
        assert!(!leaks(&out), "{out}");
    }

    #[test]
    fn e_strings_do_not_leak() {
        for s in [
            r"ALTER ROLE u PASSWORD E'x\'hunter2'",
            r"ALTER ROLE u PASSWORD e'\'hunter2\''",
            r"ALTER ROLE u PASSWORD E'a\\' || 'hunter2'",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn comments_in_sensitive_statements_are_redacted() {
        for s in [
            "ALTER USER u PASSWORD 'p' -- old was hunter2",
            "ALTER USER u PASSWORD 'p' # old was hunter2",
            "ALTER USER u PASSWORD 'p' /* hunter2 */",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn unterminated_ambiguous_quote_redacts_to_the_end() {
        let out = r(r"ALTER USER u PASSWORD 'a\' , hunter2 ; select 1");
        assert!(!leaks(&out), "{out}");
    }

    #[test]
    fn more_credential_keys_are_sensitive() {
        for s in [
            "SET aws_secret_access_key = 'hunter2'",
            "SET secret_key = 'hunter2'",
            "SET api_key = 'hunter2'",
            "SET token = 'hunter2'",
            "SET authorization = 'hunter2'",
            "SET bearer 'hunter2'",
            "ATTACH 'api_key=hunter2'",
            "ATTACH 'token=hunter2'",
            "ATTACH 'aws_secret_access_key=hunter2'",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn backslash_before_multibyte_char_does_not_panic() {
        let out = r("alter user a password 'x\\é' ü 'hunter2'");
        assert!(!leaks(&out), "{out}");
    }
}
