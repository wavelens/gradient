/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Object fetches (NARs, build logs) from the HTTP binary caches a cache
//! substitutes from. Selection and health are the ones the worker-side narinfo
//! probe uses, so a cache never proxies from an upstream the workers would not
//! substitute from, nor keeps hammering one the breaker took out of rotation.

use std::time::Duration;

use futures::StreamExt as _;
use gradient_entity::cache_upstream::{CacheUpstreamKind, Model as MCacheUpstream};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::CacheUpstreamId;

use crate::upstream::{SampleKind, breakers};

const LOG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const LOG_FETCH_MAX_BYTES: usize = 16 * 1024 * 1024;

/// One upstream an object is fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSource {
    pub id: CacheUpstreamId,
    pub url: String,
}

/// The same filter as `gradient_db::upstream_endpoints_for_project`: an HTTP
/// binary cache with a URL that is not write-only.
pub fn substitutes_from(upstream: &MCacheUpstream) -> bool {
    upstream.kind == CacheUpstreamKind::Http
        && upstream.mode != CacheSubscriptionMode::WriteOnly
        && upstream.url.is_some()
}

/// The upstream caches of one cache's rows that it substitutes from, in row order.
pub fn substitution_sources(upstream_caches: &[MCacheUpstream]) -> Vec<UpstreamSource> {
    upstream_caches
        .iter()
        .filter(|u| substitutes_from(u))
        .filter_map(|u| {
            Some(UpstreamSource {
                id: u.id,
                url: u.url.clone()?,
            })
        })
        .collect()
}

/// The first 2xx answer for `path` (relative to each upstream's base URL), in
/// order. Redirects are followed: Attic, Cachix and S3 gateways answer object
/// GETs with a 3xx. Tripped upstream caches are skipped and every answer feeds the
/// shared breakers.
pub async fn fetch_from_upstream_caches(
    sources: &[UpstreamSource],
    path: &str,
    timeout: Option<Duration>,
) -> Option<reqwest::Response> {
    let client = gradient_util::http::download_client();
    for source in sources {
        if !breakers().allows(source.id) {
            continue;
        }
        let url = format!("{}/{}", source.url.trim_end_matches('/'), path);
        let mut request = client.get(&url);
        if let Some(t) = timeout {
            request = request.timeout(t);
        }
        let response = request.send().await;
        let kind = match &response {
            Ok(r) if r.status().is_success() => SampleKind::Hit,
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => SampleKind::Miss,
            Ok(_) | Err(_) => SampleKind::Error,
        };
        breakers().record(source.id, kind);
        if kind == SampleKind::Hit {
            return response.ok();
        }
    }
    None
}

/// `drv`'s build log from the first upstream that has a non-empty one, capped
/// at 16 MiB. Logs carry no signature, so there is nothing to verify.
pub async fn fetch_upstream_log(sources: &[UpstreamSource], drv: &str) -> Option<String> {
    let path = format!("log/{drv}");
    for source in sources {
        let Some(response) = fetch_from_upstream_caches(
            std::slice::from_ref(source),
            &path,
            Some(LOG_FETCH_TIMEOUT),
        )
        .await
        else {
            continue;
        };
        if let Some(body) = read_capped(response).await {
            return Some(body);
        }
    }
    None
}

async fn read_capped(response: reqwest::Response) -> Option<String> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
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

    /// Attic, Cachix and S3 gateways redirect object GETs; an API client that
    /// refuses redirects reads the 3xx as an empty body and never gets the log.
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
}
