/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gradient_util::shutdown::CancellationToken;
use gradient_util::sync::Mutex;
use tokio::sync::Semaphore;

use gradient_db::caches::upstream::{UpstreamAccum, UpstreamEndpoint};
use gradient_types::ids::CacheUpstreamId;
use gradient_wire::types::CachedPath;

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

#[derive(Debug, Default, Clone, Copy)]
struct Breaker {
    consecutive_errors: u32,
    open_until: Option<Instant>,
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
            }
            SampleKind::Error => {
                b.consecutive_errors = b.consecutive_errors.saturating_add(1);
                if b.consecutive_errors >= TRIP_AFTER {
                    b.open_until = Some(now + BREAKER_COOLDOWN);
                }
            }
        }
    }
}

pub fn breakers() -> &'static UpstreamBreakers {
    static BREAKERS: OnceLock<UpstreamBreakers> = OnceLock::new();
    BREAKERS.get_or_init(UpstreamBreakers::new)
}

#[derive(Debug, Default)]
pub struct Http1Pins {
    pinned: Mutex<HashSet<CacheUpstreamId>>,
    writer: OnceLock<tokio::sync::mpsc::UnboundedSender<CacheUpstreamId>>,
}

impl Http1Pins {
    pub fn wants_http1(&self, id: CacheUpstreamId, stored: bool) -> bool {
        stored || self.pinned.lock().contains(&id)
    }

    pub fn pin(&self, id: CacheUpstreamId) {
        if !self.pinned.lock().insert(id) {
            return;
        }
        tracing::warn!(upstream = %id, "upstream reset an HTTP/2 stream; asking it over HTTP/1.1");
        if let Some(writer) = self.writer.get() {
            let _ = writer.send(id);
        }
    }

    pub fn on_failure(&'static self, id: CacheUpstreamId) -> impl FnOnce() + Send + 'static {
        move || self.pin(id)
    }
}

pub fn http1_pins() -> &'static Http1Pins {
    static PINS: OnceLock<Http1Pins> = OnceLock::new();
    PINS.get_or_init(Http1Pins::default)
}

pub async fn persist_http1_pins(db: gradient_db::WebDb, shutdown: CancellationToken) {
    let (writer, mut pins) = tokio::sync::mpsc::unbounded_channel();
    if http1_pins().writer.set(writer).is_err() {
        return;
    }
    while let Some(Some(id)) = shutdown.run_until_cancelled(pins.recv()).await {
        if let Err(e) = gradient_db::caches::upstream::pin_upstream_to_http1(db.inner(), id).await {
            tracing::warn!(upstream = %id, error = %e, "failed to persist the HTTP/1.1 pin");
        }
    }
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
    pool: &Arc<Semaphore>,
    ep: &UpstreamEndpoint,
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
    let narinfo_url = format!("{}/{}.narinfo", ep.url.trim_end_matches('/'), hash);
    let started = Instant::now();
    let resp = gradient_util::http1_fallback::get(
        &narinfo_url,
        http1_pins().wants_http1(ep.id, ep.http1_only),
        http1_pins().on_failure(ep.id),
        |r| r.timeout(PROBE_TIMEOUT),
    )
    .await;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

    let out = match resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(body) => match verified_narinfo(ep, hash, store_path, &body) {
                Some(cp) => (latency_ms, SampleKind::Hit, Some(cp)),
                None => (latency_ms, SampleKind::Miss, None),
            },
            Err(_) => (latency_ms, SampleKind::Error, None),
        },
        Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => {
            (latency_ms, SampleKind::Miss, None)
        }
        Ok(_) => (latency_ms, SampleKind::Error, None),
        Err(_) => (latency_ms, SampleKind::Error, None),
    };
    breakers().record(ep.id, out.1);
    out
}

#[derive(Debug, Clone)]
pub struct UpstreamProbe {
    pub id: CacheUpstreamId,
    pub url: String,
    pub public_key: String,
    pub http1_only: bool,
}

#[derive(Debug, Clone)]
pub struct UpstreamNarinfo {
    pub upstream: CacheUpstreamId,
    pub body: String,
}

pub async fn fetch_narinfo_body(
    upstream_caches: &[UpstreamProbe],
    path_hash: &str,
) -> Option<UpstreamNarinfo> {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let mut futs: FuturesUnordered<_> = upstream_caches
        .iter()
        .filter(|u| breakers().allows(u.id))
        .map(|u| async move {
            let url = format!("{}/{}.narinfo", u.url.trim_end_matches('/'), path_hash);
            let resp = gradient_util::http1_fallback::get(
                &url,
                http1_pins().wants_http1(u.id, u.http1_only),
                http1_pins().on_failure(u.id),
                |r| r.timeout(PROBE_TIMEOUT),
            )
            .await;
            let kind = match &resp {
                Ok(r) if r.status().is_success() => SampleKind::Hit,
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => SampleKind::Miss,
                Ok(_) | Err(_) => SampleKind::Error,
            };
            breakers().record(u.id, kind);
            if kind != SampleKind::Hit {
                return None;
            }
            let body = resp.ok()?.text().await.ok()?;
            if !is_trusted_narinfo(&u.public_key, path_hash, &body) {
                tracing::warn!(
                    upstream = %u.id,
                    path_hash,
                    "upstream narinfo is not signed by its configured public_key for this path; dropping"
                );
                return None;
            }
            Some(UpstreamNarinfo {
                upstream: u.id,
                body,
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

pub async fn lookup_upstream_narinfo(
    endpoints: Arc<Vec<UpstreamEndpoint>>,
    pool: Arc<Semaphore>,
    hash: String,
    store_path: String,
) -> ProbeResult {
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
                let hash = hash.clone();
                let path = store_path.clone();
                async move {
                    let (latency, kind, cp) = probe_one(&pool, ep, &hash, &path).await;
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
        let (latency, kind, cp) = probe_one(&pool, ep, &hash, &store_path).await;
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

#[derive(Debug, Default)]
pub struct BatchProbe {
    pub found: HashMap<String, CachedPath>,
    pub unanswered: HashMap<String, Vec<CacheUpstreamId>>,
    pub stats: HashMap<CacheUpstreamId, UpstreamAccum>,
}

pub async fn probe_batch(
    mut endpoints: Vec<UpstreamEndpoint>,
    pool: Arc<Semaphore>,
    targets: Vec<(String, String)>,
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
    let push = |futs: &mut FuturesUnordered<_>, hash: String, path: String| {
        let eps = Arc::clone(&endpoints);
        let pool = Arc::clone(&pool);
        futs.push(async move {
            let res = lookup_upstream_narinfo(eps, pool, hash.clone(), path).await;
            (hash, res)
        });
    };

    for _ in 0..BATCH_WINDOW {
        match iter.next() {
            Some((hash, path)) => push(&mut futs, hash, path),
            None => break,
        }
    }

    while let Some((hash, res)) = futs.next().await {
        fold_samples(&res.samples, &mut batch.stats);
        if let Some((_, cp)) = res.best {
            batch.found.insert(hash, cp);
        } else if !res.unanswered.is_empty() {
            batch.unanswered.insert(hash, res.unanswered);
        }

        if let Some((hash, path)) = iter.next() {
            push(&mut futs, hash, path);
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
    if !is_trusted_narinfo(public_key, path_hash, body) {
        tracing::warn!(
            upstream = %ep.id,
            path_hash,
            "upstream narinfo is not signed by its configured public_key for this path; dropping"
        );
        return None;
    }
    parse_upstream_narinfo(&ep.url, store_path, body)
}

fn is_trusted_narinfo(public_key: &str, path_hash: &str, body: &str) -> bool {
    names_path_hash(body, path_hash) && gradient_sources::verify_narinfo_signature(public_key, body)
}

fn names_path_hash(body: &str, path_hash: &str) -> bool {
    body.lines()
        .filter_map(|l| l.strip_prefix("StorePath: "))
        .next_back()
        .and_then(|p| gradient_sources::get_hash_from_path(p.trim().to_owned()).ok())
        .is_some_and(|(hash, _)| hash == path_hash)
}

pub fn parse_upstream_narinfo(base_url: &str, store_path: &str, body: &str) -> Option<CachedPath> {
    let mut nar_path: Option<&str> = None;
    let mut nar_hash: Option<String> = None;
    let mut file_hash: Option<String> = None;
    let mut nar_size: Option<u64> = None;
    let mut file_size: Option<u64> = None;
    let mut references: Option<Vec<String>> = None;
    let mut deriver: Option<String> = None;
    let mut ca: Option<String> = None;
    let mut sigs: Vec<String> = Vec::new();

    for line in body.lines() {
        if let Some(v) = line.strip_prefix("URL: ") {
            nar_path = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("NarHash: ") {
            nar_hash = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("FileHash: ") {
            file_hash = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("NarSize: ") {
            nar_size = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("FileSize: ") {
            file_size = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("References: ") {
            references = Some(
                v.split_whitespace()
                    .map(|r| {
                        if r.starts_with("/nix/store/") {
                            r.to_owned()
                        } else {
                            format!("/nix/store/{}", r)
                        }
                    })
                    .collect(),
            );
        } else if let Some(v) = line.strip_prefix("Deriver: ") {
            let d = v.trim();
            if !d.is_empty() {
                deriver = Some(if d.starts_with("/nix/store/") {
                    d.to_owned()
                } else {
                    format!("/nix/store/{}", d)
                });
            }
        } else if let Some(v) = line.strip_prefix("CA: ") {
            let c = v.trim();
            if !c.is_empty() {
                ca = Some(c.to_owned());
            }
        } else if let Some(v) = line.strip_prefix("Sig: ") {
            sigs.push(v.trim().to_owned());
        }
    }

    let nar_path = nar_path?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), nar_path);

    Some(CachedPath {
        path: store_path.to_string(),
        cached: true,
        file_size,
        nar_size,
        url: Some(url),
        nar_hash,
        file_hash,
        references,
        signatures: if sigs.is_empty() { None } else { Some(sigs) },
        deriver,
        ca,
    })
}

#[cfg(test)]
mod tests {
    use super::{BREAKER_COOLDOWN, SampleKind, TRIP_AFTER, UpstreamBreakers, breakers};
    use gradient_types::ids::CacheUpstreamId;
    use std::time::{Duration, Instant};

    fn upstream(n: u128) -> CacheUpstreamId {
        CacheUpstreamId::new(uuid::Uuid::from_u128(n))
    }

    #[tokio::test]
    async fn pin_writer_drains_on_shutdown() {
        let shutdown = gradient_util::shutdown::Shutdown::new();
        let db = gradient_db::WebDb::new(
            sea_orm::MockDatabase::new(sea_orm::DatabaseBackend::Postgres).into_connection(),
        );
        shutdown.spawn(super::persist_http1_pins(db, shutdown.token()));
        tokio::task::yield_now().await;

        assert!(shutdown.cancel_and_drain(Duration::from_secs(1)).await);
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

    async fn black_hole() -> (String, tokio::net::TcpListener) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        (format!("http://{addr}"), listener)
    }

    #[tokio::test]
    async fn a_black_hole_is_bounded_by_the_probe_timeout() {
        let (url, _listener) = black_hole().await;
        let probes = vec![super::UpstreamProbe {
            id: upstream(10),
            url,
            public_key: "test:0000000000000000000000000000000000000000000=".into(),
            http1_only: false,
        }];

        let started = Instant::now();
        let got = super::fetch_narinfo_body(&probes, "brj5bb4pny8pnngq3qdymkllwql6z29j").await;
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
            http1_only: false,
        }];

        let started = Instant::now();
        let got = super::fetch_narinfo_body(&probes, "brj5bb4pny8pnngq3qdymkllwql6z29j").await;
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
            url: server.uri(),
            ..ep(None, None)
        };
        (server, endpoint)
    }

    async fn lookup(endpoints: Vec<UpstreamEndpoint>) -> ProbeResult {
        lookup_upstream_narinfo(
            Arc::new(endpoints),
            Arc::new(Semaphore::new(4)),
            SIGNED_HASH.to_owned(),
            SIGNED_PATH.to_owned(),
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

    #[test]
    fn parse_upstream_narinfo_full_fields() {
        let body = "StorePath: /nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1\n\
                    URL: nar/abc.nar.xz\n\
                    Compression: xz\n\
                    FileHash: sha256:124l7vc762nsgl8wmfgp1gm9vsl3gk0j6136nyv3ff7s7da11yvz\n\
                    FileSize: 35372\n\
                    NarHash: sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf\n\
                    NarSize: 102760\n\
                    References: aaaa-dep1 /nix/store/bbbb-dep2\n\
                    Deriver: vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv\n\
                    Sig: cache.nixos.org-1:a84Gyv6ieXj7HclpmXu/i+so=\n";
        let cp = parse_upstream_narinfo(
            "https://upstream.example/",
            "/nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1",
            body,
        )
        .unwrap();
        assert!(cp.cached);
        assert_eq!(
            cp.url.as_deref(),
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
        assert!(refs.contains(&"/nix/store/aaaa-dep1".to_string()));
        assert!(refs.contains(&"/nix/store/bbbb-dep2".to_string()));
        assert_eq!(
            cp.deriver.as_deref(),
            Some("/nix/store/vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv")
        );
        assert_eq!(cp.signatures.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parse_upstream_narinfo_ca_field() {
        let body = "URL: nar/x.nar.xz\n\
                    NarHash: sha256:deadbeef\n\
                    NarSize: 1\n\
                    CA: fixed:sha256:0abc\n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.ca.as_deref(), Some("fixed:sha256:0abc"));
    }

    #[test]
    fn parse_upstream_narinfo_empty_references_is_some_empty() {
        let body = "URL: nar/x.nar.xz\n\
                    NarHash: sha256:deadbeef\n\
                    NarSize: 1\n\
                    References: \n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.references.as_deref(), Some(&[][..]));
    }

    #[test]
    fn parse_upstream_narinfo_requires_url() {
        let body = "NarHash: sha256:abc\nNarSize: 1\n";
        assert!(parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).is_none());
    }

    #[test]
    fn parse_upstream_narinfo_trims_base_url_trailing_slash() {
        let body = "URL: nar/x.nar\n";
        let cp = parse_upstream_narinfo("https://up.example/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.url.as_deref(), Some("https://up.example/nar/x.nar"));
    }

    #[test]
    fn parse_upstream_narinfo_ignores_unparseable_sizes() {
        let body = "URL: nar/x.nar\nNarSize: not-a-number\nFileSize: also-bad\n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert!(cp.nar_size.is_none());
        assert!(cp.file_size.is_none());
    }

    const SIGNED_HASH: &str = "brj5bb4pny8pnngq3qdymkllwql6z29j";
    const SIGNED_PATH: &str = "/nix/store/brj5bb4pny8pnngq3qdymkllwql6z29j-hello-2.12";

    fn signed_narinfo(store_path: &str) -> (String, String) {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let keypair = ed25519_compact::KeyPair::generate();
        let nar_hash = "sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";
        let fingerprint = format!("1;{store_path};{nar_hash};120;");
        let sig = STANDARD.encode(*keypair.sk.sign(fingerprint, None));
        let body = format!(
            "StorePath: {store_path}\nURL: nar/x.nar.xz\nNarHash: {nar_hash}\nNarSize: 120\n\
             References: \nSig: upstream-1:{sig}\n"
        );
        (body, format!("upstream-1:{}", STANDARD.encode(*keypair.pk)))
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
        assert_eq!(cp.url.as_deref(), Some("https://up.example/nar/x.nar.xz"));
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
    fn an_upstream_without_a_key_serves_nothing() {
        let (body, _) = signed_narinfo(SIGNED_PATH);
        assert!(verified_narinfo(&ep_with_key(None), SIGNED_HASH, SIGNED_PATH, &body).is_none());
    }

    fn ep(latency: Option<f64>, hit: Option<f64>) -> UpstreamEndpoint {
        UpstreamEndpoint {
            id: CacheUpstreamId::now_v7(),
            url: "https://up.example/".into(),
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
