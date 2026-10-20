// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Heuristic removal of credentials from SQL text before it is stored.

use std::borrow::Cow;

const PLACEHOLDER: &str = "'<redacted>'";
/// Lowercase fragments that mark a statement as sensitive. They are searched as
/// substrings in the raw text with whitespace removed, so no lexing is needed
/// and a marker inside a comment, a literal or an identifier counts too.
/// The list is deliberately broad: over-redaction is harmless.
const MARKERS: [&str; 33] = [
    "pass",
    "pwd",
    "secret",
    "token",
    "credential",
    "apikey",
    "api_key",
    "api-key",
    "auth",
    "bearer",
    "identified",
    "privatekey",
    "private_key",
    "-----begin",
    "key=",
    "key:",
    "key\"",
    "key'",
    "key`",
    "_key",
    "ssl",
    "salt",
    "hash",
    "cookie",
    "signature",
    "jwt",
    "sessionid",
    "session_id",
    "otp",
    "access_key",
    "accesskey",
    "keyfile",
    "cert",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Plain SQL, including bare words.
    Text,
    /// A `'...'`, `"..."` or dollar-quoted literal.
    Literal,
    /// A backtick identifier, closed or not.
    Ident,
    /// A comment.
    Comment,
}

/// One reading of the ambiguous parts of SQL syntax. The redactor lexes the
/// statement under every combination and acts on the union.
#[derive(Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // independent syntax switches, enumerated exhaustively
struct Mode {
    /// `\` escapes the next character inside quotes (`MySQL`, `E'..'`).
    backslash: bool,
    /// `/* */` comments nest (`PostgreSQL`).
    nesting: bool,
    /// `#` starts a line comment (`MySQL`).
    hash: bool,
    /// `--` starts a line comment even without a following space.
    dashes: bool,
    /// `$tag$ ... $tag$` is a literal (`PostgreSQL`).
    dollar: bool,
}

fn modes() -> impl Iterator<Item = Mode> {
    (0..32_u8).map(|n| Mode {
        backslash: n & 1 != 0,
        nesting: n & 2 != 0,
        hash: n & 4 != 0,
        dashes: n & 8 != 0,
        dollar: n & 16 != 0,
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
            b'-' if mode.dashes && b.get(i + 1) == Some(&b'-') => {
                Some((Kind::Comment, line_end(sql, i)))
            }
            b'#' if mode.hash => Some((Kind::Comment, line_end(sql, i))),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                Some((Kind::Comment, block_comment_end(b, i, mode.nesting)))
            }
            b'\'' | b'"' => Some((Kind::Literal, quoted_end(sql, i, c, mode.backslash).0)),
            b'`' => Some((Kind::Ident, quoted_end(sql, i, c, false).0)),
            b'$' if mode.dollar && !prev_word => dollar_end(sql, i).map(|end| (Kind::Literal, end)),
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
/// An unterminated run ends at the end of the text; the flag tells whether the
/// run was closed.
fn quoted_end(sql: &str, start: usize, q: u8, backslash: bool) -> (usize, bool) {
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
            return (i + 1, true);
        }
        i += 1;
    }
    (b.len(), false)
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

/// Whether the raw text holds a credential marker anywhere.
fn is_sensitive(sql: &str) -> bool {
    let squashed: String = sql
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    // A URL with user info (`scheme://user:pw@host`) needs no marker word.
    (squashed.contains("://") && squashed.contains('@'))
        || MARKERS.iter().any(|m| squashed.contains(m))
}

/// Removes literal values from a statement that may carry a credential.
///
/// Fail-safe by construction. A statement is sensitive when its raw text
/// (whitespace removed, any case) contains one of a broad list of markers as a
/// substring: `pass`, `pwd`, `secret`, `token`, `credential`, `apikey`,
/// `api_key`, `auth`, `bearer`, `identified`, `key=`, `_key`, `ssl`, ...
/// anywhere, including inside comments, identifiers and literals. No lexing is
/// involved in that decision.
///
/// In a sensitive statement every `'...'`, `"..."` and dollar-quoted literal,
/// every comment (including `MySQL` `/*! ... */` executable comments) and every
/// backtick identifier that is never closed becomes `'<redacted>'`. The text is
/// lexed under every combination of the readings that differ between dialects
/// (backslash escapes, nesting block comments, `#` and bare `--` comments,
/// dollar quoting) and the union of the ranges is redacted. An unterminated
/// construct in any reading hides everything to the end of the statement.
/// Literals holding JSON, YAML, `key=value` or URL credentials are redacted
/// whole because all literals are. Other statements are returned unchanged
/// (`Cow::Borrowed`).
///
/// It over-redacts by design. Not covered: an unquoted secret such as Oracle
/// `IDENTIFIED BY pw`, and secrets that are not in a literal, comment or open
/// identifier, such as hashes written as bare hex (`MSSQL` `0x...`).
#[must_use]
pub fn redact_credentials(sql: &str) -> Cow<'_, str> {
    if !is_sensitive(sql) {
        return Cow::Borrowed(sql);
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for mode in modes() {
        for (kind, s, e) in lex(sql, mode) {
            if kind != Kind::Text {
                ranges.push((s, e));
            }
        }
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
        assert!(r("ATTACH 'postgres://u@h/d'").contains(PLACEHOLDER));
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
    fn marker_anywhere_makes_the_statement_sensitive() {
        for s in [
            "select 1 -- password\n, 'a'",
            "select /* secret */ 'a'",
            "select 'password is secret'",
            "select \"password\", `secret` from t where a = 'x'",
            "select passwords, secret1 from t where a = 'x'",
            "select $$ password $$",
        ] {
            assert!(r(s).contains(PLACEHOLDER), "{s}");
        }
    }

    #[test]
    fn statements_without_a_marker_are_unchanged() {
        for s in [
            "select 1 -- note\n, 'a'",
            "select /* c */ 'a' from `t`",
            "select \"a\", `b` from t where c = 'x'",
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
            "alter role a password '<redacted>'"
        );
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

    #[test]
    fn unterminated_backtick_hides_the_rest() {
        let out = r("select ` ; ALTER USER u PASSWORD 'hunter2'");
        assert!(!leaks(&out), "{out}");
        let out = r("select `a``b ; ALTER USER u PASSWORD 'hunter2'");
        assert!(!leaks(&out), "{out}");
    }

    #[test]
    fn executable_comments_are_redacted() {
        for s in [
            "/*!50000 CREATE USER u IDENTIFIED BY 'hunter2' */",
            "/*!CREATE USER u IDENTIFIED BY 'hunter2'*/ select 1",
            "/*M!100100 ALTER USER u IDENTIFIED BY 'hunter2' */",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn broad_credential_names_are_sensitive() {
        for k in [
            "access_token",
            "apikey",
            "AWS_SECRET_KEY",
            "client_secret",
            "passwd",
            "sslkey",
            "ssl_cert",
            "refresh_token",
            "x-api-key",
            "private key",
            "Credentials",
        ] {
            let out = r(&format!("SET {k} = 'hunter2'"));
            assert!(!leaks(&out), "{k} -> {out}");
        }
    }

    #[test]
    fn structured_literals_are_redacted() {
        for s in [
            r#"SELECT '{"password":"hunter2"}'"#,
            "SELECT 'password: hunter2'",
            "SELECT 'db:\n  pwd: hunter2'",
            "SELECT 'postgres://u:p/ss@h/d'",
            "SELECT 'mysql://u:hunter2@h'",
            r#"SELECT '{"a":{"api_key":"hunter2"}}'"#,
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn bare_dashes_and_missing_dollar_quotes_are_read_both_ways() {
        for s in [
            "alter user u password --'hunter2'\n",
            "alter user u password --'\n'hunter2'",
            "alter user u password $a$ x $b$ hunter2 $a$",
            "alter user u password $a$ x' hunter2",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    const SECRET: &str = "S3cr3tXYZ";

    /// Small deterministic generator (xorshift); no extra dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
            pool[usize::try_from(self.next() % u64::try_from(pool.len()).unwrap()).unwrap()]
        }
    }

    /// Complete, unambiguous constructs: they do not change how a later
    /// secret is read.
    const SAFE: [&str; 12] = [
        " ",
        "select ",
        "alter user u ",
        ";",
        "'x'",
        "\"x\"",
        "`x`",
        " /* x */ ",
        " -- x\n",
        " $$x$$ ",
        "1, ",
        "from t ",
    ];
    /// Anything, including the dangerous fragments.
    const DANGER: [&str; 24] = [
        "'", "\"", "`", "\\", "/*", "*/", "--", "#", "$$", "$t$", "\n", " ", "E'", "/*!50000", ";",
        "''", "\\'", "/* /*", "'a\\'", "password", "$1", "é", "--'", "`a``",
    ];
    /// Places for the secret.
    const WRAPS: [&str; 15] = [
        "'S'",
        "\"S\"",
        "`S`",
        "/*S*/",
        "--S\n",
        "#S\n",
        "$$S$$",
        "/*!50000 CREATE USER u IDENTIFIED BY 'S' */",
        "E'\\'S'",
        "'\\'S'",
        "/* /* S */",
        "'S",
        "`S",
        "'{\"password\":\"S\"}'",
        "'u:p/ss@h S'",
    ];
    const SENSITIVE: [&str; 10] = [
        "PASSWORD",
        "identified by",
        "Access_Token",
        "apikey",
        "client_secret",
        "passwd",
        "AWS_SECRET_KEY",
        "/* pwd */",
        "'bearer'",
        "ssl_key",
    ];

    #[test]
    fn random_statements_never_leak_the_planted_secret() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..20_000 {
            let mut sql = String::new();
            for _ in 0..rng.next() % 4 {
                sql.push_str(rng.pick(&SAFE));
            }
            sql.push(' ');
            sql.push_str(rng.pick(&SENSITIVE));
            sql.push(' ');
            for _ in 0..rng.next() % 3 {
                sql.push_str(rng.pick(&SAFE));
            }
            sql.push_str(&rng.pick(&WRAPS).replace('S', SECRET));
            for _ in 0..rng.next() % 6 {
                sql.push_str(rng.pick(&DANGER));
            }
            let out = redact_credentials(&sql);
            assert!(!out.contains(SECRET), "{sql:?} -> {out:?}");
        }
    }

    #[test]
    fn random_statements_without_markers_are_unchanged() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        for _ in 0..5_000 {
            let mut sql = String::new();
            for _ in 0..rng.next() % 8 {
                let f = rng.pick(&DANGER);
                if f != "password" {
                    sql.push_str(f);
                }
                sql.push_str(rng.pick(&SAFE));
            }
            assert!(
                matches!(redact_credentials(&sql), Cow::Borrowed(_)),
                "{sql:?}"
            );
        }
    }
}
