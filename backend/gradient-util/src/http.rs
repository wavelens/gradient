/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_secret::SecretString;

use crate::http_validation::{
    DestinationRuleDnsResolver, DestinationUrlRejectedError, UserUrlDestinationAllowlist,
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub const GIT_SERVER_CONNECT_AND_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub const DOWNLOAD_MAX_REDIRECTS: usize = 5;

pub fn user_agent() -> String {
    format!(
        "Gradient/{} (+https://github.com/wavelens/gradient)",
        env!("CARGO_PKG_VERSION")
    )
}

/// rustls 0.23 is refusing to pick a provider when zero or several are enabled. A TLS handshake
/// before installation is panicking. Binaries must call this before any code path opens a TLS
/// connection. A second call is returning `Err` and ignoring it is deliberate.
pub fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    roots
}

fn rustls_config(alpn: &[&[u8]]) -> rustls::ClientConfig {
    init_crypto_provider();
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls_root_store())
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();

    config
}

const NEGOTIATED: &[&[u8]] = &[b"h2", b"http/1.1"];

pub fn untimed_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .use_preconfigured_tls(rustls_config(NEGOTIATED))
        .http2_adaptive_window(true)
}

fn client_builder() -> reqwest::ClientBuilder {
    untimed_client_builder().timeout(DEFAULT_TIMEOUT)
}

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

pub fn idle_timed_download_builder(idle: Duration) -> reqwest::ClientBuilder {
    untimed_client_builder()
        .connect_timeout(idle)
        .read_timeout(idle)
}

const UPLOAD_BODY_SLICE_SIZE_BYTES: usize = 256 * 1024;

pub fn body_from_byte_chunks_with_handed_byte_count_watch(
    chunks: Vec<bytes::Bytes>,
) -> (reqwest::Body, tokio::sync::watch::Receiver<usize>) {
    let (handed_byte_count_sender, handed_byte_count) = tokio::sync::watch::channel(0);
    let mut handed = 0;
    let slices = chunks
        .into_iter()
        .flat_map(|chunk| {
            let len = chunk.len();
            (0..len)
                .step_by(UPLOAD_BODY_SLICE_SIZE_BYTES)
                .map(move |start| {
                    chunk.slice(start..(start + UPLOAD_BODY_SLICE_SIZE_BYTES).min(len))
                })
        })
        .map(move |slice| {
            handed += slice.len();
            handed_byte_count_sender.send_replace(handed);
            Ok::<_, std::convert::Infallible>(slice)
        });

    (
        reqwest::Body::wrap_stream(futures::stream::iter(slices)),
        handed_byte_count,
    )
}

pub async fn wait_for_no_progress_for_stall_window(
    stall_window: Duration,
    mut handed_byte_count: tokio::sync::watch::Receiver<usize>,
) {
    let mut deadline = tokio::time::Instant::now() + stall_window;
    while let Ok(Ok(())) = tokio::time::timeout_at(deadline, handed_byte_count.changed()).await {
        deadline = tokio::time::Instant::now() + stall_window;
    }
    tokio::time::sleep_until(deadline).await;
}

#[cfg(any(test, feature = "test-functions"))]
pub fn start_http_peer_that_handles_one_request(
    handle: impl FnOnce(std::net::TcpStream, String, Vec<u8>) + Send + 'static,
) -> String {
    use std::io::Read as _;

    let socket =
        socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).expect("socket");
    socket
        .set_recv_buffer_size(256 << 10)
        .expect("receive buffer");
    socket
        .bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .expect("bind");
    socket.listen(1).expect("listen");
    let listener = std::net::TcpListener::from(socket);
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            let n = stream.read(&mut buf).expect("read the request head");
            assert!(n > 0, "connection closed before the request head ended");
            received.extend_from_slice(&buf[..n]);
            if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let body_start = received.split_off(head_end);
        let head = String::from_utf8_lossy(&received).to_ascii_lowercase();
        handle(stream, head, body_start);
    });

    format!("http://{addr}/")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpProtocolSelection {
    Negotiated,
    Http1Only,
    Http2PriorKnowledge,
}

impl HttpProtocolSelection {
    pub fn select_http_protocol_from_cache_row(http1_only: bool) -> Self {
        if http1_only {
            Self::Http1Only
        } else {
            Self::Negotiated
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum HttpUpstreamCredential {
    Basic {
        user: String,
        password: SecretString,
    },
    Bearer(SecretString),
}

impl HttpUpstreamCredential {
    pub fn from_decrypted_payload(scheme_is_basic: bool, payload: SecretString) -> Option<Self> {
        if !scheme_is_basic {
            return Some(Self::Bearer(payload));
        }
        let (user, password) = payload.expose().split_once(':')?;
        Some(Self::Basic {
            user: user.to_owned(),
            password: SecretString::from(password),
        })
    }

    pub fn to_payload(&self) -> SecretString {
        match self {
            Self::Basic { user, password } => {
                SecretString::new(format!("{user}:{}", password.expose()))
            }
            Self::Bearer(token) => token.clone(),
        }
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::Basic { user, password } => request.basic_auth(user, Some(password.expose())),
            Self::Bearer(token) => request.bearer_auth(token.expose()),
        }
    }
}

impl std::fmt::Debug for HttpUpstreamCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Basic { .. } => f.write_str("HttpUpstreamCredential::Basic([REDACTED])"),
            Self::Bearer(_) => f.write_str("HttpUpstreamCredential::Bearer([REDACTED])"),
        }
    }
}

/// All connections to URLs from project members go through the admin allowlist. Host names pass
/// through a filter in the resolver. Checks at request time and at redirect time target IP
/// literals. reqwest doesn't resolve an IP literal.
#[derive(Clone)]
pub struct HttpClientForUserSuppliedUrls {
    allowlist: Arc<UserUrlDestinationAllowlist>,
    api: reqwest::Client,
    negotiated_download: reqwest::Client,
    http1_download: reqwest::Client,
    http2_download: reqwest::Client,
    git_smart_http_client: reqwest::Client,
}

impl std::fmt::Debug for HttpClientForUserSuppliedUrls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClientForUserSuppliedUrls")
            .field("allowlist", &self.allowlist)
            .finish_non_exhaustive()
    }
}

impl HttpClientForUserSuppliedUrls {
    pub fn new(allowlist: UserUrlDestinationAllowlist) -> reqwest::Result<Self> {
        let allowlist = Arc::new(allowlist);
        let filtered = |builder: reqwest::ClientBuilder| {
            builder
                .dns_resolver(DestinationRuleDnsResolver(Arc::clone(&allowlist)))
                .no_proxy()
        };
        let download =
            |protocol: HttpProtocolSelection, idle: Duration, redirect_hosts: RedirectHostScope| {
                let builder = filtered(idle_timed_download_builder(idle)).redirect(
                    download_redirect_policy_refusing_long_downgrade_or_disallowed_hops(
                        Arc::clone(&allowlist),
                        redirect_hosts,
                    ),
                );
                match protocol {
                    HttpProtocolSelection::Negotiated => builder,
                    HttpProtocolSelection::Http1Only => builder
                        .use_preconfigured_tls(rustls_config(&[b"http/1.1"]))
                        .http1_only(),
                    HttpProtocolSelection::Http2PriorKnowledge => builder
                        .use_preconfigured_tls(rustls_config(&[b"h2"]))
                        .http2_prior_knowledge(),
                }
                .build()
            };

        Ok(Self {
            api: filtered(client_builder())
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            negotiated_download: download(
                HttpProtocolSelection::Negotiated,
                DEFAULT_TIMEOUT,
                RedirectHostScope::AnyHostInAllowlist,
            )?,
            http1_download: download(
                HttpProtocolSelection::Http1Only,
                DEFAULT_TIMEOUT,
                RedirectHostScope::AnyHostInAllowlist,
            )?,
            http2_download: download(
                HttpProtocolSelection::Http2PriorKnowledge,
                DEFAULT_TIMEOUT,
                RedirectHostScope::AnyHostInAllowlist,
            )?,
            git_smart_http_client: download(
                HttpProtocolSelection::Negotiated,
                GIT_SERVER_CONNECT_AND_READ_IDLE_TIMEOUT,
                RedirectHostScope::StayOnFirstRequestedHost,
            )?,
            allowlist,
        })
    }

    pub fn allowlist(&self) -> &UserUrlDestinationAllowlist {
        &self.allowlist
    }

    pub fn request_builder_for_checked_url(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, DestinationUrlRejectedError> {
        let url =
            crate::http_validation::parse_and_check_allowed_outbound_url(url, &self.allowlist)?;
        Ok(self.api.request(method, url))
    }

    /// Attic, Cachix and S3 gateways are answering a NAR GET with a 3xx to their object storage,
    /// so downloads follow redirects that pass the allowlist.
    pub fn download_get_request_builder(
        &self,
        url: &str,
        protocol: HttpProtocolSelection,
        credential: Option<&HttpUpstreamCredential>,
    ) -> Result<reqwest::RequestBuilder, DestinationUrlRejectedError> {
        let client = match protocol {
            HttpProtocolSelection::Negotiated => &self.negotiated_download,
            HttpProtocolSelection::Http1Only => &self.http1_download,
            HttpProtocolSelection::Http2PriorKnowledge => &self.http2_download,
        };
        self.check_url_against_allowlist_and_build_request_with_optional_credential(
            client,
            reqwest::Method::GET,
            url,
            credential,
        )
    }

    pub fn build_allowed_git_server_request(
        &self,
        method: reqwest::Method,
        url: &str,
        credential: Option<&HttpUpstreamCredential>,
    ) -> Result<reqwest::RequestBuilder, DestinationUrlRejectedError> {
        self.check_url_against_allowlist_and_build_request_with_optional_credential(
            &self.git_smart_http_client,
            method,
            url,
            credential,
        )
    }

    fn check_url_against_allowlist_and_build_request_with_optional_credential(
        &self,
        client: &reqwest::Client,
        method: reqwest::Method,
        url: &str,
        credential: Option<&HttpUpstreamCredential>,
    ) -> Result<reqwest::RequestBuilder, DestinationUrlRejectedError> {
        let url =
            crate::http_validation::parse_and_check_allowed_outbound_url(url, &self.allowlist)?;
        let request = client.request(method, url);
        Ok(match credential {
            Some(credential) => credential.authorize(request),
            None => request,
        })
    }
}

#[derive(Clone, Copy)]
enum RedirectHostScope {
    AnyHostInAllowlist,
    StayOnFirstRequestedHost,
}

#[derive(Debug, thiserror::Error)]
#[error("redirect to another host blocked")]
struct RedirectToAnotherHostBlockedError;

fn download_redirect_policy_refusing_long_downgrade_or_disallowed_hops(
    allowlist: Arc<UserUrlDestinationAllowlist>,
    redirect_hosts: RedirectHostScope,
) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > DOWNLOAD_MAX_REDIRECTS {
            return attempt.error(DestinationUrlRejectedError::TooManyRedirects(
                DOWNLOAD_MAX_REDIRECTS,
            ));
        }
        let from_https = attempt
            .previous()
            .last()
            .is_some_and(|previous| previous.scheme() == "https");
        if from_https && attempt.url().scheme() == "http" {
            return attempt.error(DestinationUrlRejectedError::HttpsToHttpRedirect);
        }
        if let Err(refused) = allowlist.check_destination_url_is_allowed(attempt.url()) {
            return attempt.error(refused);
        }
        let leaves_first_requested_host = attempt
            .previous()
            .first()
            .is_some_and(|first| first.host_str() != attempt.url().host_str());
        match (redirect_hosts, leaves_first_requested_host) {
            (RedirectHostScope::StayOnFirstRequestedHost, true) => {
                attempt.error(RedirectToAnotherHostBlockedError)
            }
            _ => attempt.follow(),
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ResponseBodyReadWithLimitError {
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error("Response body larger than {0} bytes")]
    TooLarge(usize),
}

pub async fn read_response_body_with_limit(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ResponseBodyReadWithLimitError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ResponseBodyReadWithLimitError::TooLarge(limit));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err(ResponseBodyReadWithLimitError::TooLarge(limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_validation::HostHasNoAllowedAddressError;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn init_crypto_provider_is_idempotent_and_enables_tls() {
        init_crypto_provider();
        init_crypto_provider();

        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }

    fn trickling_server(bytes: usize, gap: Duration) -> String {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.read(&mut [0u8; 4096]);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\n\r\n");
            for _ in 0..bytes {
                std::thread::sleep(gap);
                if stream.write_all(b"x").is_err() {
                    return;
                }
            }
        });

        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn download_outlasting_its_idle_timeout_completes_while_bytes_keep_arriving() {
        let url = trickling_server(5, Duration::from_millis(300));
        let client = idle_timed_download_builder(Duration::from_millis(800))
            .build()
            .expect("client");

        let body = client
            .get(url)
            .send()
            .await
            .expect("response")
            .bytes()
            .await
            .expect("body");

        assert_eq!(body.as_ref(), b"xxxxx");
    }

    #[tokio::test]
    async fn download_stalled_past_its_idle_timeout_fails() {
        let url = trickling_server(1, Duration::from_secs(3));
        let client = idle_timed_download_builder(Duration::from_millis(300))
            .build()
            .expect("client");

        let err = match client.get(url).send().await {
            Ok(response) => response.bytes().await.expect_err("stalled body"),
            Err(err) => err,
        };

        assert!(err.is_timeout(), "{err:?}");
    }

    async fn answering_server(status: u16, body: &'static str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    fn client_allowing(entries: &str) -> HttpClientForUserSuppliedUrls {
        let entries: Vec<String> = entries.split(',').map(str::to_owned).collect();
        HttpClientForUserSuppliedUrls::new(
            UserUrlDestinationAllowlist::parse(&entries).expect("allowlist"),
        )
        .expect("client")
    }

    async fn received(server: &MockServer) -> usize {
        server.received_requests().await.unwrap_or_default().len()
    }

    #[tokio::test]
    async fn the_public_only_client_refuses_a_loopback_literal_before_the_request() {
        let server = answering_server(200, "internal").await;
        let client = client_allowing("");
        let url = format!("http://127.0.0.1:{}/", server.address().port());

        let api = client.request_builder_for_checked_url(reqwest::Method::GET, &url);
        let download =
            client.download_get_request_builder(&url, HttpProtocolSelection::Negotiated, None);

        assert!(matches!(
            api,
            Err(DestinationUrlRejectedError::UnsafeAddress(_))
        ));
        assert!(matches!(
            download,
            Err(DestinationUrlRejectedError::UnsafeAddress(_))
        ));
        assert_eq!(received(&server).await, 0);
    }

    #[tokio::test]
    async fn the_public_only_client_refuses_a_name_that_resolves_to_loopback() {
        let server = answering_server(200, "internal").await;
        let client = client_allowing("");
        let url = format!("http://localhost.:{}/hook", server.address().port());

        let api = client
            .request_builder_for_checked_url(reqwest::Method::GET, &url)
            .expect("the name passes the early check")
            .send()
            .await;
        let download = client
            .download_get_request_builder(&url, HttpProtocolSelection::Http1Only, None)
            .expect("the name passes the early check")
            .send()
            .await;

        for refused in [api, download] {
            let refused = format!("{:?}", refused.expect_err("loopback target"));
            assert!(
                refused.contains("HostHasNoAllowedAddressError"),
                "{refused}"
            );
        }
        assert_eq!(received(&server).await, 0);
    }

    #[tokio::test]
    async fn an_allow_listed_address_and_host_name_are_reachable() {
        let server = answering_server(200, "allowed").await;
        let port = server.address().port();

        let by_address = client_allowing("127.0.0.0/8")
            .request_builder_for_checked_url(
                reqwest::Method::GET,
                &format!("http://127.0.0.1:{port}/"),
            )
            .expect("allowed address")
            .send()
            .await
            .expect("response");
        let by_name = client_allowing("localhost")
            .download_get_request_builder(
                &format!("http://localhost:{port}/"),
                HttpProtocolSelection::Negotiated,
                None,
            )
            .expect("allowed host name")
            .send()
            .await
            .expect("response");

        assert_eq!(by_address.text().await.expect("body"), "allowed");
        assert_eq!(by_name.text().await.expect("body"), "allowed");
    }

    fn error_chain_holds<E: std::error::Error + 'static>(
        error: &reqwest::Error,
        wanted: impl Fn(&E) -> bool,
    ) -> bool {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(current) = source {
            if current.downcast_ref::<E>().is_some_and(&wanted) {
                return true;
            }
            source = current.source();
        }
        false
    }

    #[tokio::test]
    async fn a_download_redirect_to_a_refused_address_is_not_followed() {
        let internal = answering_server(200, "internal").await;
        let internal_port = internal.address().port();
        type RequestRefusalErrorPredicate = fn(&reqwest::Error) -> bool;
        let cases: [(String, RequestRefusalErrorPredicate); 4] = [
            (format!("http://127.0.0.1:{internal_port}/"), |e| {
                error_chain_holds(e, |r: &DestinationUrlRejectedError| {
                    matches!(r, DestinationUrlRejectedError::UnsafeAddress(_))
                })
            }),
            (format!("http://[::ffff:127.0.0.1]:{internal_port}/"), |e| {
                error_chain_holds(e, |r: &DestinationUrlRejectedError| {
                    matches!(r, DestinationUrlRejectedError::UnsafeAddress(_))
                })
            }),
            (format!("http://localhost:{internal_port}/"), |e| {
                error_chain_holds(e, |r: &DestinationUrlRejectedError| {
                    matches!(r, DestinationUrlRejectedError::Localhost)
                })
            }),
            (format!("http://localhost.:{internal_port}/"), |e| {
                error_chain_holds(e, |_: &HostHasNoAllowedAddressError| true)
            }),
        ];
        for (location, expected_reason) in cases {
            let redirecting = MockServer::builder()
                .listener(std::net::TcpListener::bind("127.0.0.2:0").expect("bind 127.0.0.2"))
                .start()
                .await;
            Mock::given(any())
                .respond_with(
                    ResponseTemplate::new(302).insert_header("Location", location.as_str()),
                )
                .mount(&redirecting)
                .await;

            let error = client_allowing("127.0.0.2/32")
                .download_get_request_builder(
                    &format!("http://127.0.0.2:{}/nar", redirecting.address().port()),
                    HttpProtocolSelection::Negotiated,
                    None,
                )
                .expect("allowed first hop")
                .send()
                .await
                .expect_err("the redirect is refused");

            assert!(expected_reason(&error), "{location}: {error:?}");
            assert_eq!(received(&redirecting).await, 1, "{location}");
        }
        assert_eq!(received(&internal).await, 0);
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "Set HTTP_PROXY in the process for this test. The aim is to prove that the client does not use it. nextest executions use a separate process for each test. Other tests never see that change."
    )]
    async fn an_environment_proxy_is_not_used() {
        let proxy = answering_server(200, "proxied").await;
        let target = answering_server(200, "direct").await;
        let proxy_url = format!("http://127.0.0.1:{}", proxy.address().port());
        unsafe {
            std::env::set_var("HTTP_PROXY", &proxy_url);
            std::env::set_var("http_proxy", &proxy_url);
        }
        let client = client_allowing("127.0.0.0/8");
        unsafe {
            std::env::remove_var("HTTP_PROXY");
            std::env::remove_var("http_proxy");
        }

        let body = client
            .download_get_request_builder(
                &format!("http://127.0.0.1:{}/", target.address().port()),
                HttpProtocolSelection::Negotiated,
                None,
            )
            .expect("allowed address")
            .send()
            .await
            .expect("response")
            .text()
            .await
            .expect("body");

        assert_eq!(body, "direct");
        assert_eq!(received(&proxy).await, 0);
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_refused() {
        let server = answering_server(200, "0123456789").await;
        let client = client_allowing("127.0.0.0/8");
        let fetch = || async {
            client
                .request_builder_for_checked_url(
                    reqwest::Method::GET,
                    &format!("http://127.0.0.1:{}/", server.address().port()),
                )
                .expect("allowed")
                .send()
                .await
                .expect("response")
        };

        let fits = read_response_body_with_limit(fetch().await, 10).await;
        let too_large = read_response_body_with_limit(fetch().await, 9).await;

        assert_eq!(fits.expect("within the limit"), b"0123456789");
        assert!(matches!(
            too_large,
            Err(ResponseBodyReadWithLimitError::TooLarge(9))
        ));
    }

    async fn authorization_headers_received(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|request| request.headers.get(reqwest::header::AUTHORIZATION))
            .map(|value| value.to_str().expect("ascii header").to_owned())
            .collect()
    }

    async fn redirecting_server_on(ip: &str, location: &str) -> MockServer {
        let server = MockServer::builder()
            .listener(std::net::TcpListener::bind(format!("{ip}:0")).expect("bind loopback"))
            .start()
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(302).insert_header("Location", location))
            .mount(&server)
            .await;
        server
    }

    fn basic_credential() -> HttpUpstreamCredential {
        HttpUpstreamCredential::Basic {
            user: "alice".to_owned(),
            password: SecretString::from("basic-password-value"),
        }
    }

    fn bearer_credential() -> HttpUpstreamCredential {
        HttpUpstreamCredential::Bearer(SecretString::from("bearer-token-value"))
    }

    async fn fetch_with_credential(
        url: &str,
        allowlist: &str,
        credential: &HttpUpstreamCredential,
    ) -> reqwest::Response {
        client_allowing(allowlist)
            .download_get_request_builder(url, HttpProtocolSelection::Negotiated, Some(credential))
            .expect("allowed address")
            .send()
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn a_stored_credential_reaches_the_upstream() {
        let server = answering_server(200, "nar").await;
        let url = format!("http://127.0.0.1:{}/nar", server.address().port());

        fetch_with_credential(&url, "127.0.0.0/8", &basic_credential()).await;
        fetch_with_credential(&url, "127.0.0.0/8", &bearer_credential()).await;

        assert_eq!(
            authorization_headers_received(&server).await,
            [
                "Basic YWxpY2U6YmFzaWMtcGFzc3dvcmQtdmFsdWU=",
                "Bearer bearer-token-value"
            ]
        );
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_drops_the_credential() {
        let target = MockServer::builder()
            .listener(std::net::TcpListener::bind("127.0.0.3:0").expect("bind 127.0.0.3"))
            .start()
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string("object"))
            .mount(&target)
            .await;
        let location = format!("http://127.0.0.3:{}/object", target.address().port());
        let redirecting = redirecting_server_on("127.0.0.2", &location).await;
        let url = format!("http://127.0.0.2:{}/nar", redirecting.address().port());

        for credential in [basic_credential(), bearer_credential()] {
            let response =
                fetch_with_credential(&url, "127.0.0.2/32,127.0.0.3/32", &credential).await;
            assert_eq!(response.text().await.expect("body"), "object");
        }

        assert_eq!(authorization_headers_received(&redirecting).await.len(), 2);
        assert_eq!(received(&target).await, 2);
        assert!(authorization_headers_received(&target).await.is_empty());
    }

    #[tokio::test]
    async fn a_redirect_to_another_port_of_the_same_host_drops_the_credential() {
        let target = MockServer::builder()
            .listener(std::net::TcpListener::bind("127.0.0.2:0").expect("bind 127.0.0.2"))
            .start()
            .await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string("object"))
            .mount(&target)
            .await;
        let location = format!("http://127.0.0.2:{}/object", target.address().port());
        let redirecting = redirecting_server_on("127.0.0.2", &location).await;
        let url = format!("http://127.0.0.2:{}/nar", redirecting.address().port());

        for credential in [basic_credential(), bearer_credential()] {
            let response = fetch_with_credential(&url, "127.0.0.2/32", &credential).await;
            assert_eq!(response.text().await.expect("body"), "object");
        }

        assert_eq!(authorization_headers_received(&redirecting).await.len(), 2);
        assert_eq!(received(&target).await, 2);
        assert!(authorization_headers_received(&target).await.is_empty());
    }

    #[test]
    fn the_debug_output_of_a_credential_holds_no_secret() {
        let basic = format!("{:?}", basic_credential());
        let bearer = format!("{:?}", bearer_credential());

        assert!(!basic.contains("basic-password-value"), "{basic}");
        assert!(!basic.contains("alice"), "{basic}");
        assert!(!bearer.contains("bearer-token-value"), "{bearer}");
    }

    #[test]
    fn a_credential_survives_the_payload_round_trip() {
        for credential in [
            basic_credential(),
            bearer_credential(),
            HttpUpstreamCredential::Basic {
                user: "bob".to_owned(),
                password: SecretString::from("with:colon"),
            },
        ] {
            let scheme_is_basic = matches!(credential, HttpUpstreamCredential::Basic { .. });
            let restored = HttpUpstreamCredential::from_decrypted_payload(
                scheme_is_basic,
                credential.to_payload(),
            );
            assert_eq!(restored, Some(credential));
        }

        assert_eq!(
            HttpUpstreamCredential::from_decrypted_payload(true, SecretString::from("no-colon")),
            None
        );
    }
}
