// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Addresses a plugin never reaches.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is an address the sandbox refuses: unspecified, loopback, private, CGNAT, link-local, multicast,
/// broadcast, reserved, documentation, benchmarking, ULA, site-local, and the IPv4-mapped, IPv4-compatible, NAT64,
/// 6to4 and Teredo forms that embed one.
#[must_use]
pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0 // 0.0.0.0/8 "this network"
        || (a == 100 && (b & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || a >= 240 // 240.0.0.0/4 reserved
        || (a == 198 && (b & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24 IETF protocol assignments
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    let s = ip.segments();
    // IPv4-mapped (::ffff:a.b.c.d): judged as the embedded v4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    // ::/96, the deprecated IPv4-compatible form (and ::/8 more broadly): never a real destination.
    if s[..6].iter().all(|&x| x == 0) {
        return true;
    }
    // NAT64 64:ff9b::/96: judged as the embedded v4. 64:ff9b:1::/48 (local-use NAT64) is refused whole.
    if s[0] == 0x64 && s[1] == 0xff9b {
        if s[2..6].iter().all(|&x| x == 0) {
            return forbidden_v4(embedded(s[6], s[7]));
        }
        return s[2] == 1;
    }
    // 6to4 2002::/16: the v4 is in segments 1 and 2.
    if s[0] == 0x2002 {
        return forbidden_v4(embedded(s[1], s[2]));
    }
    (s[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
        || (s[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0) // 2001::/32 Teredo
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // 100::/64 discard
}

fn embedded(hi: u16, lo: u16) -> Ipv4Addr {
    let [a, b] = hi.to_be_bytes();
    let [c, d] = lo.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forbidden(s: &str) -> bool {
        is_forbidden(s.parse().unwrap())
    }

    #[test]
    fn v4_table() {
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "239.255.255.255",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "198.18.0.1",
            "198.19.255.255",
            "192.0.0.8",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.0.1",
            "172.32.0.1",
            "198.17.0.1",
            "198.20.0.1",
            "192.0.1.1",
            "93.184.216.34",
        ] {
            assert!(!forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn v6_table() {
        for ip in [
            "::",
            "::1",
            "fe80::1",
            "febf::1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "100::1",
            "::1.2.3.4",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001:81b::200e",
            "2001:4860:4860::8888",
        ] {
            assert!(!forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn mapped_nat64_and_6to4_follow_the_embedded_v4() {
        for ip in [
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "2002:7f00:1::",
            "2002:a9fe:a9fe::1",
            "2002:c0a8:101::",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
        for ip in ["::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1"] {
            assert!(!forbidden(ip), "{ip}");
        }
    }
}
