/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::{Ipv4Addr, Ipv6Addr};

/// Hostnames are passing validation unresolved. The caller is responsible for DNS resolution checks
/// at delivery time.
#[derive(Debug, thiserror::Error)]
pub enum WebhookUrlError {
    #[error("Invalid URL: {0}")]
    Parse(#[from] url::ParseError),
    #[error("URL scheme must be http or https, got '{0}'")]
    Scheme(String),
    #[error("URL must include a host")]
    MissingHost,
    #[error(
        "URL points to a disallowed address ({0}); private/loopback/link-local/cloud-metadata addresses are blocked"
    )]
    UnsafeAddress(std::net::IpAddr),
    #[error("URL host is empty")]
    EmptyHost,
    #[error("URL host 'localhost' is not allowed")]
    Localhost,
}

pub fn validate_webhook_url(url: &str) -> Result<reqwest::Url, WebhookUrlError> {
    let parsed = reqwest::Url::parse(url)?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(WebhookUrlError::Scheme(s.to_string())),
    }
    let host = parsed.host().ok_or(WebhookUrlError::MissingHost)?;
    match host {
        url::Host::Ipv4(ip) => {
            if is_unsafe_ipv4(&ip) {
                return Err(WebhookUrlError::UnsafeAddress(ip.into()));
            }
        }
        url::Host::Ipv6(ip) => {
            if is_unsafe_ipv6(&ip) {
                return Err(WebhookUrlError::UnsafeAddress(ip.into()));
            }
        }
        url::Host::Domain(d) => {
            if d.is_empty() {
                return Err(WebhookUrlError::EmptyHost);
            }
            if d.eq_ignore_ascii_case("localhost") {
                return Err(WebhookUrlError::Localhost);
            }
        }
    }
    Ok(parsed)
}

fn is_unsafe_ipv4(ip: &Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
    {
        return true;
    }
    let o = ip.octets();
    if o[0] == 100 && (o[1] & 0xC0) == 64 {
        return true;
    }
    if o[0] == 0 {
        return true;
    }
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return true;
    }
    if o[0] >= 240 {
        return true;
    }
    false
}

fn is_unsafe_ipv6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segs = ip.segments();
    if (segs[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    if (segs[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    if segs[0] == 0
        && segs[1] == 0
        && segs[2] == 0
        && segs[3] == 0
        && segs[4] == 0
        && segs[5] == 0xffff
    {
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            (segs[6] & 0xff) as u8,
            (segs[7] >> 8) as u8,
            (segs[7] & 0xff) as u8,
        );
        if is_unsafe_ipv4(&v4) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_webhook_url_accepts_public_targets() {
        for url in [
            "https://example.com/hook",
            "http://example.com:8080/hook",
            "https://example.com",
            "http://100.128.0.1/",
            "http://100.63.255.255/",
            "http://8.8.8.8/",
            "http://[2001:4860:4860::8888]/",
        ] {
            assert!(validate_webhook_url(url).is_ok(), "{url}");
        }
    }

    #[test]
    fn validate_webhook_url_rejects_every_internal_or_bad_target() {
        for url in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "gopher://example.com/",
            "javascript:alert(1)",
            "not a url",
            "",
            "http://localhost/",
            "http://LOCALHOST/",
            "http://Localhost:8080/path",
            "http://127.0.0.1/",
            "http://127.255.255.254/",
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.0.1/",
            "http://10.0.0.1/",
            "http://10.255.255.255/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.0.1/",
            "http://192.168.255.255/",
            "http://100.64.0.1/",
            "http://100.127.255.254/",
            "http://0.0.0.0/",
            "http://255.255.255.255/",
            "http://224.0.0.1/",
            "http://239.255.255.255/",
            "http://240.0.0.1/",
            "http://[::1]/",
            "http://[::]/",
            "http://[fe80::1]/",
            "http://[febf::1]/",
            "http://[fc00::1]/",
            "http://[fdff::1]/",
            "http://[ff00::1]/",
            "http://[ff02::1]/",
            "http://[::ffff:7f00:1]/",
            "http://[::ffff:a9fe:a9fe]/",
        ] {
            assert!(validate_webhook_url(url).is_err(), "{url}");
        }
    }
}
