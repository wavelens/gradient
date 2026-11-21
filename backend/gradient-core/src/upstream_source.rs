/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_entity::cache_upstream::{
    CacheUpstreamKind, Model as MCacheUpstream, UpstreamCredentialScheme,
};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_sources::SourceError;
use gradient_types::ids::CacheUpstreamId;
use gradient_util::error_cause_chain_formatting::error_chain;
use gradient_util::http::{
    HttpClientForUserSuppliedUrls, HttpProtocolSelection, HttpUpstreamCredential,
    read_response_body_with_limit,
};
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use tokio::sync::{OnceCell, Semaphore};

use crate::upstream::{SampleKind, breakers};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;
const UPSTREAM_NIX_CACHE_INFO_PROBE_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSource {
    pub id: CacheUpstreamId,
    pub url: ValidatedHttpBaseUrl,
    pub http1_only: bool,
    pub credential: Option<Arc<HttpUpstreamCredential>>,
}

pub fn substitutes_from(upstream: &MCacheUpstream) -> bool {
    upstream.kind == CacheUpstreamKind::Http
        && upstream.mode != CacheSubscriptionMode::WriteOnly
        && upstream.url.is_some()
}

const MAX_UPSTREAM_CACHE_CREDENTIALS_IN_MEMORY: usize = 4096;
const MAX_CONCURRENT_UPSTREAM_CACHE_CREDENTIAL_DECRYPTIONS: usize = 4;
const FAILED_UPSTREAM_CACHE_CREDENTIAL_DECRYPTION_RETRY_DELAY: Duration = Duration::from_secs(60);

struct UpstreamCacheCredentialDecryptionAttempt {
    crypt_file: String,
    ciphertext: String,
    scheme_is_basic: bool,
    credential: OnceCell<Arc<HttpUpstreamCredential>>,
    last_decryption_failure_time: Mutex<Option<Instant>>,
}

impl UpstreamCacheCredentialDecryptionAttempt {
    fn has_same_key_file_ciphertext_and_scheme(
        &self,
        crypt_file: &str,
        ciphertext: &str,
        scheme_is_basic: bool,
    ) -> bool {
        self.crypt_file == crypt_file
            && self.ciphertext == ciphertext
            && self.scheme_is_basic == scheme_is_basic
    }

    fn last_failed_decryption_happened_within(&self, period: Duration) -> bool {
        self.last_decryption_failure_time
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|failed_at| failed_at.elapsed() < period)
    }

    fn store_current_time_as_last_decryption_failure_time(&self) {
        *self
            .last_decryption_failure_time
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }
}

struct DecryptedUpstreamCacheCredentialCacheEntryWithDecryptionAttemptAndLastUseCounter {
    decryption: Arc<UpstreamCacheCredentialDecryptionAttempt>,
    most_recent_use_counter_value: u64,
}

#[derive(Default)]
struct UpstreamCacheEntriesAndUseCount {
    entries: HashMap<
        CacheUpstreamId,
        DecryptedUpstreamCacheCredentialCacheEntryWithDecryptionAttemptAndLastUseCounter,
    >,
    cache_lookup_counter: u64,
}

#[derive(Default)]
struct RunningAndPeakDecryptionCounts {
    now: AtomicUsize,
    highest_concurrent_credential_decryption_count: AtomicUsize,
}

/// Operations that decrypt credentials use Argon2id. Only successful decryptions stay in memory. A few decryptions run at the same time outside the async threads. Failed decryptions only get a retry, after `failed_decryption_retry_block_duration`.
struct DecryptedUpstreamCacheCredentialCache {
    by_upstream: Mutex<UpstreamCacheEntriesAndUseCount>,
    maximum_entry_count: usize,
    failed_decryption_retry_block_duration: Duration,
    credential_decryption_semaphore: Arc<Semaphore>,
    credential_decryption_count: AtomicUsize,
    decryption_counters: Arc<RunningAndPeakDecryptionCounts>,
}

impl DecryptedUpstreamCacheCredentialCache {
    fn new() -> Self {
        Self::with_limits(
            MAX_UPSTREAM_CACHE_CREDENTIALS_IN_MEMORY,
            FAILED_UPSTREAM_CACHE_CREDENTIAL_DECRYPTION_RETRY_DELAY,
        )
    }

    fn with_limits(
        maximum_entry_count: usize,
        failed_decryption_retry_block_duration: Duration,
    ) -> Self {
        Self {
            by_upstream: Mutex::new(UpstreamCacheEntriesAndUseCount::default()),
            maximum_entry_count,
            failed_decryption_retry_block_duration,
            credential_decryption_semaphore: Arc::new(Semaphore::new(
                MAX_CONCURRENT_UPSTREAM_CACHE_CREDENTIAL_DECRYPTIONS,
            )),
            credential_decryption_count: AtomicUsize::new(0),
            decryption_counters: Arc::new(RunningAndPeakDecryptionCounts::default()),
        }
    }

    async fn credential_of(
        &self,
        crypt_file: &str,
        upstream: &MCacheUpstream,
    ) -> Result<Option<Arc<HttpUpstreamCredential>>, SourceError> {
        let Some(ciphertext) = upstream.encrypted_credential.as_deref() else {
            return Ok(None);
        };
        let scheme_is_basic = upstream.credential_scheme == Some(UpstreamCredentialScheme::Basic);
        let decryption = self.get_or_insert_decryption_entry(
            upstream.id,
            crypt_file,
            ciphertext,
            scheme_is_basic,
        );
        decryption
            .credential
            .get_or_try_init(|| self.decrypt(&decryption))
            .await
            .map(|credential| Some(Arc::clone(credential)))
    }

    async fn decrypt(
        &self,
        decryption: &UpstreamCacheCredentialDecryptionAttempt,
    ) -> Result<Arc<HttpUpstreamCredential>, SourceError> {
        if decryption
            .last_failed_decryption_happened_within(self.failed_decryption_retry_block_duration)
        {
            return Err(SourceError::CryptographicOperation);
        }
        let permit = Arc::clone(&self.credential_decryption_semaphore)
            .acquire_owned()
            .await
            .map_err(|_| SourceError::CryptographicOperation)?;
        self.credential_decryption_count
            .fetch_add(1, Ordering::Relaxed);
        let running = Arc::clone(&self.decryption_counters);
        let crypt_file = decryption.crypt_file.clone();
        let ciphertext = decryption.ciphertext.clone();
        let scheme_is_basic = decryption.scheme_is_basic;
        let decrypted = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let now = running.now.fetch_add(1, Ordering::SeqCst) + 1;
            running
                .highest_concurrent_credential_decryption_count
                .fetch_max(now, Ordering::SeqCst);
            let credential = gradient_sources::decrypt_secret(&crypt_file, &ciphertext)
                .ok()
                .and_then(|payload| {
                    HttpUpstreamCredential::from_decrypted_payload(scheme_is_basic, payload)
                })
                .map(Arc::new);
            running.now.fetch_sub(1, Ordering::SeqCst);
            credential
        })
        .await
        .ok()
        .flatten();
        decrypted.ok_or_else(|| {
            decryption.store_current_time_as_last_decryption_failure_time();
            SourceError::CryptographicOperation
        })
    }

    fn get_or_insert_decryption_entry(
        &self,
        upstream: CacheUpstreamId,
        crypt_file: &str,
        ciphertext: &str,
        scheme_is_basic: bool,
    ) -> Arc<UpstreamCacheCredentialDecryptionAttempt> {
        let mut kept = self
            .by_upstream
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        kept.cache_lookup_counter += 1;
        let cache_lookup_counter = kept.cache_lookup_counter;
        if let Some(known) = kept.entries.get_mut(&upstream)
            && known.decryption.has_same_key_file_ciphertext_and_scheme(
                crypt_file,
                ciphertext,
                scheme_is_basic,
            )
        {
            known.most_recent_use_counter_value = cache_lookup_counter;
            return Arc::clone(&known.decryption);
        }
        if !kept.entries.contains_key(&upstream) && kept.entries.len() >= self.maximum_entry_count {
            let used_longest_ago = kept
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.most_recent_use_counter_value)
                .map(|(id, _)| *id);
            if let Some(id) = used_longest_ago {
                kept.entries.remove(&id);
            }
        }
        let decryption = Arc::new(UpstreamCacheCredentialDecryptionAttempt {
            crypt_file: crypt_file.to_owned(),
            ciphertext: ciphertext.to_owned(),
            scheme_is_basic,
            credential: OnceCell::new(),
            last_decryption_failure_time: Mutex::new(None),
        });
        kept.entries.insert(
            upstream,
            DecryptedUpstreamCacheCredentialCacheEntryWithDecryptionAttemptAndLastUseCounter {
                decryption: Arc::clone(&decryption),
                most_recent_use_counter_value: cache_lookup_counter,
            },
        );
        decryption
    }
}

fn process_wide_decrypted_upstream_cache_credentials_cache()
-> &'static DecryptedUpstreamCacheCredentialCache {
    static DECRYPTED: OnceLock<DecryptedUpstreamCacheCredentialCache> = OnceLock::new();
    DECRYPTED.get_or_init(DecryptedUpstreamCacheCredentialCache::new)
}

pub async fn credential_of_upstream_row(
    crypt_file: &str,
    upstream: &MCacheUpstream,
) -> Result<Option<Arc<HttpUpstreamCredential>>, SourceError> {
    process_wide_decrypted_upstream_cache_credentials_cache()
        .credential_of(crypt_file, upstream)
        .await
}

pub async fn substitution_sources(
    upstream_caches: &[MCacheUpstream],
    crypt_file: &str,
) -> Vec<UpstreamSource> {
    let mut sources = Vec::new();
    for upstream in upstream_caches.iter().filter(|u| substitutes_from(u)) {
        let Some(url) = upstream.url.as_deref().and_then(|stored| {
            gradient_db::caches::upstream::parse_stored_upstream_cache_url(upstream.id, stored)
        }) else {
            continue;
        };
        let Ok(credential) = credential_of_upstream_row(crypt_file, upstream).await else {
            tracing::warn!(upstream = %upstream.id, "can't decrypt credentials for the upstream cache. skipping this upstream.");
            continue;
        };
        sources.push(UpstreamSource {
            id: upstream.id,
            url,
            http1_only: upstream.http1_only,
            credential,
        });
    }
    sources
}

pub struct UpstreamObject {
    pub content_length: Option<u64>,
    pub body: BoxStream<'static, reqwest::Result<Bytes>>,
}

/// Redirects are followed because Attic, Cachix and S3 gateways are answering object GETs with a
/// 3xx.
pub async fn fetch_from_upstream_caches(
    http: &HttpClientForUserSuppliedUrls,
    sources: &[UpstreamSource],
    path: &str,
    timeout: Option<Duration>,
) -> Option<UpstreamObject> {
    for source in sources {
        if !breakers().allows(source.id) {
            continue;
        }
        let url = source.url.append_api_path_to_base_url(path);
        let protocol =
            HttpProtocolSelection::select_http_protocol_from_cache_row(source.http1_only);
        let Ok(request) =
            http.download_get_request_builder(&url, protocol, source.credential.as_deref())
        else {
            breakers().record(source.id, SampleKind::Error);
            continue;
        };
        let response = match timeout {
            Some(t) => request.timeout(t),
            None => request,
        }
        .send()
        .await;
        let kind = match &response {
            Ok(r) if r.status().is_success() => SampleKind::Hit,
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => SampleKind::Miss,
            Ok(_) | Err(_) => SampleKind::Error,
        };
        breakers().record(source.id, kind);
        if kind == SampleKind::Hit {
            let response = response.ok()?;
            return Some(UpstreamObject {
                content_length: response.content_length(),
                body: response.bytes_stream().boxed(),
            });
        }
    }
    None
}

pub async fn fetch_upstream_log(
    http: &HttpClientForUserSuppliedUrls,
    sources: &[UpstreamSource],
    drv: &str,
) -> Option<String> {
    let path = format!("log/{drv}");
    for source in sources {
        let Some(object) = fetch_from_upstream_caches(
            http,
            std::slice::from_ref(source),
            &path,
            Some(LOG_FETCH_TIMEOUT),
        )
        .await
        else {
            continue;
        };
        if let Some(body) = read_capped(object.body).await {
            return Some(body);
        }
    }
    None
}

async fn read_capped(mut stream: BoxStream<'static, reqwest::Result<Bytes>>) -> Option<String> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        let room = LOG_FETCH_MAX_BYTES.saturating_sub(bytes.len());
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return None;
    }
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        body.push_str("\n[truncated]\n");
    }
    Some(body)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct ProtocolProbe {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

pub async fn probe_protocol(
    http: &HttpClientForUserSuppliedUrls,
    base_url: &ValidatedHttpBaseUrl,
    protocol: HttpProtocolSelection,
    credential: Option<&HttpUpstreamCredential>,
) -> ProtocolProbe {
    let started = std::time::Instant::now();
    let outcome = fetch_cache_info(http, base_url, protocol, credential).await;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match outcome {
        Ok(()) => ProtocolProbe {
            ok: true,
            status: Some(200),
            latency_ms,
            error: None,
        },
        Err((status, error)) => ProtocolProbe {
            ok: false,
            status,
            latency_ms,
            error: Some(error),
        },
    }
}

async fn fetch_cache_info(
    http: &HttpClientForUserSuppliedUrls,
    base_url: &ValidatedHttpBaseUrl,
    protocol: HttpProtocolSelection,
    credential: Option<&HttpUpstreamCredential>,
) -> Result<(), (Option<u16>, String)> {
    let url = base_url.append_api_path_to_base_url("nix-cache-info");
    let response = http
        .download_get_request_builder(&url, protocol, credential)
        .map_err(|refused| (None, refused.to_string()))?
        .timeout(PROTOCOL_PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| (None, send_failure_reason(&e)))?;
    let status = response.status();
    if !status.is_success() {
        return Err((Some(status.as_u16()), format!("HTTP {status}")));
    }
    let body = read_response_body_with_limit(response, UPSTREAM_NIX_CACHE_INFO_PROBE_MAX_BYTES)
        .await
        .map_err(|e| (Some(status.as_u16()), error_chain(&e)))?;
    let body = String::from_utf8_lossy(&body);
    if !body.lines().any(|l| l.starts_with("StoreDir:")) {
        return Err((
            Some(status.as_u16()),
            "nix-cache-info has no StoreDir; not a binary cache".into(),
        ));
    }
    Ok(())
}

fn send_failure_reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timed out".into();
    }
    let causes: Vec<&(dyn std::error::Error + 'static)> =
        std::iter::successors(Some(error as &(dyn std::error::Error + 'static)), |e| {
            e.source()
        })
        .collect();
    for cause in &causes {
        match cause
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
        {
            Some(std::io::ErrorKind::ConnectionRefused) => return "connection refused".into(),
            Some(std::io::ErrorKind::TimedOut) => return "timed out".into(),
            _ if cause.to_string().starts_with("dns error") => {
                return "DNS lookup failed".into();
            }
            _ => {}
        }
    }
    match causes.last() {
        Some(cause) if error.is_connect() => format!("connection failed: {cause}"),
        _ => error_chain(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::SampleKind;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn bind_reserved_loopback_tcp_socket_and_port() -> (tokio::net::TcpSocket, u16) {
        let socket = tokio::net::TcpSocket::new_v4().expect("a TCP socket");
        socket
            .bind("127.0.0.1:0".parse().expect("a socket address"))
            .expect("a free port");
        let port = socket.local_addr().expect("a bound address").port();
        (socket, port)
    }

    const DRV: &str = "1mpqffikzpszxw6zzi8s63a3srqd6swx-python3.14-ctranslate2-4.8.1.drv";

    fn row(n: u128, kind: CacheUpstreamKind, mode: CacheSubscriptionMode) -> MCacheUpstream {
        MCacheUpstream {
            id: CacheUpstreamId::new(uuid::Uuid::from_u128(n)),
            kind,
            mode,
            url: Some(format!("https://u{n}.example")),
            ..Default::default()
        }
    }

    fn source(n: u128, url: String) -> UpstreamSource {
        UpstreamSource {
            id: CacheUpstreamId::new(uuid::Uuid::from_u128(n)),
            url: url.parse().unwrap(),
            http1_only: false,
            credential: None,
        }
    }

    async fn log_upstream(body: Option<&str>) -> MockServer {
        let server = MockServer::start().await;
        let response = match body {
            Some(b) => ResponseTemplate::new(200).set_body_string(b),
            None => ResponseTemplate::new(404),
        };
        Mock::given(method("GET"))
            .and(path(format!("/log/{DRV}")))
            .respond_with(response)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn only_readable_http_upstream_caches_are_substituted_from() {
        let rows = vec![
            row(1, CacheUpstreamKind::Http, CacheSubscriptionMode::ReadWrite),
            row(2, CacheUpstreamKind::Http, CacheSubscriptionMode::ReadOnly),
            row(3, CacheUpstreamKind::Http, CacheSubscriptionMode::WriteOnly),
            row(
                4,
                CacheUpstreamKind::GradientProto,
                CacheSubscriptionMode::ReadOnly,
            ),
            row(
                5,
                CacheUpstreamKind::Internal,
                CacheSubscriptionMode::ReadOnly,
            ),
            MCacheUpstream {
                url: None,
                ..row(6, CacheUpstreamKind::Http, CacheSubscriptionMode::ReadOnly)
            },
        ];

        let ids: Vec<u128> = substitution_sources(&rows, "/nonexistent/gradient-crypt")
            .await
            .into_iter()
            .map(|s| s.id.into_inner().as_u128())
            .collect();

        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn fetch_upstream_log_reads_the_first_non_empty_body() {
        let without = log_upstream(None).await;
        let empty = log_upstream(Some("")).await;
        let with = log_upstream(Some("building\n")).await;

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(20, without.uri()), source(21, with.uri())],
            DRV,
        )
        .await;
        assert_eq!(log.as_deref(), Some("building\n"));

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(22, empty.uri()), source(23, with.uri())],
            DRV,
        )
        .await;
        assert_eq!(log.as_deref(), Some("building\n"));
    }

    #[tokio::test]
    async fn fetch_upstream_log_decodes_zstd_content_encoding() {
        let server = MockServer::start().await;
        let body = b"Sourcing python-build-hook\nRunning pythonBuildPhase\n";
        let encoded = zstd::encode_all(&body[..], 1).unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/log/{DRV}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-encoding", "zstd")
                    .set_body_bytes(encoded),
            )
            .mount(&server)
            .await;

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(28, server.uri())],
            DRV,
        )
        .await;

        assert_eq!(log.as_deref(), Some(std::str::from_utf8(body).unwrap()));
    }

    #[tokio::test]
    async fn a_redirected_log_is_followed() {
        let target = log_upstream(Some("from storage\n")).await;
        let front = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/log/{DRV}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/log/{DRV}", target.uri())),
            )
            .mount(&front)
            .await;

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(24, front.uri())],
            DRV,
        )
        .await;

        assert_eq!(log.as_deref(), Some("from storage\n"));
    }

    #[tokio::test]
    async fn a_tripped_upstream_is_not_asked() {
        let tripped = log_upstream(Some("stale\n")).await;
        let healthy = log_upstream(Some("building\n")).await;
        let id = 25;
        for _ in 0..3 {
            breakers().record(
                CacheUpstreamId::new(uuid::Uuid::from_u128(id)),
                SampleKind::Error,
            );
        }

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(id, tripped.uri()), source(26, healthy.uri())],
            DRV,
        )
        .await;

        assert_eq!(log.as_deref(), Some("building\n"));
        assert!(
            tripped
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_upstream_url_is_normalized() {
        let server = log_upstream(Some("log body")).await;

        let log = fetch_upstream_log(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &[source(27, format!("{}/", server.uri()))],
            DRV,
        )
        .await;

        assert_eq!(log.as_deref(), Some("log body"));
    }

    async fn cache_info_upstream(body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nix-cache-info"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn probe_accepts_binary_caches_and_reports_what_failed() {
        let binary = cache_info_upstream("StoreDir: /nix/store\nPriority: 40\n").await;
        let probe = probe_protocol(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &binary.uri().parse().unwrap(),
            HttpProtocolSelection::Http1Only,
            None,
        )
        .await;
        assert!(probe.ok, "{probe:?}");
        assert_eq!(probe.error, None);

        let page = cache_info_upstream("<html>login</html>").await;
        let probe = probe_protocol(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &page.uri().parse().unwrap(),
            HttpProtocolSelection::Http1Only,
            None,
        )
        .await;
        assert!(!probe.ok);
        assert_eq!(probe.status, Some(200));

        let missing = MockServer::start().await;
        let probe = probe_protocol(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &missing.uri().parse().unwrap(),
            HttpProtocolSelection::Http1Only,
            None,
        )
        .await;
        assert!(!probe.ok);
        assert_eq!(probe.status, Some(404));
        assert_eq!(probe.error.as_deref(), Some("HTTP 404 Not Found"));
    }

    #[tokio::test]
    async fn probe_names_a_refused_connection_and_an_unknown_host_briefly() {
        let (_closed, port) = bind_reserved_loopback_tcp_socket_and_port();
        let refused = probe_protocol(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &format!("http://127.0.0.1:{port}").parse().unwrap(),
            HttpProtocolSelection::Http1Only,
            None,
        )
        .await;
        assert_eq!(refused.error.as_deref(), Some("connection refused"));

        let unknown = probe_protocol(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &"http://no-such-host.invalid".parse().unwrap(),
            HttpProtocolSelection::Http1Only,
            None,
        )
        .await;
        assert_eq!(unknown.error.as_deref(), Some("DNS lookup failed"));
    }

    #[tokio::test]
    async fn cache_protocol_probe_with_empty_admin_allow_list_returns_same_error_for_open_and_closed_loopback_ports_without_sending_request()
     {
        let open = cache_info_upstream("StoreDir: /nix/store\n").await;
        let (_closed, closed_port) = bind_reserved_loopback_tcp_socket_and_port();
        let http = HttpClientForUserSuppliedUrls::new(Default::default()).unwrap();

        let mut errors = Vec::new();
        for url in [
            open.uri(),
            format!("http://127.0.0.1:{closed_port}"),
            format!("http://localhost:{}", open.address().port()),
            format!("http://localhost:{closed_port}"),
            format!("http://localhost.:{}", open.address().port()),
            format!("http://localhost.:{closed_port}"),
        ] {
            let probe = probe_protocol(
                &http,
                &url.parse().unwrap(),
                HttpProtocolSelection::Http1Only,
                None,
            )
            .await;
            assert!(!probe.ok, "{url}: {probe:?}");
            assert_eq!(probe.status, None, "{url}");
            errors.push(probe.error.unwrap_or_default());
        }

        assert_eq!(errors[0], errors[1]);
        assert_eq!(errors[2], errors[3]);
        assert_eq!(errors[4], "DNS lookup failed");
        assert_eq!(errors[5], "DNS lookup failed");
        assert!(
            !errors
                .iter()
                .any(|e| e.contains("refused") && e.contains("connection")),
            "{errors:?}"
        );
        assert!(
            open.received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_empty_admin_allow_list_blocks_upstream_cache_build_log_fetch_from_loopback_ip_and_localhost()
     {
        let server = log_upstream(Some("internal secret\n")).await;
        let http = HttpClientForUserSuppliedUrls::new(Default::default()).unwrap();

        let by_ip = fetch_upstream_log(&http, &[source(30, server.uri())], DRV).await;
        let by_name = fetch_upstream_log(
            &http,
            &[source(
                31,
                format!("http://localhost:{}", server.address().port()),
            )],
            DRV,
        )
        .await;

        assert_eq!(by_ip, None);
        assert_eq!(by_name, None);
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn upstream_cache_with_stored_credential_sends_authorization_header_and_row_without_credential_gets_401()
     {
        use gradient_entity::cache_upstream::UpstreamCredentialScheme;
        use std::io::Write as _;

        let mut crypt = tempfile::NamedTempFile::new().unwrap();
        crypt
            .write_all(b"test-secret-key-32-bytes-padding!")
            .unwrap();
        let crypt_file = crypt.path().to_string_lossy().into_owned();

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nar/x.nar"))
            .and(wiremock::matchers::header(
                "authorization",
                "Basic YnVpbGRlcjpwYTpzcw==",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("nar bytes"))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .with_priority(2)
            .mount(&server)
            .await;

        let plain = MCacheUpstream {
            url: Some(server.uri()),
            ..row(40, CacheUpstreamKind::Http, CacheSubscriptionMode::ReadOnly)
        };
        let credentialed = MCacheUpstream {
            id: CacheUpstreamId::new(uuid::Uuid::from_u128(41)),
            encrypted_credential: Some(
                gradient_sources::encrypt_secret(&crypt_file, "builder:pa:ss").unwrap(),
            ),
            credential_scheme: Some(UpstreamCredentialScheme::Basic),
            ..plain.clone()
        };
        let undecryptable = MCacheUpstream {
            id: CacheUpstreamId::new(uuid::Uuid::from_u128(42)),
            encrypted_credential: Some("bm90IGEgY2lwaGVydGV4dA==".into()),
            ..credentialed.clone()
        };

        let sources = substitution_sources(&[credentialed, undecryptable], &crypt_file).await;
        assert_eq!(sources.len(), 1, "the undecryptable row is skipped");
        assert!(!format!("{sources:?}").contains("pa:ss"));

        let http = crate::upstream::http_client_for_user_supplied_urls_allowing_loopback();
        let object = fetch_from_upstream_caches(&http, &sources, "nar/x.nar", None)
            .await
            .expect("the credentialed row is answered");
        let body = read_capped(object.body).await;
        assert_eq!(body.as_deref(), Some("nar bytes"));

        let without = substitution_sources(&[plain], &crypt_file).await;
        assert!(
            fetch_from_upstream_caches(&http, &without, "nar/x.nar", None)
                .await
                .is_none()
        );
    }

    fn create_temporary_file_with_test_encryption_key() -> tempfile::NamedTempFile {
        use std::io::Write as _;
        let mut crypt = tempfile::NamedTempFile::new().unwrap();
        crypt
            .write_all(b"test-secret-key-32-bytes-padding!")
            .unwrap();
        crypt
    }

    fn build_upstream_cache_row_with_fixed_id_and_encrypted_bearer_token(
        crypt_file: &str,
        token: &str,
    ) -> MCacheUpstream {
        build_upstream_cache_row_with_id_and_encrypted_bearer_token(50, crypt_file, token)
    }

    fn build_upstream_cache_row_with_id_and_encrypted_bearer_token(
        id: u128,
        crypt_file: &str,
        token: &str,
    ) -> MCacheUpstream {
        MCacheUpstream {
            encrypted_credential: Some(
                gradient_sources::encrypt_secret(crypt_file, token).unwrap(),
            ),
            credential_scheme: Some(UpstreamCredentialScheme::Bearer),
            ..row(id, CacheUpstreamKind::Http, CacheSubscriptionMode::ReadOnly)
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_requests_decrypt_upstream_credential_once_and_bad_credential_does_not_block_others()
     {
        let crypt = create_temporary_file_with_test_encryption_key();
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let upstream = build_upstream_cache_row_with_fixed_id_and_encrypted_bearer_token(
            &crypt_file,
            "token-one",
        );
        let undecryptable = MCacheUpstream {
            id: CacheUpstreamId::new(uuid::Uuid::from_u128(51)),
            encrypted_credential: Some("bm90IGEgY2lwaGVydGV4dA==".into()),
            ..upstream.clone()
        };
        let decrypted = DecryptedUpstreamCacheCredentialCache::new();

        let answers = futures::future::join_all(
            (0..16).map(|_| decrypted.credential_of(&crypt_file, &upstream)),
        )
        .await;
        for _ in 0..3 {
            assert!(
                decrypted
                    .credential_of(&crypt_file, &undecryptable)
                    .await
                    .is_err()
            );
        }

        assert!(answers.iter().all(|answer| matches!(
            answer,
            Ok(Some(credential)) if credential.to_payload().expose() == "token-one"
        )));
        assert_eq!(
            decrypted
                .credential_decryption_count
                .load(Ordering::Relaxed),
            2
        );
    }

    #[tokio::test]
    async fn request_uses_replaced_upstream_cache_credential() {
        let crypt = create_temporary_file_with_test_encryption_key();
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let decrypted = DecryptedUpstreamCacheCredentialCache::new();

        let before = decrypted
            .credential_of(
                &crypt_file,
                &build_upstream_cache_row_with_fixed_id_and_encrypted_bearer_token(
                    &crypt_file,
                    "old-token",
                ),
            )
            .await
            .unwrap()
            .unwrap();
        let after = decrypted
            .credential_of(
                &crypt_file,
                &build_upstream_cache_row_with_fixed_id_and_encrypted_bearer_token(
                    &crypt_file,
                    "new-token",
                ),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(before.to_payload().expose(), "old-token");
        assert_eq!(after.to_payload().expose(), "new-token");
        assert_eq!(decrypted.by_upstream.lock().unwrap().entries.len(), 1);
    }

    #[tokio::test]
    async fn full_decrypted_credential_cache_drops_credential_used_longest_ago() {
        let crypt = create_temporary_file_with_test_encryption_key();
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let decrypted = DecryptedUpstreamCacheCredentialCache::with_limits(
            2,
            FAILED_UPSTREAM_CACHE_CREDENTIAL_DECRYPTION_RETRY_DELAY,
        );
        let [in_use, used_once, new] = [60, 61, 62].map(|id| {
            build_upstream_cache_row_with_id_and_encrypted_bearer_token(id, &crypt_file, "token")
        });

        for upstream in [&in_use, &used_once, &in_use, &new] {
            decrypted
                .credential_of(&crypt_file, upstream)
                .await
                .unwrap();
        }

        let kept = decrypted.by_upstream.lock().unwrap();
        assert_eq!(kept.entries.len(), 2);
        assert!(kept.entries.contains_key(&in_use.id));
        assert!(kept.entries.contains_key(&new.id));
        drop(kept);
        assert_eq!(
            decrypted
                .credential_decryption_count
                .load(Ordering::Relaxed),
            3
        );
    }

    #[tokio::test]
    async fn credential_decrypt_retries_on_next_request_after_missing_key_file() {
        let crypt = create_temporary_file_with_test_encryption_key();
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let upstream = build_upstream_cache_row_with_fixed_id_and_encrypted_bearer_token(
            &crypt_file,
            "token-after-failure",
        );
        let decrypted = DecryptedUpstreamCacheCredentialCache::with_limits(
            MAX_UPSTREAM_CACHE_CREDENTIALS_IN_MEMORY,
            Duration::ZERO,
        );
        let moved_away = format!("{crypt_file}.moved");

        std::fs::rename(&crypt_file, &moved_away).unwrap();
        let while_unreadable = decrypted.credential_of(&crypt_file, &upstream).await;
        std::fs::rename(&moved_away, &crypt_file).unwrap();
        let once_readable = decrypted.credential_of(&crypt_file, &upstream).await;

        assert!(while_unreadable.is_err());
        assert!(matches!(
            once_readable,
            Ok(Some(credential)) if credential.to_payload().expose() == "token-after-failure"
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_requests_decrypt_each_upstream_credential_once_and_respect_concurrency_limit()
     {
        let crypt = create_temporary_file_with_test_encryption_key();
        let crypt_file = crypt.path().to_string_lossy().into_owned();
        let upstreams: Vec<_> = (70..82)
            .map(|id| {
                build_upstream_cache_row_with_id_and_encrypted_bearer_token(
                    id,
                    &crypt_file,
                    "token",
                )
            })
            .collect();
        let decrypted = DecryptedUpstreamCacheCredentialCache::new();

        let answers = futures::future::join_all(
            upstreams
                .iter()
                .cycle()
                .take(48)
                .map(|upstream| decrypted.credential_of(&crypt_file, upstream)),
        )
        .await;

        assert!(answers.iter().all(|answer| matches!(answer, Ok(Some(_)))));
        assert_eq!(
            decrypted
                .credential_decryption_count
                .load(Ordering::Relaxed),
            12
        );
        assert!(
            decrypted
                .decryption_counters
                .highest_concurrent_credential_decryption_count
                .load(Ordering::SeqCst)
                <= MAX_CONCURRENT_UPSTREAM_CACHE_CREDENTIAL_DECRYPTIONS
        );
    }
}
