// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Host normalisation and the `net.http` grant check.

use sqler_plugin_api::{Capability, CapabilitySet};

use crate::egress::{DenyReason, EgressError};

/// A request target that passed [`check_host`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `http` or `https`.
    pub scheme: &'static str,
    /// The host: lowercase, one trailing dot stripped, punycode kept verbatim.
    pub host: String,
    /// The port (the scheme default when the URI has none).
    pub port: u16,
}

fn denied(host: &str, reason: DenyReason) -> EgressError {
    EgressError::Denied {
        host: host.to_owned(),
        reason,
    }
}

/// Validates `uri` against the plugin's effective capabilities.
///
/// In order: the scheme must be `http` or `https` (`BadScheme`); the plugin needs some `net.http` capability
/// (`NoCapability`); the authority must be a plain `host[:port]` without userinfo and the host a valid DNS name,
/// not an IP literal (`BadHost`); the host (exactly one trailing dot stripped, lowercased, never IDNA-decoded) and
/// the port, separately, must be covered by a pattern (`HostNotGranted`). Fails closed.
///
/// # Errors
/// [`EgressError::Denied`] with the reasons above.
pub fn check_host(effective: &CapabilitySet, uri: &http::Uri) -> Result<Target, EgressError> {
    let raw_host = uri.host().unwrap_or_default().to_ascii_lowercase();
    let scheme = match uri.scheme_str() {
        Some(s) if s.eq_ignore_ascii_case("http") => "http",
        Some(s) if s.eq_ignore_ascii_case("https") => "https",
        _ => return Err(denied(&raw_host, DenyReason::BadScheme)),
    };
    if !effective
        .iter()
        .any(|c| matches!(c, Capability::NetHttp(_)))
    {
        return Err(denied(&raw_host, DenyReason::NoCapability));
    }
    if uri.authority().is_some_and(|a| a.as_str().contains('@')) {
        return Err(denied(&raw_host, DenyReason::BadHost));
    }
    let host = raw_host.strip_suffix('.').unwrap_or(&raw_host);
    if !is_dns_name(host) {
        return Err(denied(host, DenyReason::BadHost));
    }
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    if !effective.allows_http(host, port) {
        return Err(denied(host, DenyReason::HostNotGranted));
    }
    Ok(Target {
        scheme,
        host: host.to_owned(),
        port,
    })
}

/// A host name of ASCII labels `[a-z0-9-]` (1..=63, no leading or trailing `-`), at most 253 bytes, whose last label
/// is not numeric (so no IPv4 literal in any notation and no bracketed IPv6).
fn is_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let ok_label = |l: &str| {
        let b = l.as_bytes();
        let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
        !b.is_empty()
            && b.len() <= 63
            && alnum(b[0])
            && alnum(b[b.len() - 1])
            && b.iter().all(|&c| alnum(c) || c == b'-')
    };
    host.split('.').all(ok_label)
        && host
            .rsplit('.')
            .next()
            .is_some_and(|last| !last.bytes().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(items: &[&str]) -> CapabilitySet {
        items
            .iter()
            .map(|s| s.parse::<Capability>().unwrap())
            .collect()
    }

    fn check(c: &CapabilitySet, uri: &str) -> Result<Target, DenyReason> {
        check_host(c, &uri.parse().unwrap()).map_err(|e| match e {
            EgressError::Denied { reason, .. } => reason,
            other => panic!("{other:?}"),
        })
    }

    #[test]
    fn allowed_and_default_ports() {
        let c = caps(&["net.http:api.example.com"]);
        let t = check(&c, "https://API.example.com/x?y=1").unwrap();
        assert_eq!(
            t,
            Target {
                scheme: "https",
                host: "api.example.com".into(),
                port: 443
            }
        );
        assert_eq!(check(&c, "http://api.example.com/").unwrap().port, 80);
        assert_eq!(
            check(&c, "https://api.example.com:8443/"),
            Err(DenyReason::HostNotGranted)
        );
        let c = caps(&["net.http:api.example.com:8443"]);
        assert!(check(&c, "https://api.example.com:8443/").is_ok());
        assert_eq!(
            check(&c, "https://api.example.com/"),
            Err(DenyReason::HostNotGranted)
        );
    }

    #[test]
    fn trailing_dot_is_stripped_exactly_once() {
        let c = caps(&["net.http:api.example.com"]);
        assert_eq!(
            check(&c, "https://api.example.com./").unwrap().host,
            "api.example.com"
        );
        assert_eq!(
            check(&c, "https://api.example.com../"),
            Err(DenyReason::BadHost)
        );
    }

    #[test]
    fn host_not_granted_and_subdomains() {
        let c = caps(&["net.http:example.com", "net.http:*.cdn.example.net"]);
        assert_eq!(
            check(&c, "https://evil.com/"),
            Err(DenyReason::HostNotGranted)
        );
        assert_eq!(
            check(&c, "https://example.com.evil.com/"),
            Err(DenyReason::HostNotGranted)
        );
        assert_eq!(
            check(&c, "https://a.example.com/"),
            Err(DenyReason::HostNotGranted)
        );
        assert!(check(&c, "https://x.cdn.example.net/").is_ok());
        assert_eq!(
            check(&c, "https://cdn.example.net/"),
            Err(DenyReason::HostNotGranted)
        );
    }

    #[test]
    fn punycode_is_kept_verbatim() {
        let c = caps(&["net.http:xn--bcher-kva.example"]);
        let t = check(&c, "https://xn--bcher-kva.example/").unwrap();
        assert_eq!(t.host, "xn--bcher-kva.example");
        match check_host(&c, &"https://xn--other-kva.example/".parse().unwrap()) {
            Err(EgressError::Denied { host, .. }) => assert_eq!(host, "xn--other-kva.example"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ip_literals_userinfo_and_schemes_are_refused() {
        let c = caps(&["net.http:example.com", "net.http:localhost:8080"]);
        for uri in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/",
            "http://[::1]:8080/",
            "http://[::ffff:127.0.0.1]/",
            "http://2130706433/",
            "http://0x7f.0.0.1/",
            "http://0177.0.0.1/",
            "http://127.1/",
            "http://10.0.0.1.example.com.1/",
        ] {
            assert_eq!(check(&c, uri), Err(DenyReason::BadHost), "{uri}");
        }
        assert_eq!(
            check(&c, "https://user:pw@example.com/"),
            Err(DenyReason::BadHost)
        );
        assert_eq!(
            check(&c, "https://example.com@evil.com/"),
            Err(DenyReason::BadHost)
        );
        assert_eq!(check(&c, "ftp://example.com/"), Err(DenyReason::BadScheme));
        assert_eq!(check(&c, "/relative"), Err(DenyReason::BadScheme));
    }

    #[test]
    fn no_net_capability() {
        let c = caps(&["db.read"]);
        assert_eq!(
            check(&c, "https://example.com/"),
            Err(DenyReason::NoCapability)
        );
        assert_eq!(
            check(&CapabilitySet::new(), "https://example.com/"),
            Err(DenyReason::NoCapability)
        );
    }

    #[test]
    fn localhost_needs_an_exact_pattern() {
        let c = caps(&["net.http:localhost:8080"]);
        assert!(check(&c, "http://localhost:8080/").is_ok());
        assert_eq!(
            check(&c, "http://localhost:9/"),
            Err(DenyReason::HostNotGranted)
        );
        assert_eq!(
            check(&c, "http://foo.localhost:8080/"),
            Err(DenyReason::HostNotGranted)
        );
        let c = caps(&["net.http:example.com"]);
        assert_eq!(
            check(&c, "http://localhost/"),
            Err(DenyReason::HostNotGranted)
        );
    }
}
