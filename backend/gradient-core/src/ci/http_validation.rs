/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! SSRF-style URL validation for user-supplied webhook targets.
//!
//! Used by the Actions surface to reject outbound HTTP targets that resolve
//! to loopback / private / link-local / cloud-metadata addresses.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Validate a user-supplied webhook URL against SSRF-style abuse.
///
/// Rejects schemes other than http/https, URLs without a host, IP literals
/// in loopback / private / link-local / multicast / unspecified / broadcast /
/// shared (CGNAT) ranges, and IPv6 literals in loopback / unspecified /
/// multicast / unique-local (fc00::/7) / link-local (fe80::/10) /
/// IPv4-mapped unsafe ranges. Hostnames are accepted at validation time;
/// delivery-time DNS resolution is the caller's responsibility.
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
    fn validate_url_accepts_public_https() {
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
        assert!(validate_webhook_url("http://example.com:8080/hook").is_ok());
        assert!(validate_webhook_url("https://example.com").is_ok());
    }

    #[test]
    fn validate_url_rejects_invalid_scheme() {
        assert!(validate_webhook_url("file:///etc/passwd").is_err());
        assert!(validate_webhook_url("ftp://example.com/").is_err());
        assert!(validate_webhook_url("gopher://example.com/").is_err());
        assert!(validate_webhook_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn validate_url_rejects_unparseable() {
        assert!(validate_webhook_url("not a url").is_err());
        assert!(validate_webhook_url("").is_err());
    }

    #[test]
    fn validate_url_rejects_localhost_name() {
        assert!(validate_webhook_url("http://localhost/").is_err());
        assert!(validate_webhook_url("http://LOCALHOST/").is_err());
        assert!(validate_webhook_url("http://Localhost:8080/path").is_err());
    }

    #[test]
    fn validate_url_rejects_loopback_ipv4() {
        assert!(validate_webhook_url("http://127.0.0.1/").is_err());
        assert!(validate_webhook_url("http://127.255.255.254/").is_err());
    }

    #[test]
    fn validate_url_rejects_aws_metadata_ip() {
        assert!(validate_webhook_url("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(validate_webhook_url("http://169.254.0.1/").is_err());
    }

    #[test]
    fn validate_url_rejects_rfc1918_ranges() {
        assert!(validate_webhook_url("http://10.0.0.1/").is_err());
        assert!(validate_webhook_url("http://10.255.255.255/").is_err());
        assert!(validate_webhook_url("http://172.16.0.1/").is_err());
        assert!(validate_webhook_url("http://172.31.255.255/").is_err());
        assert!(validate_webhook_url("http://192.168.0.1/").is_err());
        assert!(validate_webhook_url("http://192.168.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_cgnat_shared_space() {
        assert!(validate_webhook_url("http://100.64.0.1/").is_err());
        assert!(validate_webhook_url("http://100.127.255.254/").is_err());
        assert!(validate_webhook_url("http://100.128.0.1/").is_ok());
        assert!(validate_webhook_url("http://100.63.255.255/").is_ok());
    }

    #[test]
    fn validate_url_rejects_unspecified_and_broadcast() {
        assert!(validate_webhook_url("http://0.0.0.0/").is_err());
        assert!(validate_webhook_url("http://255.255.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_multicast_ipv4() {
        assert!(validate_webhook_url("http://224.0.0.1/").is_err());
        assert!(validate_webhook_url("http://239.255.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_reserved_ipv4() {
        assert!(validate_webhook_url("http://240.0.0.1/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_loopback_and_unspecified() {
        assert!(validate_webhook_url("http://[::1]/").is_err());
        assert!(validate_webhook_url("http://[::]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_link_and_unique_local() {
        assert!(validate_webhook_url("http://[fe80::1]/").is_err());
        assert!(validate_webhook_url("http://[febf::1]/").is_err());
        assert!(validate_webhook_url("http://[fc00::1]/").is_err());
        assert!(validate_webhook_url("http://[fdff::1]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_multicast() {
        assert!(validate_webhook_url("http://[ff00::1]/").is_err());
        assert!(validate_webhook_url("http://[ff02::1]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv4_mapped_loopback_in_ipv6() {
        assert!(validate_webhook_url("http://[::ffff:7f00:1]/").is_err());
        assert!(validate_webhook_url("http://[::ffff:a9fe:a9fe]/").is_err());
    }

    #[test]
    fn validate_url_accepts_public_ipv4_literal() {
        assert!(validate_webhook_url("http://8.8.8.8/").is_ok());
    }

    #[test]
    fn validate_url_accepts_public_ipv6_literal() {
        assert!(validate_webhook_url("http://[2001:4860:4860::8888]/").is_ok());
    }
}
