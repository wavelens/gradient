/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gradient_util::sync::Mutex;
use tokio::sync::Semaphore;

use gradient_db::caches::upstream::{UpstreamAccum, UpstreamEndpoint};
use gradient_entity::StorePathError;
use gradient_types::ids::CacheUpstreamId;
use gradient_types::{NixPathInfo, StorePath, StorePathMapper, parse_narinfo_body};
use gradient_util::http::{
    HttpClientForUserSuppliedUrls, HttpProtocolSelection, HttpUpstreamCredential,
    read_response_body_with_limit,
};
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use gradient_util::http_validation::{DestinationUrlRejectedError, HostHasNoAllowedAddressError};
use gradient_wire::types::CachedPath;

const MAX_UPSTREAM_NARINFO_RESPONSE_BYTES: usize = 1024 * 1024;

async fn read_upstream_narinfo_body(response: reqwest::Response) -> Option<String> {
    let body = read_response_body_with_limit(response, MAX_UPSTREAM_NARINFO_RESPONSE_BYTES)
        .await
        .ok()?;
    String::from_utf8(body).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Hit,
    Miss,
    Error,
}

#[derive(Debug, Clone)]
pub struct ProbeSample {
    pub upstream: CacheUpstreamId,
    pub latency_ms: f64,
    pub kind: SampleKind,
}

const TRIP_AFTER: u32 = 3;

const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone)]
struct Breaker {
    consecutive_errors: u32,
    open_until: Option<Instant>,
    last_warned_outbound_address_refusal: Option<String>,
}

/// Only transport failures are counting toward a trip. A 404 is a healthy answer for a missing path
/// and must never take a cache out of rotation. A cache accepting connections and never answering
/// is the expensive case.
#[derive(Debug, Default)]
pub struct UpstreamBreakers {
    inner: Mutex<HashMap<CacheUpstreamId, Breaker>>,
}

impl UpstreamBreakers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allows(&self, id: CacheUpstreamId) -> bool {
        self.allows_at(id, Instant::now())
    }

    pub fn record(&self, id: CacheUpstreamId, kind: SampleKind) {
        self.record_at(id, kind, Instant::now());
    }

    pub fn allows_at(&self, id: CacheUpstreamId, now: Instant) -> bool {
        let mut guard = self.inner.lock();
        match guard.get_mut(&id) {
            Some(b) => match b.open_until {
                Some(until) if now < until => false,
                Some(_) => {
                    b.open_until = None;
                    true
                }
                None => true,
            },
            None => true,
        }
    }

    pub fn record_at(&self, id: CacheUpstreamId, kind: SampleKind, now: Instant) {
        let mut guard = self.inner.lock();
        let b = guard.entry(id).or_default();
        match kind {
            SampleKind::Hit | SampleKind::Miss => {
                b.consecutive_errors = 0;
                b.open_until = None;
                b.last_warned_outbound_address_refusal = None;
            }
            SampleKind::Error => {
                b.consecutive_errors = b.consecutive_errors.saturating_add(1);
                if b.consecutive_errors >= TRIP_AFTER {
                    b.open_until = Some(now + BREAKER_COOLDOWN);
                }
            }
        }
    }

    pub fn record_refusal_reason_and_return_whether_it_is_new(
        &self,
        id: CacheUpstreamId,
        reason: &str,
    ) -> bool {
        let mut guard = self.inner.lock();
        let b = guard.entry(id).or_default();
        if b.last_warned_outbound_address_refusal.as_deref() == Some(reason) {
            return false;
        }
        b.last_warned_outbound_address_refusal = Some(reason.to_owned());
        true
    }
}

enum UpstreamCacheFetchRefusalWarningKind {
    HostHasNoAllowedAddress,
    UrlOrRedirectToForbiddenAddressOrUnfollowedRedirect,
}

fn outbound_address_refusal_kind_and_text_from_request_error(
    error: &(dyn std::error::Error + 'static),
) -> Option<(UpstreamCacheFetchRefusalWarningKind, String)> {
    let mut source = Some(error);
    while let Some(current) = source {
        if let Some(refusal) = current.downcast_ref::<HostHasNoAllowedAddressError>() {
            return Some((
                UpstreamCacheFetchRefusalWarningKind::HostHasNoAllowedAddress,
                refusal.to_string(),
            ));
        }
        if let Some(refusal) = current.downcast_ref::<DestinationUrlRejectedError>() {
            let kind = match refusal {
                DestinationUrlRejectedError::HostHasNoAllowedAddress(_) => {
                    UpstreamCacheFetchRefusalWarningKind::HostHasNoAllowedAddress
                }
                _ => UpstreamCacheFetchRefusalWarningKind::UrlOrRedirectToForbiddenAddressOrUnfollowedRedirect,
            };
            return Some((kind, refusal.to_string()));
        }
        source = current.source();
    }
    None
}

fn warn_once_per_upstream_until_refusal_reason_changes_that_outbound_address_rule_refused_fetch(
    upstream: CacheUpstreamId,
    url: &ValidatedHttpBaseUrl,
    error: &(dyn std::error::Error + 'static),
) {
    let Some((kind, reason)) = outbound_address_refusal_kind_and_text_from_request_error(error)
    else {
        return;
    };
    if !breakers().record_refusal_reason_and_return_whether_it_is_new(upstream, &reason) {
        return;
    }
    let host = reqwest::Url::parse(url.as_str())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_default();
    match kind {
        UpstreamCacheFetchRefusalWarningKind::HostHasNoAllowedAddress => tracing::warn!(
            %upstream,
            %host,
            %reason,
            "The server refused to fetch from this upstream cache. The host name does not resolve, or every address it resolves to is a private address or an address of the server itself. An admin can add a trusted internal host to setting `outbound.allowed_non_public_network_targets` (NixOS module option `outbound.allowedNonPublicNetworkTargets`); a name that does not resolve needs a DNS fix. The server logs this warning again only when the reason changes."
        ),
        UpstreamCacheFetchRefusalWarningKind::UrlOrRedirectToForbiddenAddressOrUnfollowedRedirect => tracing::warn!(
            %upstream,
            %host,
            %reason,
            "The server refused to fetch from this upstream cache. The URL or a redirect names a private address, an address of the server itself or `localhost`, or a redirect that the server does not follow; the field `reason` names which. An admin can add a trusted internal host to setting `outbound.allowed_non_public_network_targets` (NixOS module option `outbound.allowedNonPublicNetworkTargets`). The server logs this warning again only when the reason changes."
        ),
    }
}

pub fn breakers() -> &'static UpstreamBreakers {
    static BREAKERS: OnceLock<UpstreamBreakers> = OnceLock::new();
    BREAKERS.get_or_init(UpstreamBreakers::new)
}

pub const PARALLEL_THRESHOLD: usize = 4;

pub fn should_race(n: usize) -> bool {
    n <= PARALLEL_THRESHOLD
}

pub fn order_endpoints(eps: &mut [UpstreamEndpoint]) {
    eps.sort_by(|a, b| {
        let ha = a.hit_rate.unwrap_or(f64::NEG_INFINITY);
        let hb = b.hit_rate.unwrap_or(f64::NEG_INFINITY);
        hb.partial_cmp(&ha)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let la = a.avg_latency_ms.unwrap_or(f64::INFINITY);
                let lb = b.avg_latency_ms.unwrap_or(f64::INFINITY);
                la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
            })
    });
}

pub fn select_best_hit(
    results: Vec<(CacheUpstreamId, f64, Option<CachedPath>)>,
) -> Option<(CacheUpstreamId, CachedPath)> {
    results
        .into_iter()
        .filter_map(|(id, latency, cp)| cp.map(|c| (id, latency, c)))
        .min_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _, cp)| (id, cp))
}

pub fn fold_samples(samples: &[ProbeSample], into: &mut HashMap<CacheUpstreamId, UpstreamAccum>) {
    for s in samples {
        let acc = into.entry(s.upstream).or_default();
        match s.kind {
            SampleKind::Hit => acc.record_hit(s.latency_ms),
            SampleKind::Miss => acc.record_miss(s.latency_ms),
            SampleKind::Error => acc.record_error(s.latency_ms),
        }
    }
}

const PROBE_TIMEOUT_SECS: u64 = 2;
const PROBE_TIMEOUT: Duration = Duration::from_secs(PROBE_TIMEOUT_SECS);
const BATCH_WINDOW: usize = 256;
const PERMIT_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct ProbeResult {
    pub best: Option<(CacheUpstreamId, CachedPath)>,
    pub samples: Vec<ProbeSample>,
    pub unanswered: Vec<CacheUpstreamId>,
}

impl ProbeResult {
    /// A miss is final only once every upstream has answered. A timeout, an error or a tripped
    /// breaker is no answer, and the path is to be asked again rather than built.
    fn settle(
        best: Option<(CacheUpstreamId, CachedPath)>,
        samples: Vec<ProbeSample>,
        tripped: Vec<CacheUpstreamId>,
    ) -> Self {
        let unanswered = if best.is_some() {
            Vec::new()
        } else {
            tripped
                .into_iter()
                .chain(
                    samples
                        .iter()
                        .filter(|s| s.kind == SampleKind::Error)
                        .map(|s| s.upstream),
                )
                .collect()
        };
        Self {
            best,
            samples,
            unanswered,
        }
    }
}

async fn probe_one(
    http: &HttpClientForUserSuppliedUrls,
    pool: &Arc<Semaphore>,
    ep: &UpstreamEndpoint,
    mapper: &StorePathMapper,
    hash: &str,
    store_path: &str,
) -> (f64, SampleKind, Option<CachedPath>) {
    let permit_wait = Instant::now();
    let _permit = match tokio::time::timeout(PERMIT_ACQUIRE_TIMEOUT, pool.acquire()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) | Err(_) => {
            return (
                permit_wait.elapsed().as_secs_f64() * 1000.0,
                SampleKind::Error,
                None,
            );
        }
    };
    let narinfo_url = ep
        .url
        .append_api_path_to_base_url(&format!("{hash}.narinfo"));
    let started = Instant::now();
    let resp = match http.download_get_request_builder(
        &narinfo_url,
        HttpProtocolSelection::select_http_protocol_from_cache_row(ep.http1_only),
        None,
    ) {
        Ok(request) => match request.timeout(PROBE_TIMEOUT).send().await {
            Ok(response) => Some(response),
            Err(error) => {
                warn_once_per_upstream_until_refusal_reason_changes_that_outbound_address_rule_refused_fetch(ep.id, &ep.url, &error);
                None
            }
        },
        Err(error) => {
            warn_once_per_upstream_until_refusal_reason_changes_that_outbound_address_rule_refused_fetch(ep.id, &ep.url, &error);
            None
        }
    };
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

    let out = match resp {
        Some(r) if r.status().is_success() => match read_upstream_narinfo_body(r).await {
            Some(body) => match verified_narinfo(ep, hash, store_path, &body)
                .and_then(|cp| inbound_cached_path(mapper, cp).ok())
            {
                Some(cp) => (latency_ms, SampleKind::Hit, Some(cp)),
                None => (latency_ms, SampleKind::Miss, None),
            },
            None => (latency_ms, SampleKind::Error, None),
        },
        Some(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
            (latency_ms, SampleKind::Miss, None)
        }
        Some(_) | None => (latency_ms, SampleKind::Error, None),
    };
    breakers().record(ep.id, out.1);
    out
}

#[derive(Debug, Clone)]
pub struct UpstreamProbe {
    pub id: CacheUpstreamId,
    pub url: ValidatedHttpBaseUrl,
    pub public_key: String,
    pub http1_only: bool,
    pub credential: Option<std::sync::Arc<HttpUpstreamCredential>>,
}

#[derive(Debug, Clone)]
pub struct UpstreamNarinfo {
    pub upstream: CacheUpstreamId,
    pub path_info: NixPathInfo,
}

pub async fn find_first_signed_upstream_narinfo(
    http: &HttpClientForUserSuppliedUrls,
    upstream_caches: &[UpstreamProbe],
    path_hash: &str,
) -> Option<UpstreamNarinfo> {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let mut futs: FuturesUnordered<_> = upstream_caches
        .iter()
        .filter(|u| breakers().allows(u.id))
        .map(|u| async move {
            let url = u
                .url
                .append_api_path_to_base_url(&format!("{path_hash}.narinfo"));
            let resp = match http.download_get_request_builder(
                &url,
                HttpProtocolSelection::select_http_protocol_from_cache_row(u.http1_only),
                u.credential.as_deref(),
            ) {
                Ok(request) => match request.timeout(PROBE_TIMEOUT).send().await {
                    Ok(response) => Some(response),
                    Err(error) => {
                        warn_once_per_upstream_until_refusal_reason_changes_that_outbound_address_rule_refused_fetch(u.id, &u.url, &error);
                        None
                    }
                },
                Err(error) => {
                    warn_once_per_upstream_until_refusal_reason_changes_that_outbound_address_rule_refused_fetch(u.id, &u.url, &error);
                    None
                }
            };
            let kind = match &resp {
                Some(r) if r.status().is_success() => SampleKind::Hit,
                Some(r) if r.status() == reqwest::StatusCode::NOT_FOUND => SampleKind::Miss,
                Some(_) | None => SampleKind::Error,
            };
            breakers().record(u.id, kind);
            if kind != SampleKind::Hit {
                return None;
            }
            let body = read_upstream_narinfo_body(resp?).await?;
            let path_info = parse_upstream_narinfo_for_requested_path_with_valid_signature(
                u.id,
                &u.public_key,
                path_hash,
                None,
                &body,
            )?;
            Some(UpstreamNarinfo {
                upstream: u.id,
                path_info,
            })
        })
        .collect();

    while let Some(found) = futs.next().await {
        if found.is_some() {
            return found;
        }
    }
    None
}

pub fn inbound_cached_path(
    mapper: &StorePathMapper,
    cp: CachedPath,
) -> Result<CachedPath, StorePathError> {
    Ok(CachedPath {
        path: mapper.inbound_text_if_exact_store_path(&cp.path)?,
        references: cp
            .references
            .map(|refs| {
                refs.iter()
                    .map(|r| mapper.inbound_text_if_exact_store_path(r))
                    .collect()
            })
            .transpose()?,
        deriver: cp
            .deriver
            .map(|d| mapper.inbound_text_if_exact_store_path(&d))
            .transpose()?,
        ..cp
    })
}

pub async fn lookup_upstream_narinfo(
    http: &HttpClientForUserSuppliedUrls,
    endpoints: Arc<Vec<UpstreamEndpoint>>,
    pool: Arc<Semaphore>,
    mapper: StorePathMapper,
    target: StorePath,
) -> ProbeResult {
    let original = mapper.outbound(&target);
    let hash = original.hash().to_string();
    let store_path = original.full();
    let mut samples = Vec::new();

    // A tripped upstream is skipped rather than probed and recorded. Folding a sample never taken
    // would poison the hit rate that the ordering is built on.
    let tripped: Vec<CacheUpstreamId> = endpoints
        .iter()
        .map(|e| e.id)
        .filter(|id| !breakers().allows(*id))
        .collect();
    let endpoints: Arc<Vec<UpstreamEndpoint>> = if tripped.is_empty() {
        endpoints
    } else {
        Arc::new(
            endpoints
                .iter()
                .filter(|e| !tripped.contains(&e.id))
                .cloned()
                .collect(),
        )
    };

    if should_race(endpoints.len()) {
        use futures::stream::{FuturesUnordered, StreamExt as _};
        let mut futs: FuturesUnordered<_> = endpoints
            .iter()
            .map(|ep| {
                let pool = Arc::clone(&pool);
                let (mapper, hash, path) = (&mapper, &hash, &store_path);
                async move {
                    let (latency, kind, cp) = probe_one(http, &pool, ep, mapper, hash, path).await;
                    (ep.id, latency, kind, cp)
                }
            })
            .collect();

        let mut results = Vec::new();
        while let Some((id, latency, kind, cp)) = futs.next().await {
            samples.push(ProbeSample {
                upstream: id,
                latency_ms: latency,
                kind,
            });
            results.push((id, latency, cp));
        }

        return ProbeResult::settle(select_best_hit(results), samples, tripped);
    }

    for ep in endpoints.iter() {
        let (latency, kind, cp) = probe_one(http, &pool, ep, &mapper, &hash, &store_path).await;
        let is_hit = matches!(kind, SampleKind::Hit);
        samples.push(ProbeSample {
            upstream: ep.id,
            latency_ms: latency,
            kind,
        });
        if is_hit && let Some(cp) = cp {
            return ProbeResult::settle(Some((ep.id, cp)), samples, tripped);
        }
    }

    ProbeResult::settle(None, samples, tripped)
}

#[derive(Debug, Clone)]
pub struct VerifiedPath {
    pub verified_by: CacheUpstreamId,
    pub path: CachedPath,
}

#[derive(Debug, Default)]
pub struct BatchProbe {
    pub found: HashMap<String, VerifiedPath>,
    pub unanswered: HashMap<String, Vec<CacheUpstreamId>>,
    pub stats: HashMap<CacheUpstreamId, UpstreamAccum>,
}

pub async fn probe_batch(
    http: &HttpClientForUserSuppliedUrls,
    mut endpoints: Vec<UpstreamEndpoint>,
    pool: Arc<Semaphore>,
    mapper: StorePathMapper,
    targets: Vec<StorePath>,
) -> BatchProbe {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let mut batch = BatchProbe::default();
    if targets.is_empty() || endpoints.is_empty() {
        return batch;
    }

    order_endpoints(&mut endpoints);
    let endpoints = Arc::new(endpoints);

    let mut futs = FuturesUnordered::new();
    let mut iter = targets.into_iter();
    let push = |futs: &mut FuturesUnordered<_>, target: StorePath| {
        let eps = Arc::clone(&endpoints);
        let pool = Arc::clone(&pool);
        let mapper = mapper.clone();
        futs.push(async move {
            let hash = target.hash().to_owned();
            let res = lookup_upstream_narinfo(http, eps, pool, mapper, target).await;
            (hash, res)
        });
    };

    for _ in 0..BATCH_WINDOW {
        match iter.next() {
            Some(target) => push(&mut futs, target),
            None => break,
        }
    }

    while let Some((hash, res)) = futs.next().await {
        fold_samples(&res.samples, &mut batch.stats);
        if let Some((verified_by, path)) = res.best {
            batch.found.insert(hash, VerifiedPath { verified_by, path });
        } else if !res.unanswered.is_empty() {
            batch.unanswered.insert(hash, res.unanswered);
        }

        if let Some(target) = iter.next() {
            push(&mut futs, target);
        }
    }

    batch
}

pub fn verified_narinfo(
    ep: &UpstreamEndpoint,
    path_hash: &str,
    store_path: &str,
    body: &str,
) -> Option<CachedPath> {
    let Some(public_key) = ep.public_key.as_deref() else {
        tracing::warn!(upstream = %ep.id, "upstream has no public_key; ignoring its narinfo");
        return None;
    };
    let path_info = parse_upstream_narinfo_for_requested_path_with_valid_signature(
        ep.id,
        public_key,
        path_hash,
        Some(store_path),
        body,
    )?;
    Some(cached_path_from_upstream_narinfo(
        ep.url.as_str(),
        store_path,
        path_info,
    ))
}

fn parse_upstream_narinfo_for_requested_path_with_valid_signature(
    upstream: CacheUpstreamId,
    public_key: &str,
    path_hash: &str,
    requested_store_path: Option<&str>,
    body: &str,
) -> Option<NixPathInfo> {
    let path_info = match parse_narinfo_body(body) {
        Ok(path_info) => path_info,
        Err(error) => {
            tracing::warn!(
                upstream = %upstream,
                path_hash,
                %error,
                "discard invalid upstream narinfo"
            );
            return None;
        }
    };
    if !names_path_hash(&path_info, path_hash)
        || requested_store_path
            .is_some_and(|requested| !narinfo_matches_full_store_path(&path_info, requested))
        || !gradient_sources::verify_narinfo_signature(public_key, &path_info)
    {
        tracing::warn!(
            upstream = %upstream,
            path_hash,
            "upstream narinfo is not signed by its configured public_key for this path; dropping"
        );
        return None;
    }
    Some(path_info)
}

fn names_path_hash(path_info: &NixPathInfo, path_hash: &str) -> bool {
    StorePath::parse_stored(&path_info.store_path).is_ok_and(|p| p.hash() == path_hash)
}

fn narinfo_matches_full_store_path(path_info: &NixPathInfo, requested_store_path: &str) -> bool {
    match (
        StorePath::parse_stored(&path_info.store_path),
        StorePath::parse_stored(requested_store_path),
    ) {
        (Ok(named), Ok(requested)) => named == requested,
        _ => false,
    }
}

fn store_path_with_nix_store_prefix(name_or_path: &str) -> String {
    if name_or_path.starts_with("/nix/store/") {
        name_or_path.to_owned()
    } else {
        format!("/nix/store/{name_or_path}")
    }
}

pub fn cached_path_from_upstream_narinfo(
    base_url: &str,
    store_path: &str,
    path_info: NixPathInfo,
) -> CachedPath {
    CachedPath {
        path: store_path.to_string(),
        cached: true,
        file_size: path_info.file_size,
        nar_size: Some(path_info.nar_size),
        url: Some(format!("{}/{}", base_url.trim_end_matches('/'), path_info.url).into()),
        nar_hash: Some(path_info.nar_hash),
        file_hash: path_info.file_hash,
        references: Some(
            path_info
                .references
                .iter()
                .map(|r| store_path_with_nix_store_prefix(r))
                .collect(),
        ),
        signatures: Some(path_info.signature_lines),
        deriver: path_info
            .deriver
            .filter(|d| !d.is_empty())
            .map(|d| store_path_with_nix_store_prefix(&d)),
        ca: path_info.ca.filter(|c| !c.is_empty()),
    }
}

#[cfg(test)]
pub(crate) fn http_client_for_user_supplied_urls_allowing_loopback() -> HttpClientForUserSuppliedUrls
{
    let allowlist = gradient_util::http_validation::UserUrlDestinationAllowlist::parse(&[
        "127.0.0.0/8".to_owned(),
        "::1/128".to_owned(),
    ])
    .unwrap();
    HttpClientForUserSuppliedUrls::new(allowlist).unwrap()
}

#[cfg(test)]
mod tests {
    use super::{BREAKER_COOLDOWN, SampleKind, TRIP_AFTER, UpstreamBreakers, breakers};
    use gradient_types::ids::CacheUpstreamId;
    use std::time::{Duration, Instant};

    fn upstream(n: u128) -> CacheUpstreamId {
        CacheUpstreamId::new(uuid::Uuid::from_u128(n))
    }

    #[test]
    fn a_black_holed_upstream_is_taken_out_of_rotation() {
        let b = UpstreamBreakers::new();
        let id = upstream(1);
        let t0 = Instant::now();

        for _ in 0..TRIP_AFTER {
            assert!(b.allows_at(id, t0), "must keep trying up to the threshold");
            b.record_at(id, SampleKind::Error, t0);
        }

        assert!(!b.allows_at(id, t0), "the upstream should be tripped");
    }

    #[test]
    fn a_miss_is_not_a_failure() {
        let b = UpstreamBreakers::new();
        let id = upstream(2);
        let t0 = Instant::now();

        for _ in 0..(TRIP_AFTER * 3) {
            b.record_at(id, SampleKind::Miss, t0);
        }

        assert!(b.allows_at(id, t0));
    }

    #[test]
    fn a_success_resets_the_failure_count() {
        let b = UpstreamBreakers::new();
        let id = upstream(3);
        let t0 = Instant::now();

        b.record_at(id, SampleKind::Error, t0);
        b.record_at(id, SampleKind::Error, t0);
        b.record_at(id, SampleKind::Hit, t0);
        b.record_at(id, SampleKind::Error, t0);

        assert!(b.allows_at(id, t0), "two strikes short of the threshold");
    }

    #[test]
    fn a_tripped_upstream_is_retried_after_the_cooldown() {
        let b = UpstreamBreakers::new();
        let id = upstream(4);
        let t0 = Instant::now();

        for _ in 0..TRIP_AFTER {
            b.record_at(id, SampleKind::Error, t0);
        }
        assert!(!b.allows_at(id, t0));

        let later = t0 + BREAKER_COOLDOWN + Duration::from_secs(1);
        assert!(
            b.allows_at(id, later),
            "cooldown must let one probe through"
        );
    }

    #[test]
    fn a_still_broken_upstream_trips_again_on_the_next_failure() {
        let b = UpstreamBreakers::new();
        let id = upstream(5);
        let t0 = Instant::now();

        for _ in 0..TRIP_AFTER {
            b.record_at(id, SampleKind::Error, t0);
        }
        let later = t0 + BREAKER_COOLDOWN + Duration::from_secs(1);
        assert!(b.allows_at(id, later));

        b.record_at(id, SampleKind::Error, later);
        assert!(
            !b.allows_at(id, later),
            "one failure re-trips a half-open upstream"
        );
    }

    async fn black_hole() -> (super::ValidatedHttpBaseUrl, tokio::net::TcpListener) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        (format!("http://{addr}").parse().unwrap(), listener)
    }

    #[tokio::test]
    async fn a_black_hole_is_bounded_by_the_probe_timeout() {
        let (url, _listener) = black_hole().await;
        let probes = vec![super::UpstreamProbe {
            id: upstream(10),
            url,
            public_key: "test:0000000000000000000000000000000000000000000=".into(),
            credential: None,
            http1_only: false,
        }];

        let started = Instant::now();
        let got = super::find_first_signed_upstream_narinfo(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &probes,
            "brj5bb4pny8pnngq3qdymkllwql6z29j",
        )
        .await;
        let elapsed = started.elapsed();

        assert!(got.is_none());
        assert!(
            elapsed < Duration::from_secs(super::PROBE_TIMEOUT_SECS + 3),
            "a dead upstream must not hold the request: took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_tripped_upstream_is_not_probed_at_all() {
        let (url, _listener) = black_hole().await;
        let id = upstream(11);
        for _ in 0..TRIP_AFTER {
            breakers().record(id, SampleKind::Error);
        }
        let probes = vec![super::UpstreamProbe {
            id,
            url,
            public_key: "test:0000000000000000000000000000000000000000000=".into(),
            credential: None,
            http1_only: false,
        }];

        let started = Instant::now();
        let got = super::find_first_signed_upstream_narinfo(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &probes,
            "brj5bb4pny8pnngq3qdymkllwql6z29j",
        )
        .await;
        let elapsed = started.elapsed();

        assert!(got.is_none());
        assert!(
            elapsed < Duration::from_millis(500),
            "a tripped upstream must be skipped, not probed: took {elapsed:?}"
        );
    }

    use super::*;

    async fn upstream_answering(status: u16) -> (wiremock::MockServer, UpstreamEndpoint) {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let endpoint = UpstreamEndpoint {
            url: server.uri().parse().unwrap(),
            ..ep(None, None)
        };
        (server, endpoint)
    }

    async fn lookup(endpoints: Vec<UpstreamEndpoint>) -> ProbeResult {
        lookup_upstream_narinfo(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            Arc::new(endpoints),
            Arc::new(Semaphore::new(4)),
            gradient_types::StorePathMapper::for_cache(
                Arc::new(gradient_types::IdentityMapping),
                gradient_types::ids::CacheId::nil(),
            ),
            StorePath::parse_stored(SIGNED_PATH).expect("path"),
        )
        .await
    }

    #[tokio::test]
    async fn an_upstream_failing_to_answer_leaves_the_path_unanswered() {
        let (_missing, missing) = upstream_answering(404).await;
        let (_failing, failing) = upstream_answering(503).await;
        let failing_id = failing.id;

        let result = lookup(vec![missing, failing]).await;

        assert!(result.best.is_none());
        assert_eq!(result.unanswered, vec![failing_id]);
    }

    #[tokio::test]
    async fn a_miss_on_every_upstream_is_an_answer() {
        let (_a, a) = upstream_answering(404).await;
        let (_b, b) = upstream_answering(404).await;

        let result = lookup(vec![a, b]).await;

        assert!(result.best.is_none());
        assert!(result.unanswered.is_empty(), "{:?}", result.unanswered);
    }

    #[tokio::test]
    async fn a_tripped_upstream_leaves_the_path_unanswered() {
        let (url, _listener) = black_hole().await;
        let tripped = UpstreamEndpoint {
            url,
            ..ep(None, None)
        };
        for _ in 0..TRIP_AFTER {
            breakers().record(tripped.id, SampleKind::Error);
        }
        let tripped_id = tripped.id;

        let result = lookup(vec![tripped]).await;

        assert!(result.best.is_none());
        assert_eq!(result.unanswered, vec![tripped_id]);
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn warning_lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .filter(|line| line.contains(" WARN "))
                .map(str::to_owned)
                .collect()
        }
    }

    #[tokio::test]
    async fn two_refused_upstream_cache_lookups_log_one_warning_until_refusal_reason_changes() {
        let logs = CapturedLogs::default();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_ansi(false)
                .finish(),
        );
        let refused = UpstreamEndpoint {
            url: "http://10.0.0.1/private-path/".parse().unwrap(),
            ..ep(None, None)
        };

        lookup(vec![refused.clone()]).await;
        lookup(vec![refused.clone()]).await;

        let warnings = logs.warning_lines();
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0].contains("host=10.0.0.1")
                && warnings[0].contains("The URL or a redirect")
                && warnings[0].contains("outbound.allowed_non_public_network_targets")
                && warnings[0].contains("outbound.allowedNonPublicNetworkTargets")
                && !warnings[0].contains("private-path"),
            "{}",
            warnings[0]
        );

        lookup(vec![UpstreamEndpoint {
            url: "http://10.0.0.2/".parse().unwrap(),
            ..refused
        }])
        .await;

        assert_eq!(logs.warning_lines().len(), 2, "{:#?}", logs.warning_lines());
    }

    #[tokio::test]
    async fn upstream_cache_with_unresolvable_host_name_logs_one_warning_that_host_name_does_not_resolve()
     {
        let logs = CapturedLogs::default();
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_ansi(false)
                .finish(),
        );

        lookup(vec![UpstreamEndpoint {
            url: "http://upstream-name-that-does-not-resolve.invalid/"
                .parse()
                .unwrap(),
            ..ep(None, None)
        }])
        .await;

        let warnings = logs.warning_lines();
        assert_eq!(warnings.len(), 1, "{warnings:#?}");
        assert!(
            warnings[0].contains("The host name does not resolve"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn upstream_narinfo_with_all_fields_converts_to_cached_path_keeping_every_field() {
        let body = "StorePath: /nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1\n\
                    URL: nar/abc.nar.xz\n\
                    Compression: xz\n\
                    FileHash: sha256:124l7vc762nsgl8wmfgp1gm9vsl3gk0j6136nyv3ff7s7da11yvz\n\
                    FileSize: 35372\n\
                    NarHash: sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf\n\
                    NarSize: 102760\n\
                    References: REFERENCES\n\
                    Deriver: vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv\n\
                    Sig: cache.nixos.org-1:a84Gyv6ieXj7HclpmXu/i+so=\n"
            .replace(
                "REFERENCES",
                &format!("{SIGNED_HASH}-dep1 /nix/store/{DIFFERENT_STORE_PATH_HASH}-dep2"),
            );
        let cp = cached_path_from_upstream_narinfo(
            "https://upstream.example/",
            "/nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1",
            parse_narinfo_body(&body).expect("a valid narinfo"),
        );
        assert!(cp.cached);
        assert_eq!(
            cp.url.as_ref().map(
                gradient_wire::types::RedactedPresignedS3Url::as_presigned_url_with_secret_query
            ),
            Some("https://upstream.example/nar/abc.nar.xz")
        );
        assert_eq!(cp.nar_size, Some(102760));
        assert_eq!(cp.file_size, Some(35372));
        assert_eq!(
            cp.nar_hash.as_deref(),
            Some("sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf")
        );
        assert_eq!(
            cp.file_hash.as_deref(),
            Some("sha256:124l7vc762nsgl8wmfgp1gm9vsl3gk0j6136nyv3ff7s7da11yvz")
        );
        let refs = cp.references.unwrap();
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&format!("/nix/store/{SIGNED_HASH}-dep1")));
        assert!(refs.contains(&format!("/nix/store/{DIFFERENT_STORE_PATH_HASH}-dep2")));
        assert_eq!(
            cp.deriver.as_deref(),
            Some("/nix/store/vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv")
        );
        assert_eq!(cp.signatures.as_ref().unwrap().len(), 1);
    }

    fn narinfo_with_required_fields_and_one_signature() -> NixPathInfo {
        parse_narinfo_body(
            "StorePath: /nix/store/aa-x\nURL: nar/x.nar.xz\nCompression: xz\n\
             FileHash: sha256:deadbeef\nFileSize: 1\nNarHash: sha256:deadbeef\nNarSize: 1\n\
             Sig: up-1:AAAA\n",
        )
        .expect("a valid narinfo")
    }

    #[test]
    fn upstream_narinfo_to_cached_path_keeps_content_address() {
        let path_info = NixPathInfo {
            ca: Some("fixed:sha256:0abc".to_owned()),
            ..narinfo_with_required_fields_and_one_signature()
        };
        let cp = cached_path_from_upstream_narinfo("https://up/", "/nix/store/aa-x", path_info);
        assert_eq!(cp.ca.as_deref(), Some("fixed:sha256:0abc"));
    }

    #[test]
    fn test_upstream_narinfo_without_references_converts_to_cached_path_with_empty_references() {
        let cp = cached_path_from_upstream_narinfo(
            "https://up/",
            "/nix/store/aa-x",
            narinfo_with_required_fields_and_one_signature(),
        );
        assert_eq!(cp.references.as_deref(), Some(&[][..]));
    }

    #[test]
    fn upstream_narinfo_with_empty_deriver_converts_to_cached_path_without_deriver() {
        let path_info = NixPathInfo {
            deriver: Some(String::new()),
            ..narinfo_with_required_fields_and_one_signature()
        };
        let cp = cached_path_from_upstream_narinfo("https://up/", "/nix/store/aa-x", path_info);
        assert!(cp.deriver.is_none());
    }

    #[test]
    fn upstream_narinfo_to_cached_path_removes_trailing_slash_from_base_url_before_joining_nar_url()
    {
        let cp = cached_path_from_upstream_narinfo(
            "https://up.example/",
            "/nix/store/aa-x",
            narinfo_with_required_fields_and_one_signature(),
        );
        assert_eq!(
            cp.url.as_ref().map(
                gradient_wire::types::RedactedPresignedS3Url::as_presigned_url_with_secret_query
            ),
            Some("https://up.example/nar/x.nar.xz")
        );
    }

    const SIGNED_HASH: &str = "brj5bb4pny8pnngq3qdymkllwql6z29j";
    const SIGNED_PATH: &str = "/nix/store/brj5bb4pny8pnngq3qdymkllwql6z29j-hello-2.12";

    fn signed_narinfo(store_path: &str) -> (String, String) {
        signed_narinfo_referencing(store_path, None)
    }

    fn signed_narinfo_referencing(store_path: &str, reference: Option<&str>) -> (String, String) {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let keypair = ed25519_compact::KeyPair::generate();
        let nar_hash = "sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";
        let full_reference = reference.map_or(String::new(), |r| format!("/nix/store/{r}"));
        let fingerprint = format!("1;{store_path};{nar_hash};120;{full_reference}");
        let sig = STANDARD.encode(*keypair.sk.sign(fingerprint, None));
        let body = format!(
            "StorePath: {store_path}\nURL: nar/x.nar.xz\nCompression: xz\nFileHash: {nar_hash}\n\
             FileSize: 100\nNarHash: {nar_hash}\nNarSize: 120\nReferences: {}\n\
             Sig: upstream-1:{sig}\n",
            reference.unwrap_or_default()
        );
        (body, format!("upstream-1:{}", STANDARD.encode(*keypair.pk)))
    }

    #[tokio::test]
    async fn an_upstream_is_asked_for_the_original_hash_and_answers_in_stored_form() {
        use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
        let (original, stored) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let (body, key) = signed_narinfo_referencing(
            &format!("/nix/store/{original}-hello"),
            Some(&format!("{original}-dep")),
        );
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(format!("/{original}.narinfo")))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let endpoint = UpstreamEndpoint {
            url: server.uri().parse().unwrap(),
            public_key: Some(key),
            ..ep(None, None)
        };
        let mapper = gradient_types::StorePathMapper::for_cache(
            Arc::new(RotatedHashMapping),
            gradient_types::ids::CacheId::nil(),
        );

        let batch = probe_batch(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            vec![endpoint],
            Arc::new(Semaphore::new(4)),
            mapper,
            vec![gradient_types::StorePath::from_hash_and_name_without_validation(stored, "hello")],
        )
        .await;

        let found = &batch.found[stored].path;
        assert_eq!(found.path, format!("/nix/store/{stored}-hello"));
        assert_eq!(
            found.references,
            Some(vec![format!("/nix/store/{stored}-dep")])
        );
    }

    #[test]
    fn inbound_cached_path_rejects_store_path_with_subpath_or_parent_traversal() {
        let original = gradient_entity::EXAMPLE_ORIGINAL_HASH;
        let mapper = gradient_types::StorePathMapper::for_cache(
            Arc::new(gradient_entity::RotatedHashMapping),
            gradient_types::ids::CacheId::nil(),
        );
        for path in [
            format!("/nix/store/{original}-hello/sub"),
            format!("/nix/store/{original}-hello/../x"),
        ] {
            let cached = CachedPath {
                path: path.clone(),
                ..Default::default()
            };
            assert!(inbound_cached_path(&mapper, cached).is_err(), "{path}");
        }
    }

    fn ep_with_key(public_key: Option<String>) -> UpstreamEndpoint {
        UpstreamEndpoint {
            public_key,
            ..ep(None, None)
        }
    }

    #[test]
    fn a_narinfo_signed_by_the_upstream_key_is_a_hit() {
        let (body, key) = signed_narinfo(SIGNED_PATH);
        let cp = verified_narinfo(&ep_with_key(Some(key)), SIGNED_HASH, SIGNED_PATH, &body)
            .expect("verified");
        assert_eq!(
            cp.url.as_ref().map(
                gradient_wire::types::RedactedPresignedS3Url::as_presigned_url_with_secret_query
            ),
            Some("https://up.example/nar/x.nar.xz")
        );
    }

    #[test]
    fn an_unsigned_narinfo_is_a_miss() {
        let (body, key) = signed_narinfo(SIGNED_PATH);
        let unsigned: String = body
            .lines()
            .filter(|l| !l.starts_with("Sig: "))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(
            verified_narinfo(&ep_with_key(Some(key)), SIGNED_HASH, SIGNED_PATH, &unsigned)
                .is_none()
        );
    }

    #[test]
    fn a_narinfo_signed_by_another_key_is_a_miss() {
        let (body, _) = signed_narinfo(SIGNED_PATH);
        let (_, other_key) = signed_narinfo(SIGNED_PATH);
        assert!(
            verified_narinfo(
                &ep_with_key(Some(other_key)),
                SIGNED_HASH,
                SIGNED_PATH,
                &body
            )
            .is_none()
        );
    }

    #[test]
    fn a_signed_narinfo_for_another_path_is_a_miss() {
        let other = "/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-evil-1.0";
        let (body, key) = signed_narinfo(other);
        assert!(
            verified_narinfo(&ep_with_key(Some(key)), SIGNED_HASH, SIGNED_PATH, &body).is_none()
        );
    }

    #[test]
    fn rejects_narinfo_signed_for_same_hash_but_different_store_name() {
        let (body, key) = signed_narinfo(&format!("/nix/store/{SIGNED_HASH}-other"));
        let requested = format!("/nix/store/{SIGNED_HASH}-name");
        assert!(
            verified_narinfo(&ep_with_key(Some(key)), SIGNED_HASH, &requested, &body).is_none()
        );
    }

    #[test]
    fn an_upstream_without_a_key_serves_nothing() {
        let (body, _) = signed_narinfo(SIGNED_PATH);
        assert!(verified_narinfo(&ep_with_key(None), SIGNED_HASH, SIGNED_PATH, &body).is_none());
    }

    const DIFFERENT_STORE_PATH_HASH: &str = "0c8xfp8wr4dhz8gm2qv5nk5r6dl7hzsk";

    fn unsigned_evil_store_path() -> String {
        format!("/nix/store/{DIFFERENT_STORE_PATH_HASH}-evil-1.0")
    }

    fn narinfo_texts_with_two_store_paths_and_upstream_public_key() -> (Vec<String>, String) {
        let (body, key) = signed_narinfo(SIGNED_PATH);
        let signed_line = format!("StorePath: {SIGNED_PATH}\n");
        let other_line = format!("StorePath: {}\n", unsigned_evil_store_path());
        let other_first = format!("{other_line}{body}");
        let other_second = body.replacen(&signed_line, &format!("{signed_line}{other_line}"), 1);
        let other_without_space = format!("StorePath:{}\n{body}", unsigned_evil_store_path());
        (vec![other_first, other_second, other_without_space], key)
    }

    #[test]
    fn rejects_signed_upstream_narinfo_with_two_store_path_lines_for_both_paths() {
        let (bodies, key) = narinfo_texts_with_two_store_paths_and_upstream_public_key();
        let endpoint = ep_with_key(Some(key));
        let mut accepted = Vec::new();
        for (body_index, body) in bodies.iter().enumerate() {
            for (hash, path) in [
                (SIGNED_HASH, SIGNED_PATH.to_owned()),
                (DIFFERENT_STORE_PATH_HASH, unsigned_evil_store_path()),
            ] {
                if verified_narinfo(&endpoint, hash, &path, body).is_some() {
                    accepted.push((body_index, path));
                }
            }
        }
        assert_eq!(accepted, Vec::new());
    }

    #[tokio::test]
    async fn upstream_narinfo_fetch_with_two_store_path_lines_returns_none_for_each_path_hash() {
        let (bodies, key) = narinfo_texts_with_two_store_paths_and_upstream_public_key();
        for body in bodies {
            let server = wiremock::MockServer::start().await;
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body.clone()))
                .mount(&server)
                .await;
            let probes = vec![UpstreamProbe {
                id: CacheUpstreamId::now_v7(),
                url: server.uri().parse().unwrap(),
                public_key: key.clone(),
                credential: None,
                http1_only: false,
            }];
            for hash in [SIGNED_HASH, DIFFERENT_STORE_PATH_HASH] {
                let found = find_first_signed_upstream_narinfo(
                    &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
                    &probes,
                    hash,
                )
                .await;
                assert!(found.is_none(), "served for {hash}:\n{body}");
            }
        }
    }

    async fn start_mock_upstream_serving_signed_narinfo_and_fetch_narinfo(
        path_hash: &str,
    ) -> (CacheUpstreamId, Option<UpstreamNarinfo>) {
        let (body, key) = signed_narinfo(SIGNED_PATH);
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let id = CacheUpstreamId::now_v7();
        let probes = vec![UpstreamProbe {
            id,
            url: server.uri().parse().unwrap(),
            public_key: key,
            credential: None,
            http1_only: false,
        }];
        let found = find_first_signed_upstream_narinfo(
            &crate::upstream::http_client_for_user_supplied_urls_allowing_loopback(),
            &probes,
            path_hash,
        )
        .await;
        (id, found)
    }

    #[tokio::test]
    async fn fetch_returns_none_when_upstream_narinfo_is_signed_for_another_store_path() {
        let (_, found) =
            start_mock_upstream_serving_signed_narinfo_and_fetch_narinfo(DIFFERENT_STORE_PATH_HASH)
                .await;
        assert!(found.is_none());
    }

    #[tokio::test]
    async fn test_fetch_returns_verified_narinfo_with_upstream_id_when_upstream_cache_serves_correctly_signed_narinfo_for_requested_hash()
     {
        let (id, found) =
            start_mock_upstream_serving_signed_narinfo_and_fetch_narinfo(SIGNED_HASH).await;
        let found = found.expect("verified");
        assert_eq!(found.upstream, id);
        assert_eq!(found.path_info.store_path, SIGNED_PATH);
        assert_eq!(found.path_info.nar_size, 120);
    }

    fn ep(latency: Option<f64>, hit: Option<f64>) -> UpstreamEndpoint {
        UpstreamEndpoint {
            id: CacheUpstreamId::now_v7(),
            url: "https://up.example/".parse().unwrap(),
            public_key: None,
            http1_only: false,
            avg_latency_ms: latency,
            hit_rate: hit,
        }
    }

    #[test]
    fn order_endpoints_hit_rate_desc_then_latency_asc() {
        let mut v = vec![
            ep(Some(10.0), Some(0.5)),
            ep(Some(50.0), Some(0.9)),
            ep(Some(5.0), Some(0.9)),
        ];
        order_endpoints(&mut v);
        assert_eq!(v[0].hit_rate, Some(0.9));
        assert_eq!(v[0].avg_latency_ms, Some(5.0));
        assert_eq!(v[1].hit_rate, Some(0.9));
        assert_eq!(v[1].avg_latency_ms, Some(50.0));
        assert_eq!(v[2].hit_rate, Some(0.5));
    }

    #[test]
    fn order_endpoints_unknown_hit_rate_sorts_last() {
        let mut v = vec![ep(Some(1.0), None), ep(Some(99.0), Some(0.1))];
        order_endpoints(&mut v);
        assert_eq!(v[0].hit_rate, Some(0.1));
        assert_eq!(v[1].hit_rate, None);
    }

    #[test]
    fn should_race_only_for_small_n() {
        assert!(should_race(1));
        assert!(should_race(4));
        assert!(!should_race(5));
    }

    #[test]
    fn select_best_hit_picks_lowest_latency_hit() {
        let a = CacheUpstreamId::now_v7();
        let b = CacheUpstreamId::now_v7();
        let cp = |p: &str| CachedPath {
            path: p.into(),
            cached: true,
            file_size: None,
            nar_size: None,
            url: Some("https://x/nar".into()),
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        let results = vec![
            (a, 40.0, Some(cp("/nix/store/aa"))),
            (b, 9.0, Some(cp("/nix/store/bb"))),
        ];
        let (winner, _) = select_best_hit(results).expect("a hit");
        assert_eq!(winner, b);
    }

    #[test]
    fn select_best_hit_none_when_all_miss() {
        let a = CacheUpstreamId::now_v7();
        assert!(select_best_hit(vec![(a, 10.0, None)]).is_none());
    }

    #[test]
    fn fold_samples_aggregates_per_upstream() {
        let a = CacheUpstreamId::now_v7();
        let samples = vec![
            ProbeSample {
                upstream: a,
                latency_ms: 10.0,
                kind: SampleKind::Hit,
            },
            ProbeSample {
                upstream: a,
                latency_ms: 20.0,
                kind: SampleKind::Miss,
            },
            ProbeSample {
                upstream: a,
                latency_ms: 5000.0,
                kind: SampleKind::Error,
            },
        ];
        let mut map = HashMap::new();
        fold_samples(&samples, &mut map);
        let acc = &map[&a];
        assert_eq!(acc.request_count, 3);
        assert_eq!(acc.narinfo_hits, 1);
        assert_eq!(acc.narinfo_misses, 1);
        assert_eq!(acc.latency_ms_sum, 5030.0);
    }
}
