/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use futures::future::BoxFuture;
use ipnet::IpNet;

use crate::http_base_url::ValidatedHttpBaseUrl;

pub type HostNameAndPortToSocketAddressesLookupFunction =
    fn(String, u16) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>>;

#[derive(Debug, thiserror::Error)]
pub enum DestinationUrlRejectedError {
    #[error("Invalid URL: {0}")]
    Parse(#[from] url::ParseError),
    #[error("URL scheme must be http or https, got '{0}'")]
    Scheme(String),
    #[error("URL must include a host")]
    MissingHost,
    #[error(
        "The URL target is a private address ({0}). An admin can allow the private address in setting `outbound.allowedNonPublicNetworkTargets` (`GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS`)."
    )]
    UnsafeAddress(std::net::IpAddr),
    #[error("URL host is empty")]
    EmptyHost,
    #[error(
        "URL host `localhost` isn't allowed. An admin can allow the host in setting `outbound.allowedNonPublicNetworkTargets` (`GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS`)."
    )]
    Localhost,
    #[error("Stopped after {0} redirect responses")]
    TooManyRedirects(usize),
    #[error("Rejected redirect from https to http")]
    HttpsToHttpRedirect,
    #[error(transparent)]
    HostHasNoAllowedAddress(#[from] HostHasNoAllowedAddressError),
}

#[derive(Debug, thiserror::Error)]
pub enum RefusedDestinationIpAddressReason {
    #[error("{0} is not a public address, so the server administrator doesn't allow it.")]
    NonPublicAddressNotAllowed(IpAddr),
    #[error(
        "{0} is an address of the server itself. The server administrator doesn't allow this address."
    )]
    ServerHostInterfaceAddressNotAllowed(IpAddr),
    #[error(
        "The server rejected {address} because it failed to read the addresses of its network interfaces: {source}"
    )]
    NetworkInterfaceAddressesReadFailed {
        address: IpAddr,
        source: nix::errno::Errno,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid entry `{0}`: specify a CIDR range, IP address, or hostname")]
pub struct InvalidAdminAllowListEntryError(String);

/// Error messages use the same wording for a name that DNS lookup can't resolve and for a name with refused addresses. Users then can't tell which internal names exist, or which addresses belong to the server. Debug-level logs record the cause.
#[derive(Debug, thiserror::Error)]
#[error(
    "The server can't connect to {host}. The name doesn't resolve, or the address is private and not allowed. An admin can allow the host in setting `outbound.allowedNonPublicNetworkTargets` (`GRADIENT_OUTBOUND_ALLOWED_NON_PUBLIC_NETWORK_TARGETS`)."
)]
pub struct HostHasNoAllowedAddressError {
    host: String,
}

pub fn find_allowlist_rejection_in_error_chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a (dyn std::error::Error + 'static)> {
    std::iter::successors(Some(error), |cause| cause.source()).find(|cause| {
        cause.is::<DestinationUrlRejectedError>() || cause.is::<HostHasNoAllowedAddressError>()
    })
}

impl HostHasNoAllowedAddressError {
    fn from_failed_host_lookup(host: &str, error: &std::io::Error) -> Self {
        tracing::debug!(host, %error, "Lookup of the host name failed.");
        Self {
            host: host.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserUrlDestinationAllowlist {
    networks: Vec<IpNet>,
    host_names: Vec<String>,
    read_network_interface_ip_addresses: fn() -> nix::Result<Vec<IpAddr>>,
    lookup_host_socket_addresses: HostNameAndPortToSocketAddressesLookupFunction,
}

impl Default for UserUrlDestinationAllowlist {
    fn default() -> Self {
        Self {
            networks: Vec::new(),
            host_names: Vec::new(),
            read_network_interface_ip_addresses: get_local_network_interface_addresses,
            lookup_host_socket_addresses: resolve_host_name_and_port_to_socket_addresses,
        }
    }
}

fn resolve_host_name_and_port_to_socket_addresses(
    host: String,
    port: u16,
) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
    Box::pin(async move { Ok(tokio::net::lookup_host((host, port)).await?.collect()) })
}

impl UserUrlDestinationAllowlist {
    pub fn parse(entries: &[String]) -> Result<Self, InvalidAdminAllowListEntryError> {
        let mut allowlist = Self::default();
        for entry in entries
            .iter()
            .map(|raw| raw.trim())
            .filter(|e| !e.is_empty())
        {
            if let Ok(network) = entry.parse::<IpNet>() {
                allowlist.networks.push(network);
                continue;
            }
            if let Ok(ip) = entry.parse::<IpAddr>() {
                allowlist.networks.push(ip.into());
                continue;
            }
            match url::Host::parse(entry) {
                Ok(url::Host::Domain(name)) if !name.contains(['%', '[', ']']) => {
                    allowlist
                        .host_names
                        .push(name.trim_end_matches('.').to_ascii_lowercase());
                }
                _ => return Err(InvalidAdminAllowListEntryError(entry.to_owned())),
            }
        }
        Ok(allowlist)
    }

    pub fn with_added_allowed_host_or_ip(mut self, host: &str) -> Self {
        match host.trim_matches(['[', ']']).parse::<IpAddr>() {
            Ok(ip) => self.networks.push(ip.into()),
            Err(_) => self
                .host_names
                .push(host.trim_end_matches('.').to_ascii_lowercase()),
        }
        self
    }

    #[cfg(test)]
    fn with_network_interface_address_reader(
        mut self,
        reader: fn() -> nix::Result<Vec<IpAddr>>,
    ) -> Self {
        self.read_network_interface_ip_addresses = reader;
        self
    }

    #[cfg(any(test, feature = "test-functions"))]
    pub fn with_dns_lookup_function(
        mut self,
        lookup: HostNameAndPortToSocketAddressesLookupFunction,
    ) -> Self {
        self.lookup_host_socket_addresses = lookup;
        self
    }

    fn check_outbound_destination_ip_address_is_allowed(
        &self,
        ip: IpAddr,
    ) -> Result<(), RefusedDestinationIpAddressReason> {
        self.filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
            &ip.to_string(),
            std::iter::once(SocketAddr::new(ip, 0)),
            self.read_network_interface_ip_addresses,
        )
        .map(drop)
    }

    fn filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
        &self,
        host: &str,
        addrs: impl Iterator<Item = SocketAddr>,
        read_interface_addresses: impl FnOnce() -> nix::Result<Vec<IpAddr>>,
    ) -> Result<Vec<SocketAddr>, RefusedDestinationIpAddressReason> {
        use RefusedDestinationIpAddressReason::*;
        let host_allowed = self.is_host_name_on_admin_allow_list(host);
        let interface_addresses = std::cell::LazyCell::new(read_interface_addresses);
        let mut allowed = Vec::new();
        let mut refusal = None;
        for addr in addrs {
            let ip = addr.ip().to_canonical();
            let verdict = if host_allowed || self.networks.iter().any(|n| n.contains(&ip)) {
                Ok(())
            } else if is_unsafe_ip(ip) {
                Err(NonPublicAddressNotAllowed(ip))
            } else {
                match &*interface_addresses {
                    Ok(own) if own.iter().any(|own| own.to_canonical() == ip) => {
                        Err(ServerHostInterfaceAddressNotAllowed(ip))
                    }
                    Ok(_) => Ok(()),
                    Err(source) => Err(NetworkInterfaceAddressesReadFailed {
                        address: ip,
                        source: *source,
                    }),
                }
            };
            match verdict {
                Ok(()) => allowed.push(addr),
                Err(reason)
                    if refusal.is_none() || !matches!(reason, NonPublicAddressNotAllowed(_)) =>
                {
                    refusal = Some(reason)
                }
                Err(_) => {}
            }
        }
        match refusal {
            Some(reason) if allowed.is_empty() => Err(reason),
            _ => Ok(allowed),
        }
    }

    pub fn is_host_name_on_admin_allow_list(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        self.host_names
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }

    pub fn check_destination_url_is_allowed(
        &self,
        url: &reqwest::Url,
    ) -> Result<(), DestinationUrlRejectedError> {
        check_url_scheme_is_http_or_https(url)?;
        let host = url
            .host_str()
            .ok_or(DestinationUrlRejectedError::MissingHost)?;
        self.check_host_text_without_dns_for_allowed_ip_literal(host)
            .map(drop)
    }

    fn check_host_text_without_dns_for_allowed_ip_literal(
        &self,
        host: &str,
    ) -> Result<Option<IpAddr>, DestinationUrlRejectedError> {
        let unbracketed = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host);
        if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            return self
                .check_outbound_destination_ip_address_is_allowed(ip)
                .map(|()| Some(ip))
                .map_err(|reason| {
                    tracing::debug!(%reason, "The server rejected an IP address in the URL.");
                    DestinationUrlRejectedError::UnsafeAddress(ip)
                });
        }
        if host.is_empty() {
            return Err(DestinationUrlRejectedError::EmptyHost);
        }
        if host.eq_ignore_ascii_case("localhost") && !self.is_host_name_on_admin_allow_list(host) {
            return Err(DestinationUrlRejectedError::Localhost);
        }
        Ok(None)
    }

    pub async fn allowed_socket_addresses_for_host_and_port(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, DestinationUrlRejectedError> {
        if let Some(ip) = self.check_host_text_without_dns_for_allowed_ip_literal(host)? {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let resolved = (self.lookup_host_socket_addresses)(host.to_owned(), port)
            .await
            .map_err(|error| HostHasNoAllowedAddressError::from_failed_host_lookup(host, &error))?;
        Ok(self.allowed_socket_addrs_for_host(host, resolved.into_iter())?)
    }

    pub fn allowlist_entries_as_strings(&self) -> Vec<String> {
        self.networks
            .iter()
            .map(IpNet::to_string)
            .chain(self.host_names.iter().cloned())
            .collect()
    }

    pub fn allowed_socket_addrs_for_host(
        &self,
        host: &str,
        addrs: impl Iterator<Item = SocketAddr>,
    ) -> Result<Vec<SocketAddr>, HostHasNoAllowedAddressError> {
        match self.filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
            host,
            addrs,
            self.read_network_interface_ip_addresses,
        ) {
            Ok(allowed) if !allowed.is_empty() => return Ok(allowed),
            Ok(_) => tracing::debug!(host, "The host name did not resolve to any address."),
            Err(reason) => tracing::debug!(
                host,
                %reason,
                "The server rejected every address that the hostname resolved to."
            ),
        }
        Err(HostHasNoAllowedAddressError {
            host: host.to_owned(),
        })
    }

    pub async fn allowed_socket_addresses_for_url(
        &self,
        base_url: &ValidatedHttpBaseUrl,
    ) -> Result<Vec<SocketAddr>, DestinationUrlRejectedError> {
        let url = reqwest::Url::parse(base_url.as_str())?;
        check_url_scheme_is_http_or_https(&url)?;
        let host = url
            .host_str()
            .ok_or(DestinationUrlRejectedError::MissingHost)?;
        let port = url
            .port_or_known_default()
            .ok_or(DestinationUrlRejectedError::MissingHost)?;
        self.allowed_socket_addresses_for_host_and_port(host, port)
            .await
    }
}

fn check_url_scheme_is_http_or_https(
    url: &reqwest::Url,
) -> Result<(), DestinationUrlRejectedError> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(DestinationUrlRejectedError::Scheme(scheme.to_owned())),
    }
}

pub fn parse_and_check_allowed_outbound_url(
    url: &str,
    allowlist: &UserUrlDestinationAllowlist,
) -> Result<reqwest::Url, DestinationUrlRejectedError> {
    let parsed = reqwest::Url::parse(url)?;
    allowlist.check_destination_url_is_allowed(&parsed)?;
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
    #[error(transparent)]
    HostHasNoAllowedAddress(#[from] HostHasNoAllowedAddressError),
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
    let allowlist = UserUrlDestinationAllowlist::default();
    let literal = |ip: IpAddr| {
        if !private_networks_allowed {
            allowlist
                .check_outbound_destination_ip_address_is_allowed(ip)
                .map_err(|reason| {
                    tracing::debug!(%reason, "The server rejected the IP address in the worker URL.");
                    WorkerConnectionUrlError::InternalAddress(ip)
                })?;
        }
        Ok(vec![SocketAddr::new(ip, port)])
    };
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => literal(ip.into()),
        Some(url::Host::Ipv6(ip)) => literal(ip.into()),
        Some(url::Host::Domain(domain)) => {
            let resolved = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|error| {
                    HostHasNoAllowedAddressError::from_failed_host_lookup(domain, &error)
                })?;
            if private_networks_allowed {
                return Ok(resolved.collect());
            }
            Ok(allowlist.allowed_socket_addrs_for_host(domain, resolved)?)
        }
        None => Err(WorkerConnectionUrlError::MissingHost),
    }
}

pub fn read_network_interface_addresses_and_log_error_on_failure() {
    if let Err(error) = get_local_network_interface_addresses() {
        tracing::error!(
            %error,
            "This process can't read the addresses of the network interfaces of its own host. All outgoing requests to addresses that the server administrator doesn't allow fail. The refusal is active until the process can read those addresses. Allow the AF_NETLINK address family for this process."
        );
    }
}

fn get_local_network_interface_addresses() -> nix::Result<Vec<IpAddr>> {
    Ok(nix::ifaddrs::getifaddrs()?
        .filter_map(|interface| interface.address)
        .filter_map(|address| {
            address
                .as_sockaddr_in()
                .map(|v4| IpAddr::V4(v4.ip()))
                .or_else(|| address.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip())))
        })
        .collect())
}

fn is_unsafe_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_unsafe_ipv4(&v4),
        IpAddr::V6(v6) => is_unsafe_ipv6(&v6),
    }
}

pub(crate) struct DestinationRuleDnsResolver(pub(crate) Arc<UserUrlDestinationAllowlist>);

impl reqwest::dns::Resolve for DestinationRuleDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let allowlist = Arc::clone(&self.0);
        Box::pin(async move {
            let resolved = (allowlist.lookup_host_socket_addresses)(host.clone(), 0)
                .await
                .map_err(|error| {
                    HostHasNoAllowedAddressError::from_failed_host_lookup(&host, &error)
                })?;
            let allowed = allowlist.allowed_socket_addrs_for_host(&host, resolved.into_iter())?;
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
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
    if matches!(segs, [0x2001, 0, ..] | [0x64, 0xff9b, 1, ..]) {
        return true;
    }
    let ipv4_from = |high: u16, low: u16| {
        let [a, b] = high.to_be_bytes();
        let [c, d] = low.to_be_bytes();
        Ipv4Addr::new(a, b, c, d)
    };
    let embedded = match segs {
        [0, 0, 0, 0, 0, 0xffff, high, low]
        | [0, 0, 0, 0, 0xffff, 0, high, low]
        | [0, 0, 0, 0, 0, 0, high, low] => Some(ipv4_from(high, low)),
        [0x64, 0xff9b, 0, 0, 0, 0, high, low] => Some(ipv4_from(high, low)),
        [0x2002, high, low, ..] => Some(ipv4_from(high, low)),
        _ => None,
    };
    embedded.is_some_and(|v4| is_unsafe_ipv4(&v4))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_only() -> UserUrlDestinationAllowlist {
        UserUrlDestinationAllowlist::default()
    }

    fn entries(list: &str) -> Vec<String> {
        list.split(',').map(str::to_owned).collect()
    }

    #[test]
    fn parse_and_check_allowed_outbound_url_accepts_public_targets() {
        for url in [
            "https://example.com/hook",
            "http://example.com:8080/hook",
            "https://example.com",
            "http://100.128.0.1/",
            "http://100.63.255.255/",
            "http://8.8.8.8/",
            "http://[2001:4860:4860::8888]/",
            "http://[2002:808:808::1]/",
            "http://[64:ff9b::808:808]/",
        ] {
            assert!(
                parse_and_check_allowed_outbound_url(url, &public_only()).is_ok(),
                "{url}"
            );
        }
    }

    #[test]
    fn parse_and_check_allowed_outbound_url_rejects_every_internal_or_bad_target() {
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
            "http://[64:ff9b::7f00:1]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[2002:a00:1::1]/",
            "http://[2002:c0a8:101::]/",
        ] {
            assert!(
                parse_and_check_allowed_outbound_url(url, &public_only()).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn the_admin_allow_list_reads_cidrs_addresses_and_host_names() {
        let allowlist = UserUrlDestinationAllowlist::parse(&entries(
            " 10.0.0.0/8 , ::1 ,192.168.1.7,Git.Internal.Example,,",
        ))
        .expect("valid entries");

        for url in [
            "http://10.1.2.3/",
            "http://[::ffff:10.1.2.3]/",
            "http://[::1]/",
            "http://192.168.1.7/",
        ] {
            assert!(
                parse_and_check_allowed_outbound_url(url, &allowlist).is_ok(),
                "{url}"
            );
        }
        for url in ["http://192.168.1.8/", "http://127.0.0.1/"] {
            assert!(
                parse_and_check_allowed_outbound_url(url, &allowlist).is_err(),
                "{url}"
            );
        }
        assert!(allowlist.is_host_name_on_admin_allow_list("git.internal.example"));
        assert!(allowlist.is_host_name_on_admin_allow_list("GIT.internal.example."));
        assert!(!allowlist.is_host_name_on_admin_allow_list("other.internal.example"));
        assert!(
            parse_and_check_allowed_outbound_url("http://10.0.0.5:3000/api", &allowlist).is_ok()
        );
        assert!(parse_and_check_allowed_outbound_url("http://[::1]/", &allowlist).is_ok());
        assert!(parse_and_check_allowed_outbound_url("http://172.16.0.1/", &allowlist).is_err());
    }

    #[test]
    fn localhost_passes_only_when_the_admin_names_it() {
        let allowlist =
            UserUrlDestinationAllowlist::parse(&entries("localhost")).expect("valid entry");

        assert!(parse_and_check_allowed_outbound_url("http://localhost:8080/", &allowlist).is_ok());
        assert!(
            parse_and_check_allowed_outbound_url("http://localhost:8080/", &public_only()).is_err()
        );
    }

    #[test]
    fn an_invalid_allow_list_entry_is_named() {
        for list in ["10.0.0.0/33", "10.0.0.0/8,not a host", "http://x/", "a/b"] {
            let err = UserUrlDestinationAllowlist::parse(&entries(list)).expect_err(list);
            assert!(err.to_string().contains('`'), "{err}");
        }
    }

    #[test]
    fn an_allowed_host_name_keeps_its_internal_addresses() {
        let allowlist = public_only().with_added_allowed_host_or_ip("Gitea.Lan");
        let internal: SocketAddr = "10.0.0.5:443".parse().unwrap();

        let kept = allowlist
            .allowed_socket_addrs_for_host("gitea.lan", [internal].into_iter())
            .expect("allowed host");
        let refused = allowlist.allowed_socket_addrs_for_host("other.lan", [internal].into_iter());

        assert_eq!(kept, vec![internal]);
        assert!(refused.is_err());
    }

    #[test]
    fn host_name_resolving_to_private_address_is_refused_and_error_text_contains_no_ip_address() {
        let resolved: SocketAddr = "10.20.0.7:443".parse().unwrap();

        let refused = public_only()
            .allowed_socket_addrs_for_host("db.corp.internal", [resolved].into_iter())
            .expect_err("private address");

        for text in [refused.to_string(), format!("{refused:?}")] {
            assert!(text.contains("db.corp.internal"), "{text}");
            assert!(!text.contains("10.20"), "{text}");
        }
    }

    #[test]
    fn refuses_public_url_and_resolved_public_host_when_network_interface_address_read_fails() {
        let allowlist = public_only()
            .with_network_interface_address_reader(|| Err(nix::errno::Errno::EAFNOSUPPORT));
        let url = reqwest::Url::parse("http://203.0.113.8/").unwrap();

        let literal = allowlist.check_destination_url_is_allowed(&url);
        let resolved = allowlist.allowed_socket_addrs_for_host(
            "example.com",
            ["203.0.113.8:443".parse().unwrap()].into_iter(),
        );

        let literal = literal.expect_err("failed interface read").to_string();
        let resolved = resolved.expect_err("failed interface read").to_string();
        for text in [&literal, &resolved] {
            assert!(!text.contains("EAFNOSUPPORT"), "{text}");
            assert!(!text.contains("network interfaces"), "{text}");
        }
        assert!(literal.contains("203.0.113.8"), "{literal}");
    }

    #[test]
    fn url_with_ip_address_on_server_network_interface_gets_same_refusal_message_as_private_ip_url()
    {
        let own_interface = public_only()
            .with_network_interface_address_reader(|| Ok(vec!["203.0.113.7".parse().unwrap()]));
        let expect_refusal_message_for_url =
            |url: &str, allowlist: &UserUrlDestinationAllowlist| {
                parse_and_check_allowed_outbound_url(url, allowlist)
                    .expect_err(url)
                    .to_string()
            };

        let private = expect_refusal_message_for_url("http://10.0.0.5/", &public_only());
        let own = expect_refusal_message_for_url("http://203.0.113.7/", &own_interface);

        assert!(
            private.contains("10.0.0.5") && private.contains("private address"),
            "{private}"
        );
        assert_eq!(own, private.replace("10.0.0.5", "203.0.113.7"));
    }

    #[tokio::test]
    async fn dns_lookup_failure_private_address_and_no_address_produce_one_refusal_message_naming_host()
     {
        fn always_failing_dns_lookup(
            _: String,
            _: u16,
        ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
            Box::pin(async { Err(std::io::Error::other("Name or service not known")) })
        }
        fn resolve_every_host_to_private_address(
            _: String,
            port: u16,
        ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
            Box::pin(async move { Ok(vec![SocketAddr::new([10, 20, 0, 7].into(), port)]) })
        }
        fn lookup_returns_empty_address_list(
            _: String,
            _: u16,
        ) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        let url: ValidatedHttpBaseUrl = "https://db.corp.internal/".parse().unwrap();

        let mut texts = Vec::new();
        for lookup in [
            always_failing_dns_lookup,
            resolve_every_host_to_private_address,
            lookup_returns_empty_address_list,
        ] {
            let refused = public_only()
                .with_dns_lookup_function(lookup)
                .allowed_socket_addresses_for_url(&url)
                .await
                .expect_err("no allowed address");
            texts.push(refused.to_string());
        }

        assert!(texts[0].contains("db.corp.internal"), "{}", texts[0]);
        assert!(texts.iter().all(|text| text == &texts[0]), "{texts:?}");
        assert!(
            public_only()
                .allowed_socket_addrs_for_host("example.com", std::iter::empty())
                .is_err()
        );
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
    fn public_only_drops_internal_addresses_of_a_mixed_resolution() {
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let resolved = [
            "10.0.0.5:443".parse().unwrap(),
            public,
            "[::1]:443".parse().unwrap(),
            "169.254.169.254:443".parse().unwrap(),
            "[64:ff9b::a9fe:a9fe]:443".parse().unwrap(),
        ];

        let kept = public_only()
            .allowed_socket_addrs_for_host("example.com", resolved.into_iter())
            .expect("one public address");

        assert_eq!(kept, vec![public]);
    }

    #[test]
    fn an_address_of_a_host_interface_is_refused_unless_the_admin_allows_it() {
        let interface: IpAddr = "203.0.113.7".parse().unwrap();
        let other_public: IpAddr = "203.0.113.8".parse().unwrap();
        let allowed_by_admin =
            UserUrlDestinationAllowlist::parse(&entries("203.0.113.7/32")).unwrap();
        let allows = |allowlist: &UserUrlDestinationAllowlist, ip: IpAddr| {
            allowlist
                .filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
                    &ip.to_string(),
                    [SocketAddr::new(ip, 443)].into_iter(),
                    || Ok(vec![interface, "127.0.0.1".parse().unwrap()]),
                )
                .is_ok()
        };

        assert!(!allows(&public_only(), interface));
        assert!(!allows(
            &public_only(),
            "::ffff:203.0.113.7".parse().unwrap()
        ));
        assert!(allows(&public_only(), other_public));
        assert!(allows(&allowed_by_admin, interface));
    }

    #[test]
    fn when_the_interface_address_reader_fails_a_public_address_is_refused_and_an_allow_listed_address_is_allowed()
     {
        let failing_reader = || Err(nix::errno::Errno::EAFNOSUPPORT);
        let public: SocketAddr = "203.0.113.8:443".parse().unwrap();
        let allowed_by_address =
            UserUrlDestinationAllowlist::parse(&entries("203.0.113.8/32")).unwrap();
        let allowed_by_name = public_only().with_added_allowed_host_or_ip("git.example");

        let refused = public_only()
            .filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
                "203.0.113.8",
                [public].into_iter(),
                failing_reader,
            );

        assert!(matches!(
            refused,
            Err(RefusedDestinationIpAddressReason::NetworkInterfaceAddressesReadFailed { .. })
        ));
        for (allowlist, host) in [
            (allowed_by_address, "203.0.113.8"),
            (allowed_by_name, "git.example"),
        ] {
            let kept = allowlist
                .filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
                    host,
                    [public].into_iter(),
                    failing_reader,
                )
                .expect("allow-listed destination");
            assert_eq!(kept, vec![public], "{host}");
        }
    }

    #[test]
    fn interface_address_list_is_read_once_for_host_name_with_several_addresses() {
        let reads = std::cell::Cell::new(0);
        let resolved: [SocketAddr; 3] = [
            "203.0.113.8:443".parse().unwrap(),
            "203.0.113.9:443".parse().unwrap(),
            "[2001:db8::8]:443".parse().unwrap(),
        ];

        let kept = public_only()
            .filter_allowed_socket_addrs_for_host_reading_host_interface_addresses_once(
                "example.com",
                resolved.into_iter(),
                || {
                    reads.set(reads.get() + 1);
                    Ok(vec!["203.0.113.9".parse().unwrap()])
                },
            )
            .expect("public addresses");

        assert_eq!(reads.get(), 1);
        assert_eq!(kept, vec![resolved[0], resolved[2]]);
    }

    #[test]
    fn odd_spellings_of_internal_addresses_are_refused() {
        for url in [
            "http://2130706433/",
            "http://0x7f.1/",
            "http://127.1/",
            "http://127.0.0.1./",
            "http://[::7f00:1]/",
            "http://[::ffff:0:7f00:1]/",
            "http://[::a9fe:a9fe]/",
            "http://[64:ff9b:1::a00:1]/",
            "http://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/",
        ] {
            let parsed = reqwest::Url::parse(url).expect("url");
            assert!(
                matches!(
                    public_only().check_destination_url_is_allowed(&parsed),
                    Err(DestinationUrlRejectedError::UnsafeAddress(_))
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn an_allow_list_host_name_with_a_trailing_dot_matches() {
        let allowlist = UserUrlDestinationAllowlist::parse(&entries("Git.Lan.")).unwrap();
        assert!(allowlist.is_host_name_on_admin_allow_list("git.lan"));
        assert!(allowlist.is_host_name_on_admin_allow_list("git.lan."));
        let added = public_only().with_added_allowed_host_or_ip("git.lan.");
        assert!(added.is_host_name_on_admin_allow_list("git.lan"));
    }

    #[test]
    fn the_interface_addresses_of_the_host_include_loopback() {
        assert!(
            get_local_network_interface_addresses()
                .expect("interface list")
                .contains(&IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
    }

    #[test]
    fn allowlist_entries_text_with_added_git_host_parses_back_to_same_allowlist() {
        let allowlist = UserUrlDestinationAllowlist::parse(&entries(
            "10.0.0.0/8,192.168.1.5,::1/128,fd00::/8,git.lan",
        ))
        .unwrap()
        .with_added_allowed_host_or_ip("gitea.example.org");

        let parsed =
            UserUrlDestinationAllowlist::parse(&allowlist.allowlist_entries_as_strings()).unwrap();

        assert_eq!(parsed.networks, allowlist.networks);
        assert_eq!(parsed.host_names, allowlist.host_names);
        assert!(parsed.is_host_name_on_admin_allow_list("gitea.example.org"));
    }

    #[tokio::test]
    async fn allowed_socket_addresses_for_host_and_port_rejects_bracketed_ipv6_loopback_private_ipv4_empty_host_and_unlisted_localhost_and_returns_allowed_literal_with_port()
     {
        let refused = |host: &'static str| async move {
            public_only()
                .allowed_socket_addresses_for_host_and_port(host, 9418)
                .await
                .unwrap_err()
        };

        assert!(matches!(
            refused("[::1]").await,
            DestinationUrlRejectedError::UnsafeAddress(ip) if ip == Ipv6Addr::LOCALHOST
        ));
        assert!(matches!(
            refused("10.0.0.5").await,
            DestinationUrlRejectedError::UnsafeAddress(_)
        ));
        assert!(matches!(
            refused("").await,
            DestinationUrlRejectedError::EmptyHost
        ));
        assert!(matches!(
            refused("localhost").await,
            DestinationUrlRejectedError::Localhost
        ));
        let allowed = UserUrlDestinationAllowlist::parse(&entries("::1/128"))
            .unwrap()
            .allowed_socket_addresses_for_host_and_port("[::1]", 9418)
            .await
            .unwrap();
        assert_eq!(allowed, vec!["[::1]:9418".parse().unwrap()]);
    }
}
