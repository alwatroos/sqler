// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Validated `net.http` host patterns (ADR-0056).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Why a host pattern was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum HostPatternError {
    /// The pattern is empty.
    #[error("host pattern is empty")]
    Empty,
    /// The host part is longer than 253 characters.
    #[error("host pattern is too long")]
    TooLong,
    /// The pattern has a non-ASCII character.
    #[error("host pattern must be ASCII")]
    NotAscii,
    /// The pattern has an uppercase letter.
    #[error("host pattern must be lowercase")]
    Uppercase,
    /// The pattern has a scheme (`://`).
    #[error("host pattern must not have a scheme")]
    Scheme,
    /// The pattern has userinfo (`@`).
    #[error("host pattern must not have userinfo")]
    Userinfo,
    /// The pattern has a path, query, fragment or backslash.
    #[error("host pattern must not have a path, query or fragment")]
    Path,
    /// A wildcard other than a leading `*.` with two labels after it.
    #[error("wildcard is only allowed as a leading `*.` followed by at least two labels")]
    Wildcard,
    /// An IP literal (brackets or an all-numeric last label).
    #[error("IP literals are not allowed")]
    IpLiteral,
    /// A label is empty, too long or has a character outside `[a-z0-9-]`, or the pattern has whitespace or `%`.
    #[error("bad host label")]
    BadLabel,
    /// A host with one label other than `localhost`.
    #[error("host needs at least two labels")]
    SingleLabel,
    /// The port is not 1..=65535 without a leading zero, or there is more than one `:`.
    #[error("bad port")]
    BadPort,
    /// The host ends with a dot.
    #[error("host must not end with a dot")]
    TrailingDot,
}

/// A validated host pattern with an optional port.
///
/// An absent port means 80 and 443.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostPattern {
    wildcard: bool,
    /// The host without the `*.` prefix.
    host: String,
    port: Option<u16>,
}

impl HostPattern {
    /// Whether the pattern starts with `*.`.
    #[must_use]
    pub fn is_wildcard(&self) -> bool {
        self.wildcard
    }

    /// The host without a wildcard prefix.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The explicit port, if any.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Whether `host:port` is covered by this pattern. The host is compared in lowercase.
    ///
    /// Fails closed: `host` must be a bare host name, with the same label rules as a pattern (letters, digits and `-`
    /// in labels of 1 to 63 bytes, at most 253 bytes, no empty label, no trailing dot). Anything else (a port,
    /// userinfo, a path, a NUL, whitespace, a non-ASCII character, an IP literal) never matches, whatever its suffix.
    /// The caller passes the host and the port of the parsed URL separately.
    #[must_use]
    pub fn matches(&self, host: &str, port: u16) -> bool {
        let port_ok = match self.port {
            Some(p) => p == port,
            None => port == 80 || port == 443,
        };
        if !port_ok {
            return false;
        }
        let host = host.to_ascii_lowercase();
        if host.len() > 253 || host.split('.').any(|l| check_label(l).is_err()) {
            return false;
        }
        if self.wildcard {
            host.len() > self.host.len() + 1
                && host.ends_with(self.host.as_str())
                && host.as_bytes()[host.len() - self.host.len() - 1] == b'.'
        } else {
            host == self.host
        }
    }
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.wildcard {
            f.write_str("*.")?;
        }
        f.write_str(&self.host)?;
        if let Some(p) = self.port {
            write!(f, ":{p}")?;
        }
        Ok(())
    }
}

fn parse_port(s: &str) -> Result<u16, HostPatternError> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) || s.starts_with('0') {
        return Err(HostPatternError::BadPort);
    }
    match s.parse::<u16>() {
        Ok(p) if p >= 1 => Ok(p),
        _ => Err(HostPatternError::BadPort),
    }
}

fn check_label(label: &str) -> Result<(), HostPatternError> {
    let b = label.as_bytes();
    if b.is_empty() || b.len() > 63 {
        return Err(HostPatternError::BadLabel);
    }
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !alnum(b[0]) || !alnum(b[b.len() - 1]) || !b.iter().all(|&c| alnum(c) || c == b'-') {
        return Err(HostPatternError::BadLabel);
    }
    Ok(())
}

impl FromStr for HostPattern {
    type Err = HostPatternError;

    fn from_str(s: &str) -> Result<Self, HostPatternError> {
        use HostPatternError as E;
        if s.is_empty() {
            return Err(E::Empty);
        }
        if s.len() > 253 + 6 {
            return Err(E::TooLong);
        }
        if !s.is_ascii() {
            return Err(E::NotAscii);
        }
        if s.contains("://") {
            return Err(E::Scheme);
        }
        if s.contains('@') {
            return Err(E::Userinfo);
        }
        if s.contains(['/', '?', '#', '\\']) {
            return Err(E::Path);
        }
        if s.contains(['[', ']']) {
            return Err(E::IpLiteral);
        }
        if s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(E::Uppercase);
        }
        if s.bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b == b'%')
        {
            return Err(E::BadLabel);
        }
        let (host_part, port) = match s.split_once(':') {
            None => (s, None),
            Some((h, p)) => (h, Some(parse_port(p)?)),
        };
        if host_part.is_empty() {
            return Err(E::Empty);
        }
        if host_part.len() > 253 {
            return Err(E::TooLong);
        }
        if host_part.ends_with('.') {
            return Err(E::TrailingDot);
        }
        let (wildcard, host) = match host_part.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, host_part),
        };
        if host.contains('*') {
            return Err(E::Wildcard);
        }
        let labels: Vec<&str> = host.split('.').collect();
        for l in &labels {
            check_label(l)?;
        }
        if wildcard {
            if labels.len() < 2 {
                return Err(E::Wildcard);
            }
        } else if labels.len() == 1 && host != "localhost" {
            return Err(E::SingleLabel);
        }
        let last = labels[labels.len() - 1];
        // inet_aton also reads `0x7f.0x0.0x0.0x1`, so a `0x` hex last label is an IP form too.
        let hex_form = last
            .strip_prefix("0x")
            .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
        if last.bytes().all(|b| b.is_ascii_digit()) || hex_form {
            return Err(E::IpLiteral);
        }
        Ok(Self {
            wildcard,
            host: host.to_owned(),
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<HostPattern, HostPatternError> {
        s.parse()
    }

    #[test]
    fn accepted() {
        for ok in [
            "api.example.com",
            "*.example.com",
            "example.com:8443",
            "localhost",
            "localhost:5432",
            "a-b.example.org",
            "*.co.uk",
            "a.b",
        ] {
            let parsed = p(ok).unwrap();
            assert_eq!(parsed.to_string(), ok, "round trip");
        }
    }

    #[test]
    fn rejected_with_reason() {
        use HostPatternError as E;
        let table = [
            ("", E::Empty),
            ("Example.com", E::Uppercase),
            ("*", E::Wildcard),
            ("*.com", E::Wildcard),
            ("a.*.com", E::Wildcard),
            ("*.*.example.com", E::Wildcard),
            ("example.com:0", E::BadPort),
            ("example.com:08", E::BadPort),
            ("example.com:65536", E::BadPort),
            ("example.com:", E::BadPort),
            ("a.b:c:d", E::BadPort),
            ("127.0.0.1", E::IpLiteral),
            ("127.1", E::IpLiteral),
            ("2130706433.", E::TrailingDot),
            ("a.2130706433", E::IpLiteral),
            ("0x7f.0.0.1", E::IpLiteral),
            ("0x7f.0x0.0x0.0x1", E::IpLiteral),
            ("a.0x", E::IpLiteral),
            ("[::1]", E::IpLiteral),
            ("[::1]:80", E::IpLiteral),
            ("https://example.com", E::Scheme),
            ("user@example.com", E::Userinfo),
            ("example.com/x", E::Path),
            ("example.com?x", E::Path),
            ("example.com#x", E::Path),
            ("example.com\\x", E::Path),
            ("a%2eb.com", E::BadLabel),
            ("example.com.", E::TrailingDot),
            ("exämple.com", E::NotAscii),
            ("a b.com", E::BadLabel),
            ("a..com", E::BadLabel),
            ("-a.com", E::BadLabel),
            ("a_b.com", E::BadLabel),
            ("example", E::SingleLabel),
            ("*.localhost", E::Wildcard),
        ];
        for (input, want) in table {
            assert_eq!(p(input), Err(want), "{input}");
        }
        let long = format!("{}.com", "a".repeat(250));
        assert!(p(&long).is_err());
        assert_eq!(p(&"a".repeat(300)), Err(E::TooLong));
        assert_eq!(p(&format!("{}.com", "a".repeat(64))), Err(E::BadLabel));
    }

    #[test]
    fn matching() {
        let w = p("*.a.b").unwrap();
        assert!(w.matches("x.a.b", 443));
        assert!(w.matches("y.x.a.b", 80));
        assert!(w.matches("X.A.B", 443));
        assert!(!w.matches("a.b", 443));
        assert!(!w.matches("xa.b", 443));
        assert!(!w.matches(".a.b", 443));
        assert!(!w.matches("x.a.b", 8080));
        let e = p("example.com:8443").unwrap();
        assert!(e.matches("example.com", 8443));
        assert!(!e.matches("example.com", 443));
        assert!(!e.matches("www.example.com", 8443));
        assert!(p("localhost").unwrap().matches("localhost", 80));
    }

    #[test]
    fn matching_fails_closed_on_unvalidated_hosts() {
        let w = p("*.example.com").unwrap();
        let e = p("example.com").unwrap();
        assert!(w.matches("a.example.com", 443));
        for bad in [
            "evil.com/x.example.com",
            "evil.com\\x.example.com",
            "evil.com\0.example.com",
            "evil.com?.example.com",
            "evil.com#.example.com",
            "user@evil.com.example.com",
            "evil.com:80.example.com",
            "a.example.com:443",
            "a.example.com.",
            ".example.com",
            "a..example.com",
            "a b.example.com",
            "a\t.example.com",
            "a%2e.example.com",
            "a_b.example.com",
            "-a.example.com",
            "[::1].example.com",
            "\u{e4}.example.com",
            "",
            ".",
        ] {
            assert!(!w.matches(bad, 443), "{bad:?}");
            assert!(!e.matches(bad, 443), "{bad:?}");
        }
        assert!(!e.matches("example.com.", 443));
        assert!(!e.matches("example.com:443", 443));
        let long = format!("{}.example.com", "a.".repeat(130));
        assert!(!w.matches(&long, 443));
    }
}
