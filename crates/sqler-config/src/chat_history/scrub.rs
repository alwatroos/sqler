// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Heuristic removal of credentials from text before it is stored.
//!
//! Every string and every object key of a stored entry goes through [`scrub_text`] on the
//! store's own thread, so no caller can bypass it. It is conservative, not a guarantee: an
//! unquoted free-text secret with no marker and no known key shape stays.

use serde_json::Value;

use crate::redact_credentials;

const PLACEHOLDER: &str = "<redacted>";

/// Word prefixes of well-known API keys and tokens.
const KEY_PREFIXES: [&str; 11] = [
    "sk-",
    "sk_",
    "sk-ant-",
    "ghp_",
    "gho_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
];

/// A key-shaped word needs at least this many characters.
const KEY_MIN_LEN: usize = 20;

/// Lowercase fragments after which a value is hidden.
const MARKERS: [&str; 7] = [
    "passw", "pwd", "secret", "token", "apikey", "api_key", "bearer",
];

/// Removes credentials from `text`: the SQL-style redaction, then userinfo of URLs, key-shaped
/// words and the values that follow a marker word.
pub(crate) fn scrub_text(text: &str) -> String {
    let base = redact_credentials(text);
    let keyed = scrub_keys(&base);
    let words = split_words(&keyed);
    let mut out = String::with_capacity(keyed.len());
    let mut pending = false;
    for (is_space, piece) in words {
        if is_space {
            out.push_str(piece);
            continue;
        }
        let piece = scrub_userinfo(piece);
        if pending {
            if matches!(
                piece.as_str(),
                "=" | ":" | "=:" | ":=" | "is" | "are" | "was"
            ) {
                out.push_str(&piece);
                continue;
            }
            // A value word. A marker inside it (`password token x`) keeps the chain going.
            push_value(&mut out, &piece);
            pending = marker_without_value(&piece);
            continue;
        }
        match marker_value_start(&piece) {
            Marker::None => out.push_str(&piece),
            Marker::Inline(at) => {
                out.push_str(&piece[..at]);
                push_value(&mut out, &piece[at..]);
            }
            Marker::Next => {
                out.push_str(&piece);
                pending = true;
            }
        }
    }
    out
}

/// Pushes the placeholder for `value`, or `value` itself when it is already a (quoted)
/// placeholder, so a redacted SQL literal keeps its quotes.
fn push_value(out: &mut String, value: &str) {
    let bare = value.trim_end_matches([',', ';', ')']);
    if matches!(bare, "'<redacted>'" | "\"<redacted>\"") {
        out.push_str(value);
    } else {
        out.push_str(PLACEHOLDER);
    }
}

/// Scrubs every string and every object key of `value`. A string under a key that holds a
/// marker is replaced as a whole.
pub(crate) fn scrub_value(value: &mut Value) {
    match value {
        Value::String(s) => *s = scrub_text(s),
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(map) => {
            let old = std::mem::take(map);
            for (key, mut item) in old {
                let hidden =
                    has_marker(&key) && matches!(item, Value::String(_) | Value::Number(_));
                if hidden {
                    item = Value::String(PLACEHOLDER.to_owned());
                } else {
                    scrub_value(&mut item);
                }
                map.insert(scrub_text(&key), item);
            }
        }
        _ => {}
    }
}

fn has_marker(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    MARKERS.iter().any(|m| lower.contains(m))
}

/// Splits into runs of whitespace and runs of non-whitespace, in order.
fn split_words(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut current: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let space = c.is_whitespace();
        match current {
            Some(s) if s == space => {}
            Some(s) => {
                out.push((s, &text[start..i]));
                start = i;
                current = Some(space);
            }
            None => current = Some(space),
        }
    }
    if let Some(s) = current {
        out.push((s, &text[start..]));
    }
    out
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Replaces key-shaped tokens (`[A-Za-z0-9_-]+` with a known prefix, 20+ characters).
fn scrub_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut token_start: Option<usize> = None;
    let flush = |out: &mut String, from: usize, to: usize| {
        let token = &text[from..to];
        if token.len() >= KEY_MIN_LEN && KEY_PREFIXES.iter().any(|p| token.starts_with(p)) {
            out.push_str(PLACEHOLDER);
        } else {
            out.push_str(token);
        }
    };
    for (i, c) in text.char_indices() {
        if is_key_char(c) {
            token_start.get_or_insert(i);
        } else {
            if let Some(from) = token_start.take() {
                flush(&mut out, from, i);
            }
            out.push(c);
        }
    }
    if let Some(from) = token_start {
        flush(&mut out, from, text.len());
    }
    out
}

/// In a `scheme://user:pass@host` word, the userinfo becomes the placeholder.
fn scrub_userinfo(word: &str) -> String {
    let Some(at_scheme) = word.find("://") else {
        return word.to_owned();
    };
    let start = at_scheme + 3;
    let rest = &word[start..];
    match rest.rfind('@') {
        Some(at) if rest[..at].contains(':') => {
            format!("{}{PLACEHOLDER}{}", &word[..start], &rest[at..])
        }
        _ => word.to_owned(),
    }
}

enum Marker {
    /// No marker in the word.
    None,
    /// The value starts at this byte of the word.
    Inline(usize),
    /// The value is the next word.
    Next,
}

fn marker_end(word: &str) -> Option<usize> {
    let lower = word.to_ascii_lowercase();
    MARKERS
        .iter()
        .filter_map(|m| lower.find(m).map(|i| i + m.len()))
        .min()
}

fn marker_value_start(word: &str) -> Marker {
    let Some(end) = marker_end(word) else {
        return Marker::None;
    };
    match word[end..].find(['=', ':']) {
        Some(i) => {
            let value = end + i + 1;
            if word[value..].is_empty() {
                Marker::Next
            } else {
                Marker::Inline(value)
            }
        }
        None => Marker::Next,
    }
}

fn marker_without_value(word: &str) -> bool {
    matches!(marker_value_start(word), Marker::Next)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn url_userinfo_is_hidden() {
        let out = scrub_text("connect to postgres://u:CANARY@db.example/x now");
        assert!(!out.contains("CANARY"), "{out}");
        assert!(out.contains("postgres://<redacted>@db.example/x"), "{out}");
    }

    #[test]
    fn url_without_password_stays() {
        assert_eq!(scrub_text("ssh://git@host/x"), "ssh://git@host/x");
    }

    #[test]
    fn key_shaped_words_are_hidden() {
        for key in [
            "sk-abcdefghijklmnopqrstuv",
            "sk-ant-api03-abcdefghijklmnop",
            "ghp_abcdefghijklmnopqrstuvwxyz",
            "AKIAABCDEFGHIJKLMNOP",
            "AIzaSyABCDEFGHIJKLMNOPQRSTUV",
            "xoxb-1234567890-abcdefghij",
        ] {
            let out = scrub_text(&format!("my key is {key}, ok"));
            assert!(!out.contains(key), "{out}");
            assert!(out.contains("<redacted>, ok"), "{out}");
        }
    }

    #[test]
    fn short_prefixed_words_stay() {
        assert_eq!(scrub_text("the sk-1 thing"), "the sk-1 thing");
    }

    #[test]
    fn marker_values_are_hidden() {
        for text in [
            "token=CANARY",
            "token: CANARY",
            "token = CANARY",
            "token CANARY",
            "my password is CANARY",
            "Authorization: Bearer CANARY",
            "{\"password\":\"CANARY\"}",
            "password token CANARY",
        ] {
            let out = scrub_text(text);
            assert!(!out.contains("CANARY"), "{text} -> {out}");
        }
    }

    #[test]
    fn a_redacted_literal_keeps_its_quotes() {
        for text in [
            "select 1 where password = '<redacted>'",
            "password='<redacted>', x",
        ] {
            assert_eq!(scrub_text(text), text);
        }
    }

    #[test]
    fn plain_text_stays() {
        let text = "show me the orders per customer";
        assert_eq!(scrub_text(text), text);
    }

    #[test]
    fn scrub_is_idempotent() {
        let once = scrub_text("password=CANARY sk-abcdefghijklmnopqrstuv");
        assert_eq!(scrub_text(&once), once);
    }

    #[test]
    fn values_are_scrubbed_in_strings_and_keys() {
        let mut v = json!({
            "sql": "select 1 where password = 'CANARY'",
            "password": "CANARY",
            "nested": [{"api_key": "CANARY"}, "token=CANARY"],
            "token=CANARY": 1,
            "n": 5
        });
        scrub_value(&mut v);
        let text = v.to_string();
        assert!(!text.contains("CANARY"), "{text}");
        assert_eq!(v["n"], 5);
    }
}
