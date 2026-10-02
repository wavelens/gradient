/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt as _};
use gradient_derivation::parse_drv;
use gradient_util::store_path::nix_store_path;
use gradient_wire::CachedPathInfo;
use gradient_wire::messages::{BuildSpec, CachedPath, EvalMessageLevel, QueryMode};
use gradient_wire::types::JobPhase;
use tracing::{debug, error, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::compression::drv_closure_seeds_from_compressed_nar;
use crate::proto::job::JobUpdater;
use crate::proto::nar_daemon_import::import_received_nar;
use crate::proto::progress::{Progress, ProgressSink, read_body};
use gradient_worker_client::compression::resolve_compression;
use gradient_worker_client::nar_recv::NarPayload;

const PREFETCH_CONCURRENCY: usize = 8;

const PRESIGNED_DOWNLOAD_MAX_ATTEMPTS: u32 = 4;

const PRESIGNED_RETRY_BASE: Duration = Duration::from_millis(500);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Prefetched {
    pub paths: u32,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct MissingInputs(pub Vec<String>);

impl std::fmt::Display for MissingInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} required input path(s) are missing from local store and not available in the gradient cache; cannot build (first: {})",
            self.0.len(),
            self.0.first().map(String::as_str).unwrap_or("<none>")
        )
    }
}

impl std::error::Error for MissingInputs {}

#[derive(Debug)]
pub struct CorruptCachedNar(pub String);

impl std::fmt::Display for CorruptCachedNar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cached NAR for {} failed integrity verification (stored bytes do not match recorded nar_hash/nar_size)",
            self.0
        )
    }
}

impl std::error::Error for CorruptCachedNar {}

#[derive(Debug)]
pub struct SubstituteNotOnUpstream(pub String);

impl std::fmt::Display for SubstituteNotOnUpstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "substitute {}: not available on any upstream cache",
            self.0
        )
    }
}

impl std::error::Error for SubstituteNotOnUpstream {}

fn presigned_status_is_missing(status: u16) -> bool {
    matches!(status, 404 | 410)
}

fn presigned_status_is_retryable(status: u16) -> bool {
    matches!(status, 408 | 429) || status >= 500
}

fn presigned_body_is_short(declared: Option<u64>, received: usize) -> bool {
    declared.is_some_and(|want| want != received as u64)
}

type PresignedFetch = (String, Option<(Vec<u8>, CachedPath)>);

pub(crate) async fn download_one_presigned(
    http: &reqwest::Client,
    cp: CachedPath,
    progress: &mut Progress<impl ProgressSink>,
) -> Result<PresignedFetch> {
    let url = cp.url.clone().expect("by_url entries have a URL");
    let path = cp.path.clone();
    let mut backoff = PRESIGNED_RETRY_BASE;

    for attempt in 1..=PRESIGNED_DOWNLOAD_MAX_ATTEMPTS {
        let started = std::time::Instant::now();
        let attempt_err = match http.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if presigned_status_is_missing(status) {
                    warn!(
                        %path,
                        status,
                        "presigned NAR missing: cached_path claims this object but the bucket \
                         returned {status}; treating as a missing input (self-heal demotes it)"
                    );
                    return Ok((path, None));
                }
                if presigned_status_is_retryable(status) {
                    anyhow::anyhow!("HTTP {status} from {url}")
                } else if !resp.status().is_success() {
                    return Err(anyhow::anyhow!(
                        "HTTP {status} from {url} (path {path}) is not a usable NAR response"
                    ));
                } else {
                    let bytes = read_body(resp, cp.file_size, progress)
                        .await
                        .with_context(|| format!("read body of {url}"))?;
                    if presigned_body_is_short(cp.file_size, bytes.len()) {
                        warn!(
                            %path,
                            declared = ?cp.file_size,
                            received = bytes.len(),
                            "presigned NAR body does not match the declared file_size; \
                             treating as a missing input (self-heal demotes it)"
                        );
                        return Ok((path, None));
                    }

                    gradient_worker_client::throughput::NETWORK
                        .observe_transfer(bytes.len() as u64, started.elapsed());
                    return Ok((path, Some((bytes, cp))));
                }
            }
            Err(e) => anyhow::Error::new(e).context(format!("HTTP GET {url} (path {path})")),
        };

        if attempt < PRESIGNED_DOWNLOAD_MAX_ATTEMPTS {
            warn!(
                %path,
                attempt,
                error = format!("{attempt_err:#}"),
                "presigned download failed; retrying"
            );
            tokio::time::sleep(backoff).await;
            backoff *= 2;
        } else {
            return Err(attempt_err.context(format!(
                "presigned download for {path} failed after {PRESIGNED_DOWNLOAD_MAX_ATTEMPTS} attempts"
            )));
        }
    }

    unreachable!("loop returns on the final attempt")
}

struct InputPrefetcher<'a> {
    store: &'a LocalNixStore,
    drv_path: String,
    build_id: String,
    updater: &'a mut JobUpdater,
}

impl<'a> InputPrefetcher<'a> {
    fn new(store: &'a LocalNixStore, task: &'a BuildSpec, updater: &'a mut JobUpdater) -> Self {
        Self {
            store,
            drv_path: task.drv_path.clone(),
            build_id: task.build_id.clone(),
            updater,
        }
    }

    fn for_path(store: &'a LocalNixStore, label: String, updater: &'a mut JobUpdater) -> Self {
        Self {
            store,
            drv_path: label.clone(),
            build_id: label,
            updater,
        }
    }

    /// The target `.drv` is usually missing here because evaluation ran on another worker.
    /// The fetch is pulling its whole reference chain too.
    /// `add_to_store_nar` is rejecting a `.drv` whose declared references are absent locally.
    async fn ensure_self_drv_present(&mut self) -> Result<()> {
        let full_drv_path = nix_store_path(&self.drv_path);
        if tokio::fs::try_exists(&full_drv_path).await.unwrap_or(false) {
            return Ok(());
        }

        debug!(
            build_id = %self.build_id,
            drv = %self.drv_path,
            "build target drv absent locally; fetching from server cache"
        );

        self.fetch_closure(vec![self.drv_path.to_owned()]).await?;

        if !tokio::fs::try_exists(&full_drv_path).await.unwrap_or(false) {
            return Err(anyhow::anyhow!(
                "build target drv {} still missing after fetch+import",
                full_drv_path
            ));
        }

        Ok(())
    }

    async fn enumerate_inputs(&self) -> Result<HashSet<String>> {
        let drv = read_local_drv(&self.drv_path).await?;
        let mut wanted: HashSet<String> = drv.input_sources.iter().cloned().collect();
        for (input_drv_path, outputs) in &drv.input_derivations {
            let input_drv = read_local_drv(input_drv_path).await?;
            wanted.extend(input_drv.requested_output_paths(outputs).map(str::to_owned));
        }

        Ok(wanted)
    }

    async fn filter_missing(&self, wanted: HashSet<String>) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for p in wanted {
            match self.store.has_path(&p).await {
                Ok(true) => {}
                Ok(false) => missing.push(p),
                Err(e) => {
                    error!(path = %p, error = %e, "store.has_path failed during prefetch; aborting build");
                    return Err(anyhow::anyhow!("store.has_path failed for {}: {}", p, e));
                }
            }
        }
        Ok(missing)
    }

    async fn query_and_split(
        &mut self,
        missing: Vec<String>,
    ) -> Result<(Vec<CachedPath>, Vec<CachedPath>)> {
        let cached_entries = self
            .updater
            .query_cache(missing.clone(), QueryMode::Pull)
            .await
            .with_context(|| {
                format!(
                    "CacheQuery Pull for {} missing inputs of {}",
                    missing.len(),
                    self.drv_path
                )
            })?;

        let Classified {
            by_url,
            by_request,
            uncached,
        } = classify_cached_entries(&missing, cached_entries);

        if !uncached.is_empty() {
            error!(
                build_id = %self.build_id,
                drv = %self.drv_path,
                missing = uncached.len(),
                sample = ?uncached.iter().take(5).collect::<Vec<_>>(),
                "prefetch: our cache cannot serve required inputs"
            );
            return Err(anyhow::Error::new(MissingInputs(uncached)));
        }

        Ok((by_url, by_request))
    }

    async fn fetch_by_request(
        &mut self,
        by_request: Vec<CachedPath>,
    ) -> Result<Vec<(String, NarPayload, CachedPath)>> {
        if by_request.is_empty() {
            return Ok(vec![]);
        }

        let paths: Vec<String> = by_request.iter().map(|c| c.path.clone()).collect();
        let nars_by_path = self.updater.request_nars(paths).await?;

        let mut meta_by_path: HashMap<String, CachedPath> = by_request
            .into_iter()
            .map(|c| (c.path.clone(), c))
            .collect();

        let results = nars_by_path
            .into_iter()
            .filter_map(|(path, nar)| meta_by_path.remove(&path).map(|meta| (path, nar, meta)))
            .collect();

        Ok(results)
    }

    async fn download_by_url(
        &self,
        by_url: Vec<CachedPath>,
    ) -> Result<Vec<(String, NarPayload, CachedPath)>> {
        if by_url.is_empty() {
            return Ok(vec![]);
        }

        let http = gradient_worker_client::http::download_client();

        let outcomes: Vec<Result<PresignedFetch>> =
            futures::stream::iter(by_url.into_iter().map(|cp| {
                let http = http.clone();
                async move { download_one_presigned(&http, cp, &mut Progress::silent()).await }
            }))
            .buffer_unordered(PREFETCH_CONCURRENCY)
            .collect()
            .await;

        let mut results = Vec::new();
        let mut missing = Vec::new();
        for outcome in outcomes {
            let (path, fetched) = outcome.context("presigned NAR download failed")?;
            match fetched {
                Some((bytes, cp)) => results.push((path, NarPayload::Bytes(bytes), cp)),
                None => missing.push(path),
            }
        }

        if !missing.is_empty() {
            return Err(anyhow::Error::new(MissingInputs(missing)));
        }

        Ok(results)
    }

    async fn import_all(&self, results: Vec<(String, NarPayload, CachedPath)>) -> Result<usize> {
        let store = self.store;
        let total = results.len();
        if total == 0 {
            return Ok(0);
        }

        let download_paths: HashSet<String> = results.iter().map(|(p, _, _)| p.clone()).collect();

        let mut payload: HashMap<String, (NarPayload, CachedPath)> =
            results.into_iter().map(|(p, n, m)| (p, (n, m))).collect();

        let mut pending_deps: HashMap<String, HashSet<String>> = HashMap::new();
        let mut wanted_by: HashMap<String, Vec<String>> = HashMap::new();

        for (path, (_, meta)) in &payload {
            let refs = meta.references.clone().unwrap_or_default();
            let restricted: HashSet<String> = refs
                .into_iter()
                .filter(|r| r != path && download_paths.contains(r))
                .collect();
            for r in &restricted {
                wanted_by.entry(r.clone()).or_default().push(path.clone());
            }
            pending_deps.insert(path.clone(), restricted);
        }

        let mut ready: Vec<String> = pending_deps
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(p, _)| p.clone())
            .collect();

        let mut imports: FuturesUnordered<_> = FuturesUnordered::new();
        let mut completed = 0usize;

        loop {
            while !ready.is_empty() && imports.len() < PREFETCH_CONCURRENCY {
                let path = ready.pop().expect("ready is non-empty");
                let (nar, meta) = payload
                    .remove(&path)
                    .expect("payload present for ready path");
                pending_deps.remove(&path);
                imports.push(async move {
                    let result = import_received_nar(store, &path, nar, &meta)
                        .await
                        .with_context(|| format!("import {} into local store", path));
                    (path, result)
                });
            }

            let Some((path, result)) = imports.next().await else {
                break;
            };

            completed += 1;
            if let Err(e) = result {
                error!(path = %path, error = ?e, "dep NAR import failed; aborting prefetch");
                return Err(e.context(format!("prefetch import failed for {}", path)));
            }

            if let Some(kids) = wanted_by.remove(&path) {
                for k in kids {
                    if let Some(deps) = pending_deps.get_mut(&k) {
                        deps.remove(&path);
                        if deps.is_empty() {
                            ready.push(k);
                        }
                    }
                }
            }
        }

        if !pending_deps.is_empty() {
            warn!(
                remaining = pending_deps.len(),
                "topo import left paths unimported (cycle in references?)"
            );
        }

        Ok(completed)
    }

    async fn run(&mut self) -> Result<Prefetched> {
        self.ensure_self_drv_present().await?;

        let wanted = self.enumerate_inputs().await?;
        if wanted.is_empty() {
            return Ok(Prefetched::default());
        }

        let initial_missing = self.filter_missing(wanted).await?;
        if initial_missing.is_empty() {
            debug!(
                build_id = %self.build_id,
                "all inputs already in local store; no prefetch needed"
            );
            return Ok(Prefetched::default());
        }
        self.fetch_closure(initial_missing).await
    }

    async fn fetch_closure(&mut self, initial_missing: Vec<String>) -> Result<Prefetched> {
        const MAX_ITERATIONS: usize = 1024;

        debug!(
            build_id = %self.build_id,
            missing = initial_missing.len(),
            "prefetching missing paths from server cache (closure-expanding)"
        );

        let mut all_results: Vec<(String, NarPayload, CachedPath)> = Vec::new();
        let mut fetched_bytes = 0u64;
        let mut queried: HashSet<String> = initial_missing.iter().cloned().collect();
        let mut to_query: Vec<String> = initial_missing;
        let mut iterations = 0usize;

        while !to_query.is_empty() {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                warn!(
                    build_id = %self.build_id,
                    pending = to_query.len(),
                    "closure expansion exceeded MAX_ITERATIONS; proceeding with what we have"
                );
                break;
            }

            let (by_url, by_request) = self.query_and_split(to_query).await?;
            let batch = self.fetch_round(by_url, by_request).await?;
            fetched_bytes += payload_bytes(&batch).await;

            for (path, _, meta) in &batch {
                tracing::trace!(
                    path = %path,
                    refs = ?meta.references.as_ref().map(|r| r.len()).unwrap_or(0),
                    "closure walk: examining references"
                );
            }
            let mut refs: HashSet<String> = batch
                .iter()
                .flat_map(|(_, _, meta)| meta.references.clone().unwrap_or_default())
                .filter(|r| !queried.contains(r))
                .collect();

            // The `.drv` inputs are read from the parsed file, not `cached_path.references`.
            // The eval worker is storing `NULL` references when its metadata query fails.
            for (path, nar, meta) in &batch {
                if !path.ends_with(".drv") {
                    continue;
                }
                let bytes = match nar.read_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        warn!(drv = %path, error = %e, "could not read staged .drv for closure seeds");
                        continue;
                    }
                };
                let compression = resolve_compression(&bytes, meta.url.as_deref());
                for seed in drv_closure_seeds_from_compressed_nar(&bytes, compression, path).await {
                    if !queried.contains(&seed) {
                        tracing::trace!(
                            drv = %path,
                            seed = %seed,
                            "closure walk: discovered drv-content seed"
                        );
                        refs.insert(seed);
                    }
                }
            }

            all_results.extend(batch);

            let mut next_batch = Vec::with_capacity(refs.len());
            for r in refs {
                match self.store.has_path(&r).await {
                    Ok(true) => {
                        tracing::trace!(path = %r, "closure walk: ref already in local store");
                        queried.insert(r);
                    }
                    Ok(false) => {
                        tracing::trace!(path = %r, "closure walk: ref missing locally, queuing");
                        queried.insert(r.clone());
                        next_batch.push(r);
                    }
                    Err(e) => {
                        error!(
                            path = %r,
                            error = %e,
                            "store.has_path failed during closure expansion; aborting build"
                        );
                        return Err(anyhow::anyhow!("store.has_path failed for {}: {}", r, e));
                    }
                }
            }

            to_query = next_batch;
        }

        let total_queried = all_results.len();
        debug!(
            build_id = %self.build_id,
            iterations,
            total_downloaded = total_queried,
            "closure expansion complete"
        );

        let mut import = self.updater.phase(JobPhase::NarImport);
        let imported = self.import_all(all_results).await? as u32;
        import.record(imported, fetched_bytes);
        debug!(build_id = %self.build_id, imported, "prefetch complete");

        Ok(Prefetched {
            paths: imported,
            bytes: fetched_bytes,
        })
    }

    async fn fetch_round(
        &mut self,
        by_url: Vec<CachedPath>,
        by_request: Vec<CachedPath>,
    ) -> Result<Vec<(String, NarPayload, CachedPath)>> {
        let mut fetch = self.updater.phase(JobPhase::NarFetch);
        let mut batch = self.fetch_by_request(by_request).await?;
        batch.extend(self.download_by_url(by_url).await?);
        fetch.record(batch.len() as u32, payload_bytes(&batch).await);
        Ok(batch)
    }
}

async fn payload_bytes(batch: &[(String, NarPayload, CachedPath)]) -> u64 {
    let mut bytes = 0;
    for (_, nar, _) in batch {
        bytes += nar.byte_len().await;
    }

    bytes
}

pub async fn prefetch_inputs(
    store: &LocalNixStore,
    task: &BuildSpec,
    updater: &mut JobUpdater,
) -> Result<Prefetched> {
    let drv = task.drv_path.clone();
    let result = InputPrefetcher::new(store, task, updater).run().await;
    if let Err(e) = &result {
        let summary = format!("input prefetch failed for {}: {:#}", drv, e);
        if let Err(send_err) = updater
            .send_eval_message(EvalMessageLevel::Error, "build-prefetch", summary)
            .await
        {
            warn!(error = %send_err, "failed to surface prefetch error as EvalMessage");
        }
    }
    result
}

pub async fn ensure_path(
    store: &LocalNixStore,
    path: &str,
    updater: &mut JobUpdater,
) -> Result<()> {
    if store.has_path(path).await? {
        return Ok(());
    }
    InputPrefetcher::for_path(store, path.to_owned(), updater)
        .fetch_closure(vec![path.to_owned()])
        .await?;
    Ok(())
}

async fn read_local_drv(drv_path: &str) -> Result<gradient_derivation::Derivation> {
    let full = nix_store_path(drv_path);
    let bytes = tokio::fs::read(&full)
        .await
        .with_context(|| format!("read .drv {full} for prefetch"))?;
    parse_drv(&bytes).with_context(|| format!("parse .drv {full} for prefetch"))
}

#[derive(Debug, Default)]
struct Classified {
    by_url: Vec<CachedPath>,
    by_request: Vec<CachedPath>,
    uncached: Vec<String>,
}

fn classify_cached_entries(asked: &[String], entries: Vec<CachedPath>) -> Classified {
    let mut out = Classified::default();
    for cp in entries {
        match cp.as_info() {
            CachedPathInfo::Uncached { path, .. } => {
                out.uncached.push(path.to_owned());
            }
            CachedPathInfo::Cached { download_url, .. } => {
                if download_url.is_some() {
                    out.by_url.push(cp);
                } else {
                    out.by_request.push(cp);
                }
            }
        }
    }

    let answered: HashSet<&str> = out
        .by_url
        .iter()
        .chain(out.by_request.iter())
        .map(|cp| cp.path.as_str())
        .chain(out.uncached.iter().map(String::as_str))
        .collect();
    let omitted: Vec<String> = asked
        .iter()
        .filter(|p| !answered.contains(p.as_str()))
        .cloned()
        .collect();
    out.uncached.extend(omitted);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(path: &str, url: Option<&str>) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: url.map(|s| s.to_owned()),
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    fn uncached(path: &str) -> CachedPath {
        CachedPath {
            path: path.to_owned(),
            cached: false,
            file_size: None,
            nar_size: None,
            url: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    #[test]
    fn classify_splits_cached_by_url_presence() {
        let asked = vec![
            "/nix/store/aaaa-by-url".to_owned(),
            "/nix/store/bbbb-by-ws".to_owned(),
        ];
        let out = classify_cached_entries(
            &asked,
            vec![
                cached("/nix/store/aaaa-by-url", Some("https://s3.example/x")),
                cached("/nix/store/bbbb-by-ws", None),
            ],
        );
        assert_eq!(out.by_url.len(), 1);
        assert_eq!(out.by_request.len(), 1);
        assert!(out.uncached.is_empty());
        assert_eq!(out.by_url[0].path, "/nix/store/aaaa-by-url");
        assert_eq!(out.by_request[0].path, "/nix/store/bbbb-by-ws");
    }

    #[test]
    fn classify_collects_uncached_separately() {
        let asked = vec![
            "/nix/store/aaaa-ok".to_owned(),
            "/nix/store/xxxx-missing-upstream".to_owned(),
            "/nix/store/yyyy-also-missing".to_owned(),
        ];
        let out = classify_cached_entries(
            &asked,
            vec![
                cached("/nix/store/aaaa-ok", None),
                uncached("/nix/store/xxxx-missing-upstream"),
                uncached("/nix/store/yyyy-also-missing"),
            ],
        );
        assert_eq!(out.by_request.len(), 1);
        assert!(out.by_url.is_empty());
        assert_eq!(
            out.uncached,
            vec![
                "/nix/store/xxxx-missing-upstream".to_owned(),
                "/nix/store/yyyy-also-missing".to_owned(),
            ]
        );
    }

    #[test]
    fn classify_empty_input_is_empty_output() {
        let out = classify_cached_entries(&[], vec![]);
        assert!(out.by_url.is_empty());
        assert!(out.by_request.is_empty());
        assert!(out.uncached.is_empty());
    }

    #[test]
    fn a_path_the_reply_omits_is_uncached() {
        let asked = vec![
            "/nix/store/aaaa-served".to_owned(),
            "/nix/store/bbbb-omitted".to_owned(),
        ];

        let out = classify_cached_entries(&asked, vec![cached("/nix/store/aaaa-served", None)]);

        assert_eq!(out.by_request.len(), 1);
        assert_eq!(out.uncached, vec!["/nix/store/bbbb-omitted".to_owned()]);
    }

    #[test]
    fn an_uncached_entry_is_not_also_counted_as_omitted() {
        let asked = vec!["/nix/store/xxxx-missing".to_owned()];

        let out = classify_cached_entries(&asked, vec![uncached("/nix/store/xxxx-missing")]);

        assert_eq!(out.uncached, vec!["/nix/store/xxxx-missing".to_owned()]);
    }

    #[test]
    fn short_body_contradicts_a_declared_file_size() {
        assert!(presigned_body_is_short(Some(227840), 0));
        assert!(presigned_body_is_short(Some(227840), 227839));
        assert!(!presigned_body_is_short(Some(227840), 227840));
        assert!(!presigned_body_is_short(None, 0));
    }

    #[tokio::test]
    async fn presigned_download_follows_a_redirect_to_object_storage() {
        use wiremock::matchers::{method, path as path_matcher};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let objects = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"NAR-BYTES".to_vec()))
            .mount(&objects)
            .await;

        let cache = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/nar/abc.nar"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", &*format!("{}/blob", objects.uri())),
            )
            .mount(&cache)
            .await;

        let mut cp = cached(
            "/nix/store/aaaa-redirected",
            Some(&format!("{}/nar/abc.nar", cache.uri())),
        );
        cp.file_size = Some(9);

        let http = gradient_util::http::build_download_client().expect("download client");
        let (path, fetched) = download_one_presigned(&http, cp, &mut Progress::silent())
            .await
            .expect("download");
        assert_eq!(path, "/nix/store/aaaa-redirected");
        let (bytes, _) = fetched.expect("redirect followed to the object");
        assert_eq!(bytes, b"NAR-BYTES");
    }

    #[tokio::test]
    async fn a_presigned_download_feeds_the_network_throughput() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let objects = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"NAR-BYTES".to_vec()))
            .mount(&objects)
            .await;
        let cp = cached(
            "/nix/store/aaaa-measured",
            Some(&format!("{}/nar/abc.nar", objects.uri())),
        );

        let http = gradient_util::http::build_download_client().expect("download client");
        download_one_presigned(&http, cp, &mut Progress::silent())
            .await
            .expect("download");

        assert!(
            gradient_worker_client::throughput::NETWORK
                .current()
                .is_some()
        );
    }

    #[tokio::test]
    async fn redirect_swallowed_as_empty_body_is_not_a_download() {
        use wiremock::matchers::{method, path as path_matcher};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let cache = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/nar/abc.nar"))
            .respond_with(
                ResponseTemplate::new(307).insert_header("location", "https://elsewhere.invalid/o"),
            )
            .mount(&cache)
            .await;

        let mut cp = cached(
            "/nix/store/aaaa-redirected",
            Some(&format!("{}/nar/abc.nar", cache.uri())),
        );
        cp.file_size = Some(227840);

        let http = gradient_util::http::build_client().expect("api client");
        let err = download_one_presigned(&http, cp, &mut Progress::silent())
            .await
            .expect_err("an unfollowed 3xx is not a usable NAR response");
        assert!(
            format!("{err:#}").contains("307"),
            "error must name the status: {err:#}"
        );
    }

    #[tokio::test]
    async fn truncated_body_is_reported_as_a_miss() {
        use wiremock::matchers::{method, path as path_matcher};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let cache = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/nar/abc.nar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"short".to_vec()))
            .mount(&cache)
            .await;

        let mut cp = cached(
            "/nix/store/aaaa-truncated",
            Some(&format!("{}/nar/abc.nar", cache.uri())),
        );
        cp.file_size = Some(227840);

        let http = gradient_util::http::build_download_client().expect("download client");
        let (path, fetched) = download_one_presigned(&http, cp, &mut Progress::silent())
            .await
            .expect("download");
        assert_eq!(path, "/nix/store/aaaa-truncated");
        assert!(
            fetched.is_none(),
            "a body shorter than the declared file_size must be a miss, not bytes"
        );
    }

    #[test]
    fn presigned_404_410_are_missing_inputs_other_statuses_retry() {
        assert!(presigned_status_is_missing(404));
        assert!(presigned_status_is_missing(410));
        for retryable in [200, 403, 429, 500, 502, 503] {
            assert!(
                !presigned_status_is_missing(retryable),
                "status {retryable} must stay retryable, not a missing input"
            );
        }
    }

    #[test]
    fn presigned_retryable_statuses_are_timeout_rate_limit_and_5xx() {
        for s in [408, 429, 500, 502, 503, 504] {
            assert!(presigned_status_is_retryable(s), "status {s} must retry");
        }
        for s in [400, 403, 404, 410] {
            assert!(
                !presigned_status_is_retryable(s),
                "status {s} must not retry"
            );
        }
    }
}
