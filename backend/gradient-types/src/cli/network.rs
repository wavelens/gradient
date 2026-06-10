/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Network-layer config: trusted-proxy and local-IP CIDR allowlists.
//!
//! `--trusted-proxies` gates X-Forwarded-For unwrapping (only peers in this
//! list may rewrite the client IP). `--local-ips` selects which resolved
//! client IPs are eligible for a cache's `local_priority` override.

use clap::Args;
use ipnet::IpNet;

#[derive(Args, Debug, Clone)]
pub struct NetworkArgs {
    /// Comma-separated CIDR allowlist of peers permitted to set
    /// `X-Forwarded-For`. Defaults to loopback (covers reverse-proxies
    /// running on the same host).
    #[arg(
        long,
        env = "GRADIENT_TRUSTED_PROXIES",
        default_value = "127.0.0.1/32,::1/128"
    )]
    pub trusted_proxies: String,

    /// Comma-separated CIDR allowlist whose resolved client IPs receive a
    /// cache's `local_priority` (when set and non-zero). Defaults to the
    /// RFC1918 10/8 block.
    #[arg(long, env = "GRADIENT_LOCAL_IPS", default_value = "10.0.0.0/8")]
    pub local_ips: String,
}

impl Default for NetworkArgs {
    fn default() -> Self {
        Self {
            trusted_proxies: "127.0.0.1/32,::1/128".into(),
            local_ips: "10.0.0.0/8".into(),
        }
    }
}

/// Parse failure for a single CIDR entry; carries the offending token so the
/// operator can spot which one was malformed.
#[derive(Debug, thiserror::Error)]
#[error("invalid CIDR `{entry}`: {source}")]
pub struct CidrParseError {
    pub entry: String,
    #[source]
    pub source: ipnet::AddrParseError,
}

/// Parse a comma-separated CIDR list. Empty / whitespace-only entries are
/// skipped.
pub fn parse_cidr_list(s: &str) -> Result<Vec<IpNet>, CidrParseError> {
    let mut out = Vec::new();
    for raw in s.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let net: IpNet = trimmed.parse().map_err(|source| CidrParseError {
            entry: trimmed.to_string(),
            source,
        })?;
        out.push(net);
    }
    Ok(out)
}

/// `true` if `ip` is contained in any of `nets`.
pub fn in_any(ip: std::net::IpAddr, nets: &[IpNet]) -> bool {
    nets.iter().any(|n| n.contains(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn empty_string_returns_empty_vec() {
        assert!(parse_cidr_list("").unwrap().is_empty());
        assert!(parse_cidr_list("   ").unwrap().is_empty());
        assert!(parse_cidr_list(" , , ").unwrap().is_empty());
    }

    #[test]
    fn single_ipv4_cidr() {
        let v = parse_cidr_list("10.0.0.0/8").unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].contains(&IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
    }

    #[test]
    fn single_ipv6_cidr() {
        let v = parse_cidr_list("fd00::/8").unwrap();
        assert_eq!(v.len(), 1);
        assert!(v[0].contains(&IpAddr::V6("fd00::1".parse::<Ipv6Addr>().unwrap())));
    }

    #[test]
    fn mixed_with_whitespace() {
        let v = parse_cidr_list("  10.0.0.0/8 , ::1/128 ,192.168.0.0/16").unwrap();
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn malformed_entry_returns_err() {
        let err = parse_cidr_list("not-a-cidr").unwrap_err();
        assert!(err.to_string().contains("not-a-cidr"));
    }

    #[test]
    fn malformed_entry_in_middle_returns_err() {
        let err = parse_cidr_list("10.0.0.0/8, banana, 192.168.0.0/16").unwrap_err();
        assert!(err.to_string().contains("banana"));
    }

    #[test]
    fn in_any_hit_and_miss() {
        let nets = parse_cidr_list("10.0.0.0/8, 192.168.0.0/16").unwrap();
        assert!(in_any(IpAddr::V4(Ipv4Addr::new(10, 4, 5, 6)), &nets));
        assert!(in_any(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), &nets));
        assert!(!in_any(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), &nets));
    }

    #[test]
    fn in_any_ipv6_hit() {
        let nets = parse_cidr_list("fd00::/8").unwrap();
        assert!(in_any(IpAddr::V6("fd00::abcd".parse().unwrap()), &nets));
        assert!(!in_any(IpAddr::V6("2001:db8::1".parse().unwrap()), &nets));
    }
}
