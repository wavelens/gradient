/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use gradient_entity::cache_upstream::{CacheUpstreamKind, Model as MCacheUpstream};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::CacheUpstreamId;
use gradient_util::http::{HttpVersion, build_version_download_client};

use crate::upstream::{SampleKind, breakers, http1_pins};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSource {
    pub id: CacheUpstreamId,
    pub url: String,
    pub http1_only: bool,
}

pub fn substitutes_from(upstream: &MCacheUpstream) -> bool {
    upstream.kind == CacheUpstreamKind::Http
        && upstream.mode != CacheSubscriptionMode::WriteOnly
        && upstream.url.is_some()
}

pub fn substitution_sources(upstream_caches: &[MCacheUpstream]) -> Vec<UpstreamSource> {
    upstream_caches
        .iter()
        .filter(|u| substitutes_from(u))
        .filter_map(|u| {
            Some(UpstreamSource {
                id: u.id,
                url: u.url.clone()?,
                http1_only: u.http1_only,
            })
        })
        .collect()
}

pub struct UpstreamObject {
    pub content_length: Option<u64>,
    pub body: BoxStream<'static, reqwest::Result<Bytes>>,
}

/// Redirects are followed because Attic, Cachix and S3 gateways are answering object GETs with a
/// 3xx.
pub async fn fetch_from_upstream_caches(
    sources: &[UpstreamSource],
    path: &str,
    timeout: Option<Duration>,
) -> Option<UpstreamObject> {
    for source in sources {
        if !breakers().allows(source.id) {
            continue;
        }
        let url = format!("{}/{}", source.url.trim_end_matches('/'), path);
        let response = gradient_util::http1_fallback::get(
            &url,
            http1_pins().wants_http1(source.id, source.http1_only),
            http1_pins().on_failure(source.id),
            |request| match timeout {
                Some(t) => request.timeout(t),
                None => request,
            },
        )
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
                body: gradient_util::http1_fallback::resumable_body(
                    response,
                    http1_pins().on_failure(source.id),
                ),
            });
        }
    }
    None
}

pub async fn fetch_upstream_log(sources: &[UpstreamSource], drv: &str) -> Option<String> {
    let path = format!("log/{drv}");
    for source in sources {
        let Some(object) = fetch_from_upstream_caches(
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

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProtocolProbe {
    pub ok: bool,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

pub async fn probe_protocol(base_url: &str, version: HttpVersion) -> ProtocolProbe {
    let started = std::time::Instant::now();
    let outcome = fetch_cache_info(base_url, version).await;
    let latency_ms = started.elapsed().as_millis() as u64;
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
    base_url: &str,
    version: HttpVersion,
) -> Result<(), (Option<u16>, String)> {
    let client = build_version_download_client(version).map_err(|e| (None, error_chain(&e)))?;
    let url = format!("{}/nix-cache-info", base_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .timeout(PROTOCOL_PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| (None, error_chain(&e)))?;
    let status = response.status();
    if !status.is_success() {
        return Err((
            Some(status.as_u16()),
            format!("nix-cache-info answered {status}"),
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|e| (Some(status.as_u16()), error_chain(&e)))?;
    if !body.lines().any(|l| l.starts_with("StoreDir:")) {
        return Err((
            Some(status.as_u16()),
            "nix-cache-info has no StoreDir; not a binary cache".into(),
        ));
    }
    Ok(())
}

fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![err.to_string()];
    let mut cause = err.source();
    while let Some(e) = cause {
        let part = e.to_string();
        if !parts.iter().any(|p| p.contains(&part)) {
            parts.push(part);
        }
        cause = e.source();
    }
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::SampleKind;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            url,
            http1_only: false,
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

    #[test]
    fn only_readable_http_upstream_caches_are_substituted_from() {
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

        let ids: Vec<u128> = substitution_sources(&rows)
            .into_iter()
            .map(|s| s.id.into_inner().as_u128())
            .collect();

        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn serves_the_log_from_the_first_upstream_that_has_it() {
        let without = log_upstream(None).await;
        let with = log_upstream(Some("building\n")).await;

        let log =
            fetch_upstream_log(&[source(20, without.uri()), source(21, with.uri())], DRV).await;

        assert_eq!(log.as_deref(), Some("building\n"));
    }

    #[tokio::test]
    async fn an_empty_log_falls_through_to_the_next_upstream() {
        let empty = log_upstream(Some("")).await;
        let with = log_upstream(Some("building\n")).await;

        let log = fetch_upstream_log(&[source(22, empty.uri()), source(23, with.uri())], DRV).await;

        assert_eq!(log.as_deref(), Some("building\n"));
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

        let log = fetch_upstream_log(&[source(24, front.uri())], DRV).await;

        assert_eq!(log.as_deref(), Some("from storage\n"));
    }

    #[tokio::test]
    async fn a_tripped_upstream_is_not_asked() {
        let tripped = log_upstream(Some("stale\n")).await;
        let healthy = log_upstream(Some("building\n")).await;
        let id = 25;
        for _ in 0..3 {
            breakers().record(source(id, String::new()).id, SampleKind::Error);
        }

        let log =
            fetch_upstream_log(&[source(id, tripped.uri()), source(26, healthy.uri())], DRV).await;

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

        let log = fetch_upstream_log(&[source(27, format!("{}/", server.uri()))], DRV).await;

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
    async fn a_binary_cache_passes_the_http1_probe() {
        let server = cache_info_upstream("StoreDir: /nix/store\nPriority: 40\n").await;

        let probe = probe_protocol(&server.uri(), HttpVersion::Http1).await;

        assert!(probe.ok, "{probe:?}");
        assert_eq!(probe.error, None);
    }

    #[tokio::test]
    async fn a_page_that_is_no_binary_cache_fails_the_probe() {
        let server = cache_info_upstream("<html>login</html>").await;

        let probe = probe_protocol(&server.uri(), HttpVersion::Http1).await;

        assert!(!probe.ok);
        assert_eq!(probe.status, Some(200));
    }

    #[tokio::test]
    async fn a_missing_nix_cache_info_reports_its_status() {
        let server = MockServer::start().await;

        let probe = probe_protocol(&server.uri(), HttpVersion::Http1).await;

        assert!(!probe.ok);
        assert_eq!(probe.status, Some(404));
    }
}
