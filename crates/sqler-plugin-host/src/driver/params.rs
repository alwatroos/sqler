// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! What a guest driver receives at connect: the parts of the connection's target, and nothing else.
//!
//! The target is the string the user entered for a connection that uses the plugin's driver, after the connect
//! pipeline put the password into it. `scheme://user:pass@host:port/db?k=v` becomes host, user, password, port,
//! database and the query pairs; a target that is not a URL is the database name (a file path, for example). The
//! guest never gets the SSH secret, TLS file paths, other connections, the environment or a host engine's config.

use std::fmt;

/// The connect parameters of a guest driver.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct ConnectParams {
    pub host: String,
    pub database: String,
    pub user: String,
    pub password: Option<String>,
    pub port: Option<u16>,
    pub options: Vec<(String, String)>,
}

impl fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectParams")
            .field("host", &self.host)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("port", &self.port)
            .field("options", &self.options.len())
            .finish()
    }
}

impl ConnectParams {
    /// Splits `target`. Never fails: whatever is not understood stays in `database`.
    pub(crate) fn parse(target: &str) -> Self {
        let Some((scheme, rest)) = target.split_once("://") else {
            return Self {
                database: target.to_owned(),
                ..Self::default()
            };
        };
        if scheme.is_empty()
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Self {
                database: target.to_owned(),
                ..Self::default()
            };
        }
        let rest = rest.split('#').next().unwrap_or_default();
        let (before_query, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, path) = before_query
            .split_once('/')
            .map_or((before_query, ""), |(a, p)| (a, p));
        let (userinfo, hostport) = authority
            .rsplit_once('@')
            .map_or(("", authority), |(u, h)| (u, h));
        let (user, password) = match userinfo.split_once(':') {
            Some((u, p)) => (decode(u), Some(decode(p))),
            None => (decode(userinfo), None),
        };
        let (host, port) = host_and_port(hostport);
        let options = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(k), decode(v))
            })
            .collect();
        Self {
            host: decode(&host),
            database: decode(path),
            user,
            password,
            port,
            options,
        }
    }
}

/// `host`, `host:port` or `[v6]` / `[v6]:port`. A port that is not a number stays part of the host.
fn host_and_port(hostport: &str) -> (String, Option<u16>) {
    if let Some(inner) = hostport.strip_prefix('[') {
        if let Some((v6, after)) = inner.split_once(']') {
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
            return (v6.to_owned(), port);
        }
        return (hostport.to_owned(), None);
    }
    match hostport.rsplit_once(':') {
        Some((h, p)) => p.parse().map_or_else(
            |_| (hostport.to_owned(), None),
            |port| (h.to_owned(), Some(port)),
        ),
        None => (hostport.to_owned(), None),
    }
}

/// Percent-decoding; an invalid escape stays as it is, invalid UTF-8 becomes U+FFFD.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| hex(*b)),
                bytes.get(i + 2).and_then(|b| hex(*b)),
            )
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_url_is_split() {
        let p = ConnectParams::parse(
            "fixture://ann:s3cret@db.example.com:5433/sales?sslmode=require&x=1",
        );
        assert_eq!(p.host, "db.example.com");
        assert_eq!(p.port, Some(5433));
        assert_eq!(p.user, "ann");
        assert_eq!(p.password.as_deref(), Some("s3cret"));
        assert_eq!(p.database, "sales");
        assert_eq!(
            p.options,
            [
                ("sslmode".into(), "require".into()),
                ("x".into(), "1".into())
            ]
        );
    }

    #[test]
    fn percent_escapes_are_decoded() {
        let p = ConnectParams::parse("d://a%40b:p%3Aw%2Fd%25@h/db%20x?k%3D=v%26w");
        assert_eq!(p.user, "a@b");
        assert_eq!(p.password.as_deref(), Some("p:w/d%"));
        assert_eq!(p.database, "db x");
        assert_eq!(p.options, [("k=".into(), "v&w".into())]);
    }

    #[test]
    fn invalid_escapes_stay() {
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(decode("%C3%A9"), "\u{e9}");
        assert_eq!(decode("%FF"), "\u{fffd}");
    }

    #[test]
    fn ipv6_hosts() {
        let p = ConnectParams::parse("d://u@[::1]:5432/x");
        assert_eq!(p.host, "::1");
        assert_eq!(p.port, Some(5432));
        let p = ConnectParams::parse("d://[fe80::1]/x");
        assert_eq!(p.host, "fe80::1");
        assert_eq!(p.port, None);
        let p = ConnectParams::parse("d://[broken/x");
        assert_eq!(p.host, "[broken");
    }

    #[test]
    fn missing_parts_are_empty() {
        let p = ConnectParams::parse("d://host");
        assert_eq!(p.host, "host");
        assert_eq!(p.user, "");
        assert_eq!(p.password, None);
        assert_eq!(p.port, None);
        assert_eq!(p.database, "");
        assert_eq!(p.options, Vec::<(String, String)>::new());
        let p = ConnectParams::parse("d://u@host/");
        assert_eq!(p.user, "u");
        assert_eq!(p.password, None);
        let p = ConnectParams::parse("d://u:@host");
        assert_eq!(p.password.as_deref(), Some(""));
    }

    #[test]
    fn a_port_that_is_not_a_number_stays_in_the_host() {
        let p = ConnectParams::parse("d://host:abc/x");
        assert_eq!(p.host, "host:abc");
        assert_eq!(p.port, None);
        let p = ConnectParams::parse("d://host:99999/x");
        assert_eq!(p.port, None);
    }

    #[test]
    fn a_target_that_is_not_a_url_is_the_database() {
        for t in [
            "/var/data/my.db",
            ":memory:",
            "C:\\data\\x.db",
            "",
            "a b://c",
        ] {
            let p = ConnectParams::parse(t);
            assert_eq!(p.database, t);
            assert_eq!(p.host, "");
            assert_eq!(p.password, None);
        }
    }

    #[test]
    fn an_option_without_a_value_and_a_fragment() {
        let p = ConnectParams::parse("d://h/db?flag&&k=v#frag");
        assert_eq!(
            p.options,
            [("flag".into(), String::new()), ("k".into(), "v".into())]
        );
    }

    #[test]
    fn debug_never_shows_the_password() {
        let p = ConnectParams::parse("d://u:hunter2@h/db");
        let text = format!("{p:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<redacted>"));
    }
}
