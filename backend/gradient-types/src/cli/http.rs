/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;
use ipnet::IpNet;

#[derive(Args, Debug, Clone)]
pub struct HttpArgs {
    /// Maximum size in bytes of an HTTP request body for most endpoints (default 2 MiB).
    #[arg(
        long = "http-max-request-size",
        env = "GRADIENT_HTTP_MAX_REQUEST_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 2 * 1024 * 1024,
    )]
    pub max_request_size: usize,

    /// Maximum size in bytes of a source upload to `POST /build-requests/source`
    /// (single-shot NAR) and the chunked manifest total (default 512 MiB).
    #[arg(
        long = "http-max-source-upload-size",
        env = "GRADIENT_HTTP_MAX_SOURCE_UPLOAD_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 512 * 1024 * 1024,
    )]
    pub max_source_upload_size: usize,

    /// Comma-separated CIDR allowlist of peers permitted to set `X-Forwarded-For`. The default is
    /// loopback, covering reverse proxies running on the same host.
    #[arg(
        long = "http-trusted-proxies",
        env = "GRADIENT_HTTP_TRUSTED_PROXIES",
        default_value = "127.0.0.1/32,::1/128"
    )]
    pub trusted_proxies: String,

    /// Comma-separated CIDR allowlist whose resolved client IPs receive a cache's `local_priority`
    /// (when set and non-zero). The default is the RFC1918 10/8 block.
    #[arg(
        long = "http-local-ips",
        env = "GRADIENT_HTTP_LOCAL_IPS",
        default_value = "10.0.0.0/8"
    )]
    pub local_ips: String,
}

impl Default for HttpArgs {
    fn default() -> Self {
        Self {
            max_request_size: 2 * 1024 * 1024,
            max_source_upload_size: 512 * 1024 * 1024,
            trusted_proxies: "127.0.0.1/32,::1/128".into(),
            local_ips: "10.0.0.0/8".into(),
        }
    }
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
