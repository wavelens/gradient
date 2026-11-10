/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Hostnames are passing validation unresolved. [`PublicResolver`] is checking them at connect time.
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

#[derive(Debug, thiserror::Error)]
pub enum WorkerConnectionUrlError {
    #[error("Invalid URL: {0}")]
    Parse(#[from] url::ParseError),
    #[error("Use wss as the scheme of the worker URL. Use ws only for a loopback host. Got '{0}'")]
    Scheme(String),
    #[error(
        "Connection tokens travel over ws:// without encryption. Use wss:// for all hosts other than loopback."
    )]
    Unencrypted,
    #[error("Include a host in the URL")]
    MissingHost,
    #[error("The URL has an internal address ({0}). Allow worker URLs on private networks.")]
    InternalAddress(IpAddr),
    #[error(
        "The URL host \"localhost\" is internal. Allow worker URLs on private networks in the admin settings."
    )]
    Localhost,
    #[error("Failed to resolve worker host: {0}")]
    Resolve(#[source] std::io::Error),
    #[error(transparent)]
    InternalOnly(#[from] InternalOnly),
}

pub fn validate_worker_websocket_url(
    url: &str,
    private_networks_allowed: bool,
) -> Result<reqwest::Url, WorkerConnectionUrlError> {
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host().ok_or(WorkerConnectionUrlError::MissingHost)?;
    let loopback = match &host {
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
    };
    match parsed.scheme() {
        "wss" => {}
        "ws" if loopback => {}
        "ws" => return Err(WorkerConnectionUrlError::Unencrypted),
        s => return Err(WorkerConnectionUrlError::Scheme(s.to_owned())),
    }
    if private_networks_allowed {
        return Ok(parsed);
    }
    match host {
        url::Host::Ipv4(ip) if is_unsafe_ipv4(&ip) => {
            Err(WorkerConnectionUrlError::InternalAddress(ip.into()))
        }
        url::Host::Ipv6(ip) if is_unsafe_ipv6(&ip) => {
            Err(WorkerConnectionUrlError::InternalAddress(ip.into()))
        }
        url::Host::Domain(_) if loopback => Err(WorkerConnectionUrlError::Localhost),
        _ => Ok(parsed),
    }
}

/// The server can connect only to the returned addresses, so a DNS response can't redirect the
/// destination to an internal host after the check.
pub async fn check_worker_url_and_resolve_allowed_socket_addresses(
    url: &str,
    private_networks_allowed: bool,
) -> Result<Vec<SocketAddr>, WorkerConnectionUrlError> {
    let parsed = validate_worker_websocket_url(url, private_networks_allowed)?;
    let port = parsed
        .port_or_known_default()
        .ok_or(WorkerConnectionUrlError::MissingHost)?;
    let resolved: Vec<SocketAddr> = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
        Some(url::Host::Domain(domain)) => tokio::net::lookup_host((domain, port))
            .await
            .map_err(WorkerConnectionUrlError::Resolve)?
            .collect(),
        None => return Err(WorkerConnectionUrlError::MissingHost),
    };
    if private_networks_allowed {
        return Ok(resolved);
    }
    Ok(public_addrs(
        parsed.host_str().unwrap_or_default(),
        resolved.into_iter(),
    )?)
}

#[derive(Debug, thiserror::Error)]
#[error("{0} resolves only to private/loopback/link-local/cloud-metadata addresses")]
pub struct InternalOnly(String);

fn public_addrs(
    host: &str,
    addrs: impl Iterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, InternalOnly> {
    let public: Vec<SocketAddr> = addrs.filter(|addr| !is_unsafe_ip(addr.ip())).collect();
    if public.is_empty() {
        return Err(InternalOnly(host.to_owned()));
    }
    Ok(public)
}

fn is_unsafe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_unsafe_ipv4(&v4),
        IpAddr::V6(v6) => is_unsafe_ipv6(&v6),
    }
}

pub(crate) struct PublicResolver;

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public = public_addrs(&host, resolved)?;
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
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

    #[test]
    fn a_worker_url_needs_encryption_and_a_public_host() {
        for url in [
            "wss://worker.example.com/proto",
            "wss://8.8.8.8:9000/proto",
            "wss://[2001:4860:4860::8888]/proto",
        ] {
            assert!(validate_worker_websocket_url(url, false).is_ok(), "{url}");
        }
        for url in [
            "ws://worker.example.com/proto",
            "ws://10.0.0.5:6379/",
            "wss://10.0.0.5:6379/",
            "wss://169.254.169.254/",
            "wss://[::1]/proto",
            "wss://localhost/proto",
            "ws://127.0.0.1:3000/proto",
            "https://worker.example.com/proto",
            "not a url",
        ] {
            assert!(validate_worker_websocket_url(url, false).is_err(), "{url}");
        }
    }

    #[test]
    fn an_admin_can_allow_worker_urls_on_private_networks() {
        for url in [
            "wss://10.0.0.5/proto",
            "ws://127.0.0.1:3000/proto",
            "ws://localhost/proto",
        ] {
            assert!(validate_worker_websocket_url(url, true).is_ok(), "{url}");
        }
        assert!(validate_worker_websocket_url("ws://10.0.0.5/proto", true).is_err());
    }

    #[tokio::test]
    async fn the_dial_addresses_of_a_literal_host_keep_its_port() {
        let addrs =
            check_worker_url_and_resolve_allowed_socket_addresses("wss://8.8.8.8/proto", false)
                .await
                .expect("public address");
        let local = check_worker_url_and_resolve_allowed_socket_addresses("ws://[::1]:3000/", true)
            .await
            .expect("allowed loopback");

        assert_eq!(addrs, vec!["8.8.8.8:443".parse().unwrap()]);
        assert_eq!(local, vec!["[::1]:3000".parse().unwrap()]);
    }

    #[test]
    fn public_addrs_drops_internal_addresses_of_a_mixed_resolution() {
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let resolved = [
            "10.0.0.5:443".parse().unwrap(),
            public,
            "[::1]:443".parse().unwrap(),
            "169.254.169.254:443".parse().unwrap(),
        ];

        let kept = public_addrs("example.com", resolved.into_iter()).expect("one public address");

        assert_eq!(kept, vec![public]);
    }
}
