// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Parsing and rewriting of URL connection targets.
//!
//! A stored target never holds a password (ADR-0011); the database user lives in the `user` query
//! parameter. Right before a driver connects, the pipeline rewrites the in-memory target: the host
//! and port point at the tunnel, and the password from the keychain goes into the userinfo. The
//! result exists only in memory and is never stored or printed. No error here echoes the target.

use std::fmt::{self, Write as _};

use sqler_config::Secret;
use sqler_ui::connections::ConnectFailure;

/// A parsed `scheme://[user[:password]@]host[:port][/rest]` target.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct UrlTarget {
    scheme: String,
    /// Raw (still percent-encoded) userinfo user.
    user: Option<String>,
    /// Raw userinfo password; never printed.
    password: Option<String>,
    /// The host; IPv6 literals keep their brackets.
    host: String,
    port: Option<u16>,
    /// Everything from the first `/`, `?` or `#` after the authority (or empty).
    rest: String,
}

impl fmt::Debug for UrlTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UrlTarget")
            .field("scheme", &self.scheme)
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

fn target_error(message: &str) -> ConnectFailure {
    ConnectFailure::Target(message.to_owned())
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme.to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" => Some(5432),
        "mysql" | "mariadb" => Some(3306),
        _ => None,
    }
}

impl UrlTarget {
    /// Parses a URL target.
    ///
    /// # Errors
    /// [`ConnectFailure::Target`] when the text is not `scheme://host...`, the scheme has no known
    /// default port or the port is not a number in 1-65535.
    pub(crate) fn parse(target: &str) -> Result<Self, ConnectFailure> {
        let (scheme, after) = target
            .split_once("://")
            .ok_or_else(|| target_error("the target is not a URL (scheme://host:port/database)"))?;
        if default_port(scheme).is_none() {
            return Err(target_error(
                "the URL scheme is not supported for tunnelled or password connections",
            ));
        }
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let (authority, rest) = after.split_at(end);
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(info) => match info.split_once(':') {
                Some((u, p)) => (Some(u.to_owned()), Some(p.to_owned())),
                None => (Some(info.to_owned()), None),
            },
            None => (None, None),
        };
        let (host, port) = split_host_port(hostport)?;
        if host.is_empty() || host == "[]" {
            return Err(target_error("the URL has no host"));
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            user: user.filter(|u| !u.is_empty()),
            password,
            host,
            port,
            rest: rest.to_owned(),
        })
    }

    /// The host without IPv6 brackets.
    pub(crate) fn host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host)
    }

    /// The explicit port or the default of the scheme.
    pub(crate) fn port(&self) -> u16 {
        self.port
            .or_else(|| default_port(&self.scheme))
            .unwrap_or_default()
    }

    /// The same target with another host and port (userinfo and the rest are kept).
    pub(crate) fn with_host_port(mut self, host: &str, port: u16) -> Self {
        self.host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        self.port = Some(port);
        self
    }

    /// Whether the target itself asks for host-name verification
    /// (key `sslmode` or `ssl-mode`, value `verify-full` or `verify_identity`, in any driver,
    /// case-insensitive, also percent-encoded).
    pub(crate) fn wants_verify_full(&self) -> bool {
        query_pairs(&self.rest).any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            let value = value.to_ascii_lowercase();
            matches!(key.as_str(), "sslmode" | "ssl-mode")
                && matches!(value.as_str(), "verify-full" | "verify_identity")
        })
    }

    /// The target with the password in the userinfo.
    ///
    /// The user comes from the userinfo or else from the `user` query parameter (which is moved
    /// into the userinfo). User and password are percent-encoded.
    ///
    /// # Errors
    /// [`ConnectFailure::Target`] when the target names no user.
    pub(crate) fn with_password(mut self, password: &Secret) -> Result<Self, ConnectFailure> {
        let user = match self.user.take() {
            Some(user) => user,
            None => query_pairs(&self.rest)
                .find(|(key, _)| key.eq_ignore_ascii_case("user"))
                .map(|(_, value)| value)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    target_error("a database password needs a user; add ?user=name to the target")
                })?,
        };
        self.rest = remove_query_param(&self.rest, "user");
        // The userinfo user is stored encoded; a query value is raw text from the user.
        self.user = Some(encode(&decode(&user)));
        self.password = Some(encode(password.expose()));
        Ok(self)
    }

    /// The target text (contains the password when one was injected: keep it in memory only).
    pub(crate) fn render(&self) -> String {
        let mut out = format!("{}://", self.scheme);
        match (&self.user, &self.password) {
            (Some(user), Some(password)) => {
                out.push_str(user);
                out.push(':');
                out.push_str(password);
                out.push('@');
            }
            (Some(user), None) => {
                out.push_str(user);
                out.push('@');
            }
            (None, Some(password)) => {
                out.push(':');
                out.push_str(password);
                out.push('@');
            }
            (None, None) => {}
        }
        out.push_str(&self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out.push_str(&self.rest);
        out
    }
}

fn split_host_port(hostport: &str) -> Result<(String, Option<u16>), ConnectFailure> {
    let bad_port = || target_error("the URL port is not a number from 1 to 65535");
    let (host, port) = if hostport.starts_with('[') {
        let close = hostport
            .find(']')
            .ok_or_else(|| target_error("the URL has an unclosed IPv6 address"))?;
        let host = &hostport[..=close];
        match &hostport[close + 1..] {
            "" => (host, None),
            tail => (host, Some(tail.strip_prefix(':').ok_or_else(bad_port)?)),
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (hostport, None),
        }
    };
    let port = match port {
        None | Some("") => None,
        Some(text) => match text.parse::<u16>() {
            Ok(0) | Err(_) => return Err(bad_port()),
            Ok(port) => Some(port),
        },
    };
    Ok((host.to_owned(), port))
}

/// The `key=value` pairs of the query in `rest` (values percent-decoded).
fn query_pairs(rest: &str) -> impl Iterator<Item = (String, String)> + '_ {
    let query = rest
        .split_once('?')
        .map_or("", |(_, q)| q.split('#').next().unwrap_or(""));
    query.split('&').filter(|p| !p.is_empty()).map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(key), decode(value))
    })
}

/// `rest` without the query parameter `name` (case-insensitive); other parameters are untouched.
fn remove_query_param(rest: &str, name: &str) -> String {
    let Some((path, tail)) = rest.split_once('?') else {
        return rest.to_owned();
    };
    let (query, fragment) = match tail.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (tail, None),
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            !decode(key).eq_ignore_ascii_case(name)
        })
        .collect();
    let mut out = path.to_owned();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// Percent-encodes everything but `A-Za-z0-9-._~`.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Percent-decodes (invalid escapes stay as they are; invalid UTF-8 becomes `U+FFFD`).
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> UrlTarget {
        UrlTarget::parse(text).unwrap()
    }

    #[test]
    fn parses_host_port_and_defaults() {
        let t = parse("postgres://db.internal/billing?user=app");
        assert_eq!((t.host(), t.port()), ("db.internal", 5432));
        let t = parse("postgresql://db:6543/x");
        assert_eq!((t.host(), t.port()), ("db", 6543));
        assert_eq!(parse("mysql://h").port(), 3306);
        assert_eq!(parse("mariadb://h:1/").port(), 1);
        assert_eq!(parse("postgres://u:p@h").host(), "h");
    }

    #[test]
    fn parses_ipv6() {
        let t = parse("postgres://[::1]:5433/db");
        assert_eq!((t.host(), t.port()), ("::1", 5433));
        assert_eq!(parse("postgres://[fe80::1]/db").port(), 5432);
        assert!(UrlTarget::parse("postgres://[::1/db").is_err());
        assert!(UrlTarget::parse("postgres://[::1]x/db").is_err());
        let moved = t.with_host_port("::1", 9);
        assert_eq!(moved.render(), "postgres://[::1]:9/db");
    }

    #[test]
    fn rejects_bad_targets_without_echoing_them() {
        for bad in [
            "/tmp/file.db",
            "sqlite://x",
            "postgres://",
            "postgres://:5432/db",
            "postgres://h:0/db",
            "postgres://h:99999/db",
            "postgres://h:abc/db",
        ] {
            let err = UrlTarget::parse(bad).unwrap_err();
            assert!(matches!(err, ConnectFailure::Target(_)), "{bad}");
            assert!(!err.to_string().contains(bad), "{bad}");
        }
    }

    #[test]
    fn rewrite_keeps_user_and_rest() {
        let t = parse("postgres://u@db.internal:5432/billing?user=app&application_name=x");
        let t = t.with_host_port("127.0.0.1", 40001);
        assert_eq!(
            t.render(),
            "postgres://u@127.0.0.1:40001/billing?user=app&application_name=x"
        );
    }

    #[test]
    fn password_uses_the_user_query_parameter() {
        let t = parse("postgres://db/billing?user=app&sslmode=require");
        let t = t.with_password(&Secret::new("pw")).unwrap();
        assert_eq!(t.render(), "postgres://app:pw@db/billing?sslmode=require");
        let t = parse("postgres://db?user=app")
            .with_password(&Secret::new("pw"))
            .unwrap();
        assert_eq!(t.render(), "postgres://app:pw@db");
        let t = parse("postgres://db/x?a=1&USER=app#f")
            .with_password(&Secret::new("pw"))
            .unwrap();
        assert_eq!(t.render(), "postgres://app:pw@db/x?a=1#f");
    }

    #[test]
    fn password_prefers_userinfo_user_and_replaces_password() {
        let t = parse("postgres://me:old@db/x?user=other")
            .with_password(&Secret::new("new"))
            .unwrap();
        assert_eq!(t.render(), "postgres://me:new@db/x");
    }

    #[test]
    fn password_without_user_is_a_clear_error() {
        for target in [
            "postgres://db/x",
            "postgres://db/x?user=",
            "postgres://db/x?u=1",
        ] {
            let err = parse(target).with_password(&Secret::new("pw")).unwrap_err();
            assert!(matches!(err, ConnectFailure::Target(_)));
            assert!(err.to_string().contains("?user=name"), "{err}");
            assert!(!err.to_string().contains("pw"));
        }
    }

    #[test]
    fn user_and_password_are_percent_encoded() {
        let t = parse("postgres://db/x?user=a%40b%20c")
            .with_password(&Secret::new("p@ss:w/rd?#%&é~-._"))
            .unwrap();
        assert_eq!(
            t.render(),
            "postgres://a%40b%20c:p%40ss%3Aw%2Frd%3F%23%25%26%C3%A9~-._@db/x"
        );
        // Reparsing keeps host, port and rest intact.
        let again = UrlTarget::parse(&t.render()).unwrap();
        assert_eq!((again.host(), again.port()), ("db", 5432));
    }

    #[test]
    fn decode_round_trips_and_tolerates_bad_escapes() {
        for text in ["plain", "a b", "é€", "%zz", "100%"] {
            assert_eq!(decode(&encode(text)), text);
        }
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(decode("%41"), "A");
    }

    #[test]
    fn detects_verify_full_in_both_dialects() {
        let mut yes = vec![
            "postgres://h/d?a=1&SSLMODE=Verify-Full".to_owned(),
            "mysql://h/d?ssl-mode=VERIFY_IDENTITY".to_owned(),
            "postgres://h/d?ssl%2Dmode=verify%2Dfull".to_owned(),
            "mysql://h/d?%73slmode=verify%5Fidentity".to_owned(),
        ];
        for scheme in ["postgres", "mysql"] {
            for key in ["sslmode", "ssl-mode"] {
                for value in ["verify-full", "verify_identity"] {
                    yes.push(format!("{scheme}://h/d?{key}={value}"));
                }
            }
        }
        for yes in yes {
            assert!(parse(&yes).wants_verify_full(), "{yes}");
        }
        for no in [
            "postgres://h/d",
            "postgres://h/d?sslmode=verify-ca",
            "postgres://h/d?x=sslmode=verify-full",
            "mysql://h/d?ssl-mode=required",
        ] {
            assert!(!parse(no).wants_verify_full(), "{no}");
        }
    }

    #[test]
    fn debug_hides_credentials() {
        let t = parse("postgres://zed:topsecret@db/x");
        assert!(!format!("{t:?}").contains("topsecret"));
        assert!(!format!("{t:?}").contains("zed"));
    }
}
