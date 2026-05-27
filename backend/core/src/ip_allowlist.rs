/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Source-IP allowlist matching for API keys and inbound integrations.
//! Empty list = allow all (backwards compatible with existing rows).

use ipnet::IpNet;
use std::net::IpAddr;

pub fn is_allowed(ip: IpAddr, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let ip = normalize(ip);
    allowlist
        .iter()
        .filter_map(|s| s.parse::<IpNet>().ok())
        .any(|net| net.contains(&ip))
}

pub fn normalize_entry(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty entry".into());
    }
    if let Ok(net) = trimmed.parse::<IpNet>() {
        return Ok(net.to_string());
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        let prefix = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let net = IpNet::new(ip, prefix).map_err(|e| e.to_string())?;
        return Ok(net.to_string());
    }
    Err(format!("not a valid IP or CIDR: {trimmed}"))
}

fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v => v,
    }
}
