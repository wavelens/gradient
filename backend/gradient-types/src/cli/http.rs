/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;
use gradient_derive::config_group;
use ipnet::IpNet;

#[config_group("http")]
#[derive(Args, Debug, Clone)]
pub struct HttpArgs {
    /// Maximum HTTP request body size in bytes for most endpoints, to keep an unbounded body
    /// from exhausting server memory. Build request blob uploads use a fixed 20 MiB cap.
    #[arg(value_parser = greater_than_zero::<usize>, default_value_t = 2 * 1024 * 1024)]
    pub max_request_size: usize,

    /// Maximum size in bytes of a source upload to `POST /build-requests/source` (as sent by
    /// `gradient build`) and of a chunked manifest in total. The built-in reverse proxies raise
    /// their body size limit to fit it.
    #[arg(value_parser = greater_than_zero::<usize>, default_value_t = 512 * 1024 * 1024)]
    pub max_source_upload_size: usize,

    /// CIDR ranges of peers allowed to set `X-Forwarded-For`. The defaults trust a reverse
    /// proxy on the same host.
    #[arg(default_value = "127.0.0.1/8,::1/128")]
    pub trusted_proxies: String,

    /// Clients in these CIDR ranges receive a cache's `local_priority`, if set and non-zero.
    #[arg(default_value = "192.168.0.0/16,172.16.0.0/12,100.64.0.0/10,10.0.0.0/8,fc00::/7")]
    pub local_ips: String,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid CIDR `{entry}`: {source}")]
pub struct CidrParseError {
    pub entry: String,
    #[source]
    pub source: ipnet::AddrParseError,
}

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
