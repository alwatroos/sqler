// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Heuristic removal of credentials from SQL text before it is stored.
//!
//! Conservative by design, not a guarantee: see [`redact_credentials`] for the
//! markers and the known gaps.

use std::borrow::Cow;

const PLACEHOLDER: &str = "'<redacted>'";

/// Lowercase fragments that mark a statement as sensitive. They are searched
/// as substrings of the lowercased raw text, token by token as written
/// (whitespace is not removed), so no lexing is needed and a marker inside a
/// comment, a literal or an identifier counts too. A fragment may sit inside
/// a longer word (`passw` matches `password_hash`). The list is deliberately
/// broad: over-redaction is harmless. Bare `auth`, `ssl`, `cert`, `otp` and
/// `hash` are not markers: they match too many harmless words (`author`,
/// `class`, `NOT price`).
const MARKERS: [&str; 36] = [
    "passw",
    "passphrase",
    "passcode",
    "pwd",
    "secret",
    "token",
    "credential",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "auth_",
    "authenticat",
    "oauth",
    "bearer",
    "identified",
    "private_key",
    "-----begin",
    "_key",
    "key_id",
    "keyid",
    "access_key",
    "accesskey",
    "keyfile",
    "sslkey",
    "sslcert",
    "ssl_cert",
    "salt",
    "cookie",
    "signature",
    "jwt",
    "sessionid",
    "session_id",
    "encrypt",
    "decrypt",
    "crypt(",
];

/// Markers made of several words. They are searched in the lowercased text
/// with all whitespace removed, so `private   key` and `API KEY` match.
const MULTI_WORD_MARKERS: [&str; 5] = [
    "privatekey",
    "apikey",
    "accesskey",
    "sessionid",
    "identifiedby",
];

/// Markers that count only as a whole word: `pw` (not `pwn`), and `key` as in
/// `pgp_sym_encrypt(x, key)`, `AES_ENCRYPT(x, key)` or `SQLCipher`
/// `ATTACH 'f' AS e KEY 'k'`.
const WORD_MARKERS: [&str; 2] = ["pw", "key"];

/// Words that make a following `key` a schema keyword, not a secret
/// (`PRIMARY KEY`, `ON DUPLICATE KEY`, `PARTITION KEY`).
const KEY_SCHEMA_WORDS: [&str; 11] = [
    "primary",
    "foreign",
    "unique",
    "partition",
    "sort",
    "duplicate",
    "distribution",
    "cluster",
    "clustering",
    "shard",
    "sharding",
];

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
    /// With `dollar`: `$tag$` opens a literal even right after a word or digit
    /// (`1$$secret$$`).
    glued: bool,
    /// Oracle `q'[...]'` alternative quoting.
    qquote: bool,
}

fn modes() -> impl Iterator<Item = Mode> {
    (0..128_u8)
        .map(|n| Mode {
            backslash: n & 1 != 0,
            nesting: n & 2 != 0,
            hash: n & 4 != 0,
            dashes: n & 8 != 0,
            dollar: n & 16 != 0,
            glued: n & 32 != 0,
            qquote: n & 64 != 0,
        })
        .filter(|m| m.dollar || !m.glued)
}

/// Lexes `sql` under `mode` and calls `mark(start, end)` for every segment
/// that is not plain text. Nothing is collected.
fn lex(sql: &str, mode: Mode, mut mark: impl FnMut(usize, usize)) {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut prev_word = false;
    while i < b.len() {
        let c = b[i];
        let end = match c {
            b'-' if mode.dashes && b.get(i + 1) == Some(&b'-') => Some(line_end(sql, i)),
            b'#' if mode.hash => Some(line_end(sql, i)),
            b'/' if b.get(i + 1) == Some(&b'*') => Some(block_comment_end(b, i, mode.nesting)),
            b'\'' if mode.qquote && i > 0 && matches!(b[i - 1], b'q' | b'Q') => {
                Some(q_quote_end(sql, i).unwrap_or_else(|| quoted_end(sql, i, c, false).0))
            }
            b'\'' | b'"' => Some(quoted_end(sql, i, c, mode.backslash).0),
            b'`' => Some(quoted_end(sql, i, c, false).0),
            b'$' if mode.dollar && (mode.glued || !prev_word) => dollar_end(sql, i),
            _ => None,
        };
        if let Some(end) = end {
            mark(i, end);
            i = end;
            prev_word = false;
        } else {
            prev_word = is_word_byte(c);
            i += 1;
        }
    }
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

/// End (exclusive) of an Oracle `q'<d>...<d>'` literal whose opening quote is at
/// `quote`; `[ ( { <` close with their mirror, any other non-space character
/// with itself. `None` when no delimiter follows. Unterminated: to the end.
fn q_quote_end(sql: &str, quote: usize) -> Option<usize> {
    let d = sql[quote + 1..].chars().next()?;
    if d.is_whitespace() {
        return None;
    }
    let close = match d {
        '[' => ']',
        '(' => ')',
        '{' => '}',
        '<' => '>',
        o => o,
    };
    let body = quote + 1 + d.len_utf8();
    let mut pat = String::new();
    pat.push(close);
    pat.push('\'');
    Some(
        sql[body..]
            .find(&pat)
            .map_or(sql.len(), |n| body + n + pat.len()),
    )
}

/// End (exclusive) of a `$tag$ ... $tag$` literal starting at `start`, or
/// `None` when `$` does not open one (for example `$1`). As in `PostgreSQL`, a
/// tag is empty or starts with an ASCII letter, `_` or any non-ASCII character
/// (letter, symbol or emoji) and continues with those and ASCII digits.
fn dollar_end(sql: &str, start: usize) -> Option<usize> {
    let rest = &sql[start + 1..];
    let tag_len = rest.find('$')?;
    let tag = &rest[..tag_len];
    let mut chars = tag.chars();
    let tag_ok = match chars.next() {
        None => true,
        Some(f) => {
            let head = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
            head(f) && chars.all(|c| head(c) || c.is_ascii_digit())
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

/// Whether the text holds a credential marker anywhere.
fn is_sensitive(sql: &str) -> bool {
    let lower: String = sql.chars().flat_map(char::to_lowercase).collect();
    // A URL with user info (`scheme://user:pw@host`) needs no marker word.
    if (lower.contains("://") && lower.contains('@')) || MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    let squashed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if MULTI_WORD_MARKERS.iter().any(|m| squashed.contains(m)) {
        return true;
    }
    let mut prev = "";
    for word in lower.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if word.is_empty() {
            continue;
        }
        if WORD_MARKERS.contains(&word) && !(word == "key" && KEY_SCHEMA_WORDS.contains(&prev)) {
            return true;
        }
        prev = word;
    }
    false
}

/// Removes literal values from a statement that may carry a credential.
///
/// Conservative by design, not a guarantee. A statement is sensitive when its
/// raw text (any case) contains one of a broad list of markers: as a
/// substring, `passw`, `pwd`, `secret`, `token`, `credential`, `apikey`,
/// `api_key`, `authorization`, `auth_`, `authenticat`, `oauth`, `bearer`,
/// `identified`, `_key`, `salt`, `encrypt`, `decrypt` and similar; as a whole
/// word, `pw` and `key` (but not `PRIMARY KEY` and similar); on
/// whitespace-normalised text, `private key`, `api key` and `identified by`;
/// or `://` together with `@` (a URL with user info). The marker may sit
/// anywhere, including inside comments, identifiers and literals. No lexing is
/// involved in that decision. Bare `auth`, `ssl`, `cert`, `otp` and `hash` are
/// not markers, because they match harmless words.
///
/// In a sensitive statement every `'...'`, `"..."` and dollar-quoted literal,
/// every comment (including `MySQL` `/*! ... */` executable comments) and every
/// backtick identifier that is never closed becomes `'<redacted>'`. The text is
/// lexed under every combination of the readings that differ between dialects
/// (backslash escapes, nesting block comments, `#` and bare `--` comments,
/// dollar quoting, also glued to a preceding word, Oracle `q'[..]'` quoting) and the union of the ranges is redacted. An unterminated
/// construct in any reading hides everything to the end of the statement.
/// Literals holding JSON, YAML, `key=value` or URL credentials are redacted
/// whole because all literals are. Other statements are returned unchanged
/// (`Cow::Borrowed`).
///
/// Known gaps:
/// - an unquoted secret, such as Oracle `IDENTIFIED BY pw`;
/// - password hashes written as bare hex (`MSSQL` `0x...`);
/// - a secret behind a column or variable name that carries no marker;
/// - an encryption key passed as a literal to a function whose name carries no
///   marker (`encrypt`, `decrypt`, `crypt(`) and without a `key` word.
///
/// Memory is bounded by the input: a byte mask of the statement, no per-lexing
/// collection. The history store cuts the text to its size cap before calling
/// this.
#[must_use]
pub fn redact_credentials(sql: &str) -> Cow<'_, str> {
    if !is_sensitive(sql) {
        return Cow::Borrowed(sql);
    }
    let mut covered = vec![false; sql.len()];
    for mode in modes() {
        lex(sql, mode, |s, e| covered[s..e].fill(true));
    }
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < covered.len() {
        if covered[i] {
            while i < covered.len() && covered[i] {
                i += 1;
            }
            out.push_str(PLACEHOLDER);
        } else {
            let start = i;
            while i < covered.len() && !covered[i] {
                i += 1;
            }
            out.push_str(&sql[start..i]);
        }
    }
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

    #[test]
    fn non_ascii_dollar_tags_do_not_leak() {
        for s in [
            "ALTER ROLE u PASSWORD $é$hunter2$é$",
            "ALTER ROLE u PASSWORD $ü_1$hunter2$ü_1$",
            "ALTER ROLE u PASSWORD $日本$hunter2$日本$",
            "ALTER ROLE u PASSWORD $é$ x $é$ || $é$hunter2$é$",
            "ALTER ROLE u PASSWORD $é$hunter2",
            "ALTER ROLE u PASSWORD $😀$hunter2$😀$",
            "ALTER ROLE u PASSWORD $→$hunter2$→$",
            "ALTER ROLE u PASSWORD $é·$hunter2$é·$",
            "ALTER ROLE u PASSWORD $a→1$hunter2$a→1$",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn dollar_quote_glued_to_a_word_does_not_leak() {
        for s in [
            "ALTER ROLE u PASSWORD 1$$hunter2$$",
            "ALTER ROLE u PASSWORD x$t$hunter2$t$",
            "ALTER ROLE u PASSWORD a1$$it's hunter2$$",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn oracle_q_quotes_do_not_leak() {
        for s in [
            "ALTER USER u IDENTIFIED BY q'[it's hunter2]'",
            "ALTER USER u IDENTIFIED BY Q'(it's hunter2)'",
            "ALTER USER u IDENTIFIED BY q'{it's hunter2}'",
            "ALTER USER u IDENTIFIED BY q'<it's hunter2>'",
            "ALTER USER u IDENTIFIED BY q'!it's hunter2!'",
            "ALTER USER u IDENTIFIED BY q'#it's hunter2#'",
            "ALTER USER u IDENTIFIED BY q'-it's hunter2-'",
            "ALTER USER u IDENTIFIED BY q'é it's hunter2é'",
            "ALTER USER u IDENTIFIED BY nq'[it's hunter2]'",
            "ALTER USER u IDENTIFIED BY q'[it's hunter2",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn bare_dollar_quote_does_not_leak() {
        for s in [
            "ALTER ROLE u PASSWORD $$hunter2$$",
            "ALTER ROLE u PASSWORD $$it's hunter2$$",
            "ALTER ROLE u PASSWORD $$hunter2",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
        assert_eq!(
            r("alter role u password $$hunter2$$"),
            "alter role u password '<redacted>'"
        );
    }

    #[test]
    fn encryption_and_key_forms_are_sensitive() {
        for s in [
            "SELECT pgp_sym_encrypt('data', 'hunter2')",
            "SELECT AES_ENCRYPT('data', 'hunter2')",
            "SELECT aes_decrypt(c, 'hunter2') FROM t",
            "ATTACH 'f.db' AS e KEY 'hunter2'",
            "PRAGMA key = 'hunter2'",
            "SET pw = 'hunter2'",
            "SELECT f(x, 'hunter2') -- pw",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn cross_token_and_harmless_words_are_not_markers() {
        for s in [
            "select 'a' from t where not price > 1",
            "select 'a' from class l",
            "select 'a' from author",
            "select 'a', hash from t",
            "select 'a' from t where pwn = 1",
            "create table t (id int primary key, n text default 'x')",
            "insert into t values ('a') on duplicate key update n = 'b'",
            "select 'a' from monkey where x = 'b'",
            "select 'a' from t -- cert, ssl, otp",
        ] {
            assert!(matches!(redact_credentials(s), Cow::Borrowed(_)), "{s}");
        }
    }

    #[test]
    fn multi_word_markers_ignore_whitespace() {
        for s in [
            "SET private   key = 'hunter2'",
            "SET API\nKEY = 'hunter2'",
            "create user u identified\tby 'hunter2'",
        ] {
            let out = r(s);
            assert!(!leaks(&out), "{s} -> {out}");
        }
    }

    #[test]
    fn password_hash_column_is_sensitive() {
        let out = r("insert into users (name, password_hash) values ('a', 'hunter2')");
        assert!(!leaks(&out), "{out}");
    }

    #[test]
    fn one_mebibyte_statement_is_redacted_in_bounded_time() {
        let mut sql = String::from("insert into users (name, password_hash) values ");
        while sql.len() < 1 << 20 {
            sql.push_str("('alice', '$2b$12$abcdefghijklmnopqrstuv'),");
        }
        let t = std::time::Instant::now();
        let out = redact_credentials(&sql);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            t.elapsed()
        );
        assert!(!out.contains("alice"));
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
    const DANGER: [&str; 26] = [
        "'", "\"", "`", "\\", "/*", "*/", "--", "#", "$$", "$t$", "\n", " ", "E'", "/*!50000", ";",
        "''", "\\'", "/* /*", "'a\\'", "password", "$1", "é", "--'", "`a``", "$é$", "$ж$",
    ];
    /// Places for the secret.
    const WRAPS: [&str; 27] = [
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
        "$é$S$é$",
        "$ü_1$S$ü_1$",
        "$日本$S$日本$",
        "$😀$S$😀$",
        "$→$S$→$",
        "$é·$S$é·$",
        "1$$S$$",
        "x$t$S$t$",
        "q'[it's S]'",
        "Q'{it's S}'",
        "q'!it's S!'",
        "q'<it's S",
    ];
    const SENSITIVE: [&str; 14] = [
        "pw",
        "pgp_sym_encrypt",
        "AES_DECRYPT",
        "attach x as e key",
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

    /// Detection, not lexing: every real marker, in varied case and position,
    /// must make the statement sensitive so its literal is removed.
    #[test]
    fn every_marker_in_varied_positions_triggers_redaction() {
        let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
        let mut markers: Vec<&str> = MARKERS.to_vec();
        markers.extend(MULTI_WORD_MARKERS);
        markers.extend(WORD_MARKERS);
        for m in markers {
            for _ in 0..40 {
                let cased: String = m
                    .chars()
                    .map(|c| match rng.next() % 3 {
                        0 => c.to_ascii_uppercase(),
                        _ => c,
                    })
                    .collect();
                let m = cased.as_str();
                let sql = match rng.next() % 7 {
                    0 => format!("select '{SECRET}' from t where {m} = 1"),
                    1 => format!("select {m}, '{SECRET}' from t"),
                    2 => format!("select '{SECRET}' /* {m} */"),
                    3 => format!("select x.{m}, '{SECRET}'"),
                    4 => format!("-- {m}\nselect '{SECRET}'"),
                    5 => format!("select \"{m}\" = '{SECRET}'"),
                    _ => format!("select '{SECRET}' where a = '{m}'"),
                };
                let out = redact_credentials(&sql);
                assert!(!out.contains(SECRET), "{sql:?} -> {out:?}");
            }
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
