/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolve the real client IP from `(peer_addr, X-Forwarded-For)` gated by
//! a CIDR allowlist of trusted proxies.
//!
//! XFF is honoured **only** when the peer itself is in `trusted_proxies`;
//! otherwise the peer IP is returned verbatim so an internet client can't
//! spoof its own apparent origin by sending the header.

use axum::http::HeaderMap;
use ipnet::IpNet;
use std::net::IpAddr;

pub fn resolve_client_ip(headers: &HeaderMap, peer: IpAddr, trusted_proxies: &[IpNet]) -> IpAddr {
    let peer = normalize(peer);
    if !in_any(peer, trusted_proxies) {
        return peer;
    }

    let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) else {
        return peer;
    };

    let parsed: Vec<IpAddr> = xff
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .map(normalize)
        .collect();

    if parsed.is_empty() {
        return peer;
    }

    for ip in parsed.iter().rev() {
        if !in_any(*ip, trusted_proxies) {
            return *ip;
        }
    }
    parsed[0]
}

/// Collapse IPv4-mapped IPv6 (`::ffff:a.b.c.d`) to plain IPv4 so dual-stack
/// sockets compare correctly against IPv4 CIDR allowlists.
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v => v,
    }
}

fn in_any(ip: IpAddr, nets: &[IpNet]) -> bool {
    nets.iter().any(|n| n.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    fn headers_with_xff(xff: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(xff).unwrap());
        h
    }

    #[test]
    fn untrusted_peer_no_xff_returns_peer() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = HeaderMap::new();
        assert_eq!(resolve_client_ip(&h, ip("8.8.8.8"), &trusted), ip("8.8.8.8"));
    }

    #[test]
    fn untrusted_peer_with_xff_ignores_xff() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("10.0.0.5");
        assert_eq!(resolve_client_ip(&h, ip("8.8.8.8"), &trusted), ip("8.8.8.8"));
    }

    #[test]
    fn trusted_peer_no_xff_returns_peer() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = HeaderMap::new();
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("127.0.0.1"));
    }

    #[test]
    fn trusted_peer_single_xff_returns_xff() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("10.0.0.5");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("10.0.0.5"));
    }

    #[test]
    fn trusted_peer_multi_xff_stops_at_first_untrusted_from_right() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("10.0.0.5, 127.0.0.1");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("10.0.0.5"));
    }

    #[test]
    fn trusted_peer_all_xff_trusted_returns_leftmost() {
        let trusted = nets(&["127.0.0.0/8"]);
        let h = headers_with_xff("127.0.0.7, 127.0.0.8");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("127.0.0.7"));
    }

    #[test]
    fn malformed_xff_entries_are_skipped() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("garbage, 10.0.0.5");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("10.0.0.5"));
    }

    #[test]
    fn ipv6_peer_and_xff() {
        let trusted = nets(&["::1/128"]);
        let h = headers_with_xff("fd00::1");
        assert_eq!(resolve_client_ip(&h, ip("::1"), &trusted), ip("fd00::1"));
    }

    #[test]
    fn empty_xff_string_returns_peer() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("127.0.0.1"));
    }

    #[test]
    fn untrusted_peer_default_ipv4() {
        let trusted = nets(&["127.0.0.1/32", "::1/128"]);
        let h = headers_with_xff("8.8.8.8");
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        assert_eq!(resolve_client_ip(&h, peer, &trusted), peer);
    }

    #[test]
    fn ipv4_mapped_ipv6_peer_matches_ipv4_trusted_cidr() {
        // Dual-stack listener delivers loopback as ::ffff:127.0.0.1; an IPv4
        // CIDR allowlist must still recognise it as trusted.
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("10.0.0.5");
        let peer = ip("::ffff:127.0.0.1");
        assert_eq!(resolve_client_ip(&h, peer, &trusted), ip("10.0.0.5"));
    }

    #[test]
    fn all_malformed_xff_returns_peer() {
        let trusted = nets(&["127.0.0.1/32"]);
        let h = headers_with_xff("garbage, also-garbage");
        assert_eq!(resolve_client_ip(&h, ip("127.0.0.1"), &trusted), ip("127.0.0.1"));
    }
}
