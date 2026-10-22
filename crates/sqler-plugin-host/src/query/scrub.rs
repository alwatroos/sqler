// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The host-side sanitiser of `DriverError::Query` text.
//!
//! The drivers build `Query` from every server-side error on an established connection, so besides syntax and
//! constraint errors it carries permission failures that name the database account, the client host and the database
//! (`SELECT command denied to user 'app'@'10.0.3.7' for table 'orders'`). A plugin with `db.read` and `net.http` could
//! send that out. [`query_text`] is what the guest may see of such a message: a fixed text for anything that looks like
//! a denial, otherwise the message with accounts, hosts, addresses and file system paths replaced, then clipped.

use super::convert::clip;

/// What the guest is told for any message that looks like an authorization, authentication or permission failure.
pub(crate) const DENIED: &str = "the database denied the request";

/// What replaces an account, host, address or path.
pub(crate) const REDACTED: &str = "<redacted>";

/// Lower-case fragments that mark a message as a denial.
const DENIAL_MARKERS: &[&str] = &[
    "access denied",
    "command denied",
    "permission denied",
    "denied to user",
    "not permitted",
    "authentication",
    "password",
    "login failed",
    "role \"",
    "must be owner",
    "insufficient privilege",
    "privilege",
];

/// Words after which a quoted string names an account or a host.
const IDENTITY_WORDS: &[&str] = &["user", "host", "hostname", "role", "account", "login"];

/// The text of a `DriverError::Query` as the guest may see it.
pub(crate) fn query_text(text: &str) -> String {
    let lower = text.to_lowercase();
    if DENIAL_MARKERS.iter().any(|m| lower.contains(m)) {
        return DENIED.to_owned();
    }
    clip(&scrub(text))
}

fn is_quote(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}

/// The last alphabetic word of `out`, lower-cased.
fn last_word(out: &str) -> String {
    let trimmed = out.trim_end();
    let word: String = trimmed
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect();
    word.chars().rev().collect::<String>().to_lowercase()
}

/// Index of the next `quote` after `from`, if any.
fn find_close(chars: &[char], from: usize, quote: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == quote)
        .map(|p| from + p)
}

/// Whether a path starts at `p` of `token` (a unix `/x`, a Windows `C:\` or `C:/`, or a UNC `\\`).
fn path_starts(token: &[char], p: usize) -> bool {
    let at = |i: usize| token.get(i).copied();
    let boundary = p == 0 || matches!(token[p - 1], ':' | '=' | '(' | ',' | '[' | '<');
    match token[p] {
        '/' => {
            boundary
                && at(p + 1)
                    .is_some_and(|n| n.is_alphanumeric() || matches!(n, '.' | '_' | '~' | '/'))
        }
        '\\' => (p == 0 || boundary) && at(p + 1) == Some('\\'),
        c if c.is_ascii_alphabetic() => {
            (p == 0 || boundary) && at(p + 1) == Some(':') && matches!(at(p + 2), Some('\\' | '/'))
        }
        _ => false,
    }
}

fn is_ipv4(core: &str) -> bool {
    let host = core.split(':').next().unwrap_or("");
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_ipv6(core: &str) -> bool {
    let inner = core.trim_start_matches('[');
    let inner = inner.split(']').next().unwrap_or(inner);
    inner.matches(':').count() >= 2
        && inner
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '%'))
}

fn has_address_shape(core: &str) -> bool {
    if core.is_empty() {
        return false;
    }
    if is_ipv4(core) || is_ipv6(core) {
        return true;
    }
    // `x@y`, with something on both sides.
    match core.find('@') {
        Some(at) => at > 0 && at + 1 < core.len(),
        None => false,
    }
}

/// The edge punctuation that is not part of a token's meaning.
fn is_edge(c: char) -> bool {
    matches!(c, '(' | ')' | ',' | ';' | '.' | '{' | '}' | '<' | '>' | '=')
}

/// Replaces accounts, hosts, addresses and paths in `text` with [`REDACTED`]. A path swallows the rest of the text
/// (or the rest of its quotes), since a path may hold spaces.
pub(crate) fn scrub(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            out.push(c);
            i += 1;
        } else if is_quote(c) {
            let Some(j) = find_close(&chars, i + 1, c) else {
                // An unterminated quote: whatever follows may be a name.
                out.push(c);
                out.push_str(&scrub(&chars[i + 1..].iter().collect::<String>()));
                return out;
            };
            let inner: String = chars[i + 1..j].iter().collect();
            let after_at = chars.get(j + 1) == Some(&'@');
            if after_at || IDENTITY_WORDS.contains(&last_word(&out).as_str()) {
                out.push_str(REDACTED);
                i = j + 1;
                if after_at {
                    i = skip_after_at(&chars, j + 2);
                }
            } else {
                out.push(c);
                out.push_str(&scrub(&inner));
                out.push(c);
                i = j + 1;
            }
        } else {
            let end = chars[i..]
                .iter()
                .position(|&ch| ch.is_whitespace() || is_quote(ch))
                .map_or(chars.len(), |p| i + p);
            let token = &chars[i..end];
            if let Some(p) = (0..token.len()).find(|&p| path_starts(token, p)) {
                out.extend(&token[..p]);
                out.push_str(REDACTED);
                // A path may hold spaces: it runs to the end of the text (or of its quotes, one level up).
                return out;
            }
            let lead = token.iter().take_while(|&&ch| is_edge(ch)).count();
            let trail = token[lead..]
                .iter()
                .rev()
                .take_while(|&&ch| is_edge(ch))
                .count();
            let core: String = token[lead..token.len() - trail].iter().collect();
            if has_address_shape(&core) {
                out.extend(&token[..lead]);
                out.push_str(REDACTED);
                out.extend(&token[token.len() - trail..]);
                i = end;
                // `user@'host'`: the quoted host belongs to the account.
                if core.ends_with('@') && chars.get(i).is_some_and(|&q| is_quote(q)) {
                    i = skip_after_at(&chars, i);
                }
            } else if core.ends_with('@') && chars.get(end).is_some_and(|&q| is_quote(q)) {
                out.extend(&token[..lead]);
                out.push_str(REDACTED);
                i = skip_after_at(&chars, end);
            } else {
                out.extend(token);
                i = end;
            }
        }
    }
    out
}

/// Index after the host part of an account that starts at `from` (right after the `@`): a quoted string or a word.
fn skip_after_at(chars: &[char], from: usize) -> usize {
    match chars.get(from) {
        Some(&q) if is_quote(q) => find_close(chars, from + 1, q).map_or(chars.len(), |j| j + 1),
        _ => chars[from.min(chars.len())..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(chars.len(), |p| from + p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn assert_clean(message: &str, secrets: &[&str]) {
        let out = query_text(message);
        for s in secrets {
            assert!(!out.contains(s), "`{s}` leaked from `{message}` as `{out}`");
        }
    }

    #[test]
    fn mysql_permission_errors_become_the_fixed_text() {
        for m in [
            "Access denied for user 'u'@'10.0.3.7' to database 'prod' (1044)",
            "Access denied for user 'app'@'10.0.3.7' (using password: YES)",
            "SELECT command denied to user 'app'@'10.0.3.7' for table 'orders'",
            "SELECT command denied to user 'app'@'10.0.3.7' for column 'c' in table 'prod'",
            "Access denied; you need (at least one of) the SUPER privilege(s) for this operation",
            "Unknown database 'prod'; Access denied for user 'app'@'10.0.3.7'",
        ] {
            assert_eq!(query_text(m), DENIED, "{m}");
        }
    }

    #[test]
    fn postgres_denials_become_the_fixed_text() {
        for m in [
            "password authentication failed for user \"bob\"",
            "permission denied for table orders",
            "role \"bob\" does not exist",
            "must be owner of table t",
            "permission denied for schema prod",
        ] {
            assert_eq!(query_text(m), DENIED, "{m}");
        }
    }

    #[test]
    fn mssql_login_failures_become_the_fixed_text() {
        assert_eq!(query_text("Login failed for user 'sa'"), DENIED);
    }

    #[test]
    fn hosts_accounts_and_paths_are_scrubbed_from_other_messages() {
        assert_clean(
            "could not connect to server: host \"10.1.2.3\", port 5432 failed",
            &["10.1.2.3"],
        );
        assert_clean(
            "unable to open database file: /Users/x/db.sqlite",
            &["/Users/x", "db.sqlite"],
        );
        assert_clean(
            "cannot open C:\\data\\db.sqlite",
            &["C:\\data", "db.sqlite"],
        );
        assert_clean(
            "cannot open \\\\?\\C:\\data\\db.sqlite",
            &["C:\\data", "db.sqlite"],
        );
        assert_clean(
            "cannot open 'C:/data/my db.sqlite' now",
            &["data", "db.sqlite"],
        );
        assert_clean("lost connection to 10.0.3.7:3306 at read", &["10.0.3.7"]);
        assert_clean("lost connection to [fe80::1]:3306", &["fe80::1"]);
        assert_clean("definer app@10.0.3.7 is gone", &["app", "10.0.3.7"]);
        assert_clean("definer 'app'@'10.0.3.7' is gone", &["app", "10.0.3.7"]);
        assert_clean("definer `app`@`prod` is gone", &["app", "prod"]);
        assert_clean("definer app@'prod' is gone", &["app", "prod"]);
        assert_clean("view of user 'app' is invalid", &["app"]);
        assert_clean("view of user \"bob\" is invalid", &["bob"]);
        assert_clean("no route to host 'prod'", &["prod"]);
        assert_clean("file=/Users/x/db.sqlite failed", &["/Users/x"]);
    }

    #[test]
    fn ordinary_errors_stay_readable() {
        for m in [
            "near \"selct\": syntax error",
            "UNIQUE constraint failed: users.email",
            "relation \"t\" does not exist",
            "Duplicate entry '5' for key 'PRIMARY'",
            "column \"c\" of relation \"t\" violates not-null constraint",
            "Table 'shop.orders' doesn't exist",
            "division by zero in 10 / 2",
            "syntax error at or near \"/*\"",
        ] {
            assert_eq!(query_text(m), m);
        }
    }

    #[test]
    fn the_scrub_comes_before_the_clip() {
        let long = format!("{} unable to open /Users/x/db.sqlite", "é".repeat(500));
        let out = query_text(&long);
        assert!(!out.contains("/Users"));
        assert!(out.chars().count() <= 512);
        let cut = format!("{} 10.0.3.7", "é".repeat(505));
        assert!(!query_text(&cut).contains("10.0"));
        assert_eq!(query_text(&"é".repeat(900)).chars().count(), 512);
    }

    proptest! {
        #[test]
        fn no_account_or_host_survives(
            user in "zq[a-z0-9]{3,8}",
            host in "[a-z]{2,6}\\.zq[a-z]{3,6}\\.test",
            a in 1u8..255, b in 0u8..255, c in 0u8..255, d in 1u8..255,
            prefix in "[a-zA-Z ]{0,20}",
        ) {
            let ip = format!("{a}.{b}.{c}.{d}");
            let templates = [
                format!("{prefix} 'x' for user '{user}'@'{host}'"),
                format!("{prefix} user '{user}' on host '{host}'"),
                format!("{prefix} user \"{user}\" at {ip}"),
                format!("{prefix} {user}@{host} failed"),
                format!("{prefix} connect to {ip}:5432 failed"),
                format!("{prefix} open /home/{user}/{host}.db"),
                format!("{prefix} open C:\\Users\\{user}\\{host}.db"),
                format!("{prefix} Access denied for user '{user}'@'{host}'"),
                format!("{prefix} permission denied for table {user}"),
            ];
            for t in &templates {
                let out = query_text(t);
                prop_assert!(!out.contains(&user), "{t} -> {out}");
                prop_assert!(!out.contains(&host), "{t} -> {out}");
                prop_assert!(!out.contains(&ip), "{t} -> {out}");
                prop_assert!(out.chars().count() <= 512);
            }
        }

        #[test]
        fn never_panics_and_never_exceeds_the_clip(text in "\\PC{0,700}") {
            prop_assert!(query_text(&text).chars().count() <= 512);
        }
    }
}
