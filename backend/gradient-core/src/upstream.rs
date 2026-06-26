/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Upstream binary-cache narinfo lookup, shared by the cache-query handler
//! (worker pulls) and the eval-time substitutability probe (scheduler). Given a
//! set of upstream base URLs and a store-path hash, fetch and parse the
//! `<hash>.narinfo` into a [`CachedPath`] carrying the absolute NAR URL plus the
//! metadata needed to import the path.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Semaphore;

use gradient_db::{UpstreamAccum, UpstreamEndpoint};
use gradient_types::ids::CacheUpstreamId;
use gradient_types::proto::CachedPath;

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

const PROBE_TIMEOUT_SECS: u64 = 5;
const BATCH_WINDOW: usize = 256;

pub struct ProbeResult {
    pub best: Option<(CacheUpstreamId, CachedPath)>,
    pub samples: Vec<ProbeSample>,
}

async fn probe_one(
    http: &reqwest::Client,
    pool: &Arc<Semaphore>,
    ep: &UpstreamEndpoint,
    hash: &str,
    store_path: &str,
) -> (f64, SampleKind, Option<CachedPath>) {
    let _permit = pool.acquire().await;
    let narinfo_url = format!("{}/{}.narinfo", ep.url.trim_end_matches('/'), hash);
    let started = Instant::now();
    let resp = http
        .get(&narinfo_url)
        .timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
        .send()
        .await;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

    match resp {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(body) => match parse_upstream_narinfo(&ep.url, store_path, &body) {
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
    }
}

pub async fn lookup_upstream_narinfo(
    http: reqwest::Client,
    endpoints: Arc<Vec<UpstreamEndpoint>>,
    pool: Arc<Semaphore>,
    hash: String,
    store_path: String,
) -> ProbeResult {
    let mut samples = Vec::new();

    if should_race(endpoints.len()) {
        use futures::stream::{FuturesUnordered, StreamExt as _};
        let mut futs: FuturesUnordered<_> = endpoints
            .iter()
            .map(|ep| {
                let http = http.clone();
                let pool = Arc::clone(&pool);
                let hash = hash.clone();
                let path = store_path.clone();
                async move {
                    let (latency, kind, cp) = probe_one(&http, &pool, ep, &hash, &path).await;
                    (ep.id, latency, kind, cp)
                }
            })
            .collect();

        let mut results = Vec::new();
        while let Some((id, latency, kind, cp)) = futs.next().await {
            samples.push(ProbeSample { upstream: id, latency_ms: latency, kind });
            results.push((id, latency, cp));
        }

        let best = select_best_hit(results);
        return ProbeResult { best, samples };
    }

    for ep in endpoints.iter() {
        let (latency, kind, cp) = probe_one(&http, &pool, ep, &hash, &store_path).await;
        let is_hit = matches!(kind, SampleKind::Hit);
        samples.push(ProbeSample { upstream: ep.id, latency_ms: latency, kind });
        if is_hit && let Some(cp) = cp {
            return ProbeResult {
                best: Some((ep.id, cp)),
                samples,
            };
        }
    }

    ProbeResult { best: None, samples }
}

pub async fn probe_batch(
    http: reqwest::Client,
    mut endpoints: Vec<UpstreamEndpoint>,
    pool: Arc<Semaphore>,
    targets: Vec<(String, String)>,
) -> (
    HashMap<String, CachedPath>,
    HashMap<CacheUpstreamId, UpstreamAccum>,
) {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let mut found = HashMap::new();
    let mut stats = HashMap::new();
    if targets.is_empty() || endpoints.is_empty() {
        return (found, stats);
    }

    order_endpoints(&mut endpoints);
    let endpoints = Arc::new(endpoints);

    let mut futs = FuturesUnordered::new();
    let mut iter = targets.into_iter();
    let push = |futs: &mut FuturesUnordered<_>, hash: String, path: String| {
        let http = http.clone();
        let eps = Arc::clone(&endpoints);
        let pool = Arc::clone(&pool);
        futs.push(async move {
            let res = lookup_upstream_narinfo(http, eps, pool, hash.clone(), path).await;
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
        fold_samples(&res.samples, &mut stats);
        if let Some((_, cp)) = res.best {
            found.insert(hash, cp);
        }

        if let Some((hash, path)) = iter.next() {
            push(&mut futs, hash, path);
        }
    }

    (found, stats)
}

/// Parse a narinfo `body` into a [`CachedPath`]. The `URL:` field is resolved
/// against `base_url` into an absolute NAR URL; `None` if the body has no `URL:`.
pub fn parse_upstream_narinfo(
    base_url: &str,
    store_path: &str,
    body: &str,
) -> Option<CachedPath> {
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
    use super::*;

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

    fn ep(latency: Option<f64>, hit: Option<f64>) -> UpstreamEndpoint {
        UpstreamEndpoint {
            id: CacheUpstreamId::now_v7(),
            url: "https://up.example/".into(),
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
            ProbeSample { upstream: a, latency_ms: 10.0, kind: SampleKind::Hit },
            ProbeSample { upstream: a, latency_ms: 20.0, kind: SampleKind::Miss },
            ProbeSample { upstream: a, latency_ms: 5000.0, kind: SampleKind::Error },
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
