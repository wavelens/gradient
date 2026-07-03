/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Server-to-worker input prefetch: ensure every input a build needs is
//! present in the local nix store before the build is handed off.
//!
//! `prefetch_inputs` drives an [`InputPrefetcher`] pipeline:
//!
//! ```text
//! enumerate_inputs  →  HashSet<String>        (all input paths)
//! filter_missing    →  Vec<String>             (only what's absent from store)
//! query_and_split   →  (by_url, by_request)   (split by download method)
//! fetch_by_request  →  Vec<(path, nar, meta)>  (request over WS)
//! download_by_url   →  Vec<(path, nar, meta)>  (HTTP download from S3)
//! import_all        →  usize                   (stream into nix-daemon)
//! ```

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt as _};
use gradient_db::parse_drv;
use gradient_exec::path_utils::nix_store_path;
use gradient_proto::messages::{
    BuildTask, CachedPath, EvalMessageLevel, QueryMode, TRANSFER_TIMEOUT,
};
use gradient_types::CachedPathInfo;
use tracing::{debug, error, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::compression::{
    Compression, detect_compression, drv_closure_seeds_from_compressed_nar,
};
use crate::proto::job::JobUpdater;
use crate::proto::nar_daemon_import::import_received_nar;

/// How many missing inputs to download + import in parallel before invoking
/// the build. Conservative - each one streams a NAR into the local daemon
/// and we don't want to swamp the AddToStoreNar queue.
const PREFETCH_CONCURRENCY: usize = 8;

/// Attempts for a single presigned S3 download before giving up. The cache's
/// object store can flake at the transport layer (TLS handshake resets,
/// connection drops) under concurrent load; retrying a few times turns a
/// transient edge failure into a successful fetch instead of a failed build.
const PRESIGNED_DOWNLOAD_MAX_ATTEMPTS: u32 = 4;

/// Base backoff before the first presigned-download retry; doubled each attempt.
const PRESIGNED_RETRY_BASE: Duration = Duration::from_millis(500);

/// Required input store paths the gradient cache could not serve. Carried as a
/// typed error so the executor classifies the failure as
/// `BuildFailureKind::InputsUnavailable` and forwards the paths to the server,
/// which demotes those outputs and re-queues their producers (#410).
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

/// A cached NAR we fetched fails its own recorded integrity (`nar_hash` /
/// `nar_size`): the object bytes in our store do not match the metadata the
/// server signs and serves. This happens when object and `cached_path` metadata
/// are written by different producers (e.g. a non-reproducible local build vs an
/// upstream substitute relay) and desync. The path is treated as a missing input
/// so the server demotes the corrupt object and rebuilds it with consistent
/// metadata, rather than retrying forever against poison.
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

/// A substitute output is *genuinely* absent from every upstream cache (the
/// CacheQuery Pull reported it uncached everywhere), as opposed to a transient
/// timeout/transport failure during the relay. Only this case makes escalating
/// the anchor to a real build sound; transient relay failures must retry as a
/// substitute instead of counting toward the miss-escalation threshold.
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

/// True when a presigned download's HTTP status means the object is genuinely
/// absent (treat as a missing input, self-heal) rather than a retryable
/// transport error: 404 Not Found / 410 Gone.
fn presigned_status_is_missing(status: u16) -> bool {
    matches!(status, 404 | 410)
}

/// True when a presigned download's HTTP status is worth retrying: request
/// timeout, rate limiting, or any 5xx (the object store is briefly unhealthy,
/// not the object missing). 404/410 are handled as missing; other 4xx are
/// terminal client errors.
fn presigned_status_is_retryable(status: u16) -> bool {
    matches!(status, 408 | 429) || status >= 500
}

/// One presigned download's outcome: the store path, and `Some((bytes, meta))`
/// when fetched or `None` when the object is a genuine 404/410 miss.
type PresignedFetch = (String, Option<(Vec<u8>, CachedPath)>);

/// Download one presigned NAR with bounded retries. `Ok((path, Some((bytes,
/// cp))))` fetched it; `Ok((path, None))` is a genuine 404/410 miss (the row
/// claims the NAR but the bucket lost it - the `InputsUnavailable` self-heal
/// demotes and rebuilds it, #410). Transport errors and retryable statuses are
/// retried with exponential backoff before surfacing as a transient `Err`.
pub(crate) async fn download_one_presigned(
    http: &reqwest::Client,
    cp: CachedPath,
) -> Result<PresignedFetch> {
    let url = cp.url.clone().expect("by_url entries have a URL");
    let path = cp.path.clone();
    let mut backoff = PRESIGNED_RETRY_BASE;

    for attempt in 1..=PRESIGNED_DOWNLOAD_MAX_ATTEMPTS {
        let attempt_err = match http.get(&url).timeout(TRANSFER_TIMEOUT).send().await {
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
                } else {
                    let resp = resp
                        .error_for_status()
                        .with_context(|| format!("HTTP {url} returned non-2xx"))?;
                    let bytes = resp
                        .bytes()
                        .await
                        .with_context(|| format!("read body of {url}"))?
                        .to_vec();
                    return Ok((path, Some((bytes, cp))));
                }
            }
            Err(e) => anyhow::Error::new(e).context(format!("HTTP GET {url} (path {path})")),
        };

        if attempt < PRESIGNED_DOWNLOAD_MAX_ATTEMPTS {
            warn!(%path, attempt, error = %attempt_err, "presigned download failed; retrying");
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

/// How the closure walker treats `.drv` content seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClosureMode {
    /// Mine each fetched `.drv` for outputs **and** input_derivations +
    /// input_sources. Use when prefetching binary inputs: declared outputs of
    /// intermediate `.drv`s are typically cached and downstream builds will
    /// need them, so eager fetching is a useful optimisation.
    FollowOutputs,
    /// Skip output seeds; mine only input_derivations + input_sources. Use
    /// when fetching a build target's own `.drv`: its outputs are by
    /// definition not in the cache (we're about to build them), and asking
    /// the server for them would surface as a fatal `Uncached` classification
    /// even though the daemon only needs the input chain to accept the import.
    InputsOnly,
}

// ── InputPrefetcher ───────────────────────────────────────────────────────────

/// Drives the five-stage pipeline that ensures every input path a build needs
/// is present in the local nix store before the build is handed off.
///
/// Created by [`prefetch_inputs`] from a [`BuildTask`] + store + updater.
struct InputPrefetcher<'a> {
    store: &'a LocalNixStore,
    /// Derivation path of the build task (used for logging and cache queries).
    drv_path: String,
    /// Build ID (used for logging only).
    build_id: String,
    /// Live WS connection back to the server (used for `CacheQuery` /
    /// `NarRequest`). Requires `&mut` because sending advances the framing state.
    updater: &'a mut JobUpdater,
}

impl<'a> InputPrefetcher<'a> {
    fn new(store: &'a LocalNixStore, task: &'a BuildTask, updater: &'a mut JobUpdater) -> Self {
        Self {
            store,
            drv_path: task.drv_path.clone(),
            build_id: task.build_id.clone(),
            updater,
        }
    }

    /// Construct a prefetcher not tied to a `BuildTask` - used by
    /// [`ensure_path`] to substitute a single store path (and its closure)
    /// without a build context. `label` only feeds logging.
    fn for_path(store: &'a LocalNixStore, label: String, updater: &'a mut JobUpdater) -> Self {
        Self {
            store,
            drv_path: label.clone(),
            build_id: label,
            updater,
        }
    }

    /// Stage 0 - ensure the build's own `.drv` file is present locally so
    /// `enumerate_inputs` can read it.
    ///
    /// On a build worker that didn't perform the eval, the target drv is not
    /// on disk: eval ran on a different worker (or in-process on the server),
    /// pushed produced drvs to the cache via `push_drvs`, and dispatched a
    /// `BuildJob` carrying only `drv_path` strings. Without this stage,
    /// `enumerate_inputs` fails with `read .drv … No such file or directory`.
    ///
    /// The fetch must also pull the `.drv`'s reference chain - every
    /// transitive input_derivation `.drv` plus its input_sources - because
    /// `add_to_store_nar` rejects the build target's `.drv` if any reference
    /// declared in its `ValidPathInfo` is absent from the local store. We use
    /// [`ClosureMode::InputsOnly`] to skip output seeds: the build target's
    /// outputs are by construction not cached (we're about to produce them),
    /// and the daemon doesn't need them present to accept the `.drv` import.
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

        self.fetch_closure(vec![self.drv_path.to_owned()], ClosureMode::InputsOnly)
            .await?;

        if !tokio::fs::try_exists(&full_drv_path).await.unwrap_or(false) {
            return Err(anyhow::anyhow!(
                "build target drv {} still missing after fetch+import",
                full_drv_path
            ));
        }

        Ok(())
    }

    /// Stage 1 - collect every input store path declared by this derivation.
    ///
    /// Reads `input_sources` (plain paths) and the output paths of each
    /// `input_derivation` by parsing their `.drv` files.
    async fn enumerate_inputs(&self) -> Result<HashSet<String>> {
        let full_drv_path = nix_store_path(&self.drv_path);
        let drv_bytes = tokio::fs::read(&full_drv_path)
            .await
            .with_context(|| format!("read .drv {} for prefetch", full_drv_path))?;
        let drv = parse_drv(&drv_bytes)
            .with_context(|| format!("parse .drv {} for prefetch", full_drv_path))?;

        let mut wanted: HashSet<String> = HashSet::new();
        for src in &drv.input_sources {
            wanted.insert(src.clone());
        }
        for (input_drv_path, _outputs) in &drv.input_derivations {
            let input_full = nix_store_path(input_drv_path);
            match tokio::fs::read(&input_full).await {
                Ok(bytes) => match parse_drv(&bytes) {
                    Ok(input_drv) => {
                        for o in &input_drv.outputs {
                            if !o.path.is_empty() {
                                wanted.insert(o.path.clone());
                            }
                        }
                    }
                    Err(e) => {
                        warn!(drv = %input_full, error = %e, "cannot parse input .drv during prefetch");
                    }
                },
                Err(e) => {
                    debug!(drv = %input_full, error = %e, "input .drv not present locally; queuing for fetch");
                    // Queue the input .drv itself. Its outputs will be
                    // discovered after it lands in the local store and the
                    // closure walk processes its `references` (which include
                    // any input drvs of its own). Output paths the build
                    // ultimately needs are reached transitively via the same
                    // walk.
                    wanted.insert(input_drv_path.clone());
                }
            }
        }

        Ok(wanted)
    }

    /// Stage 2 - filter `wanted` down to paths absent from the local store.
    ///
    /// A `has_path` failure means we can't tell whether the daemon already
    /// holds a path - proceeding would either skip an actually-missing input
    /// (build fails late with "dependency does not exist") or re-import one
    /// the daemon already has (wasted work and confusing logs). Neither is
    /// acceptable, so we fail the build immediately.
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

    /// Stage 3 - ask the server which missing paths it can serve, then split
    /// them into two buckets: presigned-URL downloads and `NarRequest` transfers.
    ///
    /// Any path the server reports as `Uncached` is a **hard failure**: the
    /// path is known to be absent from the worker's local store (checked in
    /// Stage 2) and builds run with `use_substitutes = false`, so the daemon
    /// will not be able to fetch it from any upstream. Continuing would
    /// eventually surface as a confusing `path '…' is not valid` error deep
    /// inside `add_to_store_nar` when a dependent path is imported. Failing
    /// here keeps the blame at the right layer.
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
        } = classify_cached_entries(cached_entries);

        if !uncached.is_empty() {
            error!(
                build_id = %self.build_id,
                drv = %self.drv_path,
                missing = uncached.len(),
                sample = ?uncached.iter().take(5).collect::<Vec<_>>(),
                "prefetch: server cannot serve required inputs (not in gradient cache)"
            );
            return Err(anyhow::Error::new(MissingInputs(uncached)));
        }

        Ok((by_url, by_request))
    }

    /// Stage 4a - fetch NARs from the server via `NarRequest` (local-mode cache).
    async fn fetch_by_request(
        &mut self,
        by_request: Vec<CachedPath>,
    ) -> Result<Vec<(String, Vec<u8>, CachedPath)>> {
        if by_request.is_empty() {
            return Ok(vec![]);
        }

        let paths: Vec<String> = by_request.iter().map(|c| c.path.clone()).collect();
        let bytes_by_path = self.updater.request_nars(paths).await?;

        let mut meta_by_path: HashMap<String, CachedPath> = by_request
            .into_iter()
            .map(|c| (c.path.clone(), c))
            .collect();

        let results = bytes_by_path
            .into_iter()
            .filter_map(|(path, bytes)| meta_by_path.remove(&path).map(|meta| (path, bytes, meta)))
            .collect();

        Ok(results)
    }

    /// Stage 4b - download NARs from presigned S3 URLs (S3-backed cache).
    ///
    /// A failed download is fatal: silently dropping it would let the build
    /// proceed with a missing input or output and surface later as an opaque
    /// "No such file or directory" when we try to NAR-pack the absent path.
    async fn download_by_url(
        &self,
        by_url: Vec<CachedPath>,
    ) -> Result<Vec<(String, Vec<u8>, CachedPath)>> {
        if by_url.is_empty() {
            return Ok(vec![]);
        }

        let http = crate::http::client();

        // Bound concurrency: firing every download at once opens a TLS
        // connection per path, which is what tips a flaky object store into
        // `tls handshake eof`. Cap it at the same width as the import pipeline.
        let outcomes: Vec<Result<PresignedFetch>> =
            futures::stream::iter(by_url.into_iter().map(|cp| {
                let http = http.clone();
                async move { download_one_presigned(&http, cp).await }
            }))
            .buffer_unordered(PREFETCH_CONCURRENCY)
            .collect()
            .await;

        let mut results = Vec::new();
        let mut missing = Vec::new();
        for outcome in outcomes {
            let (path, fetched) = outcome.context("presigned NAR download failed")?;
            match fetched {
                Some((bytes, cp)) => results.push((path, bytes, cp)),
                None => missing.push(path),
            }
        }

        if !missing.is_empty() {
            return Err(anyhow::Error::new(MissingInputs(missing)));
        }

        Ok(results)
    }

    /// Stage 5 - import every downloaded NAR into the local nix-daemon in
    /// topological order: a path's `references` (from its `CachedPath.references`)
    /// that are also in the download set must finish importing before the
    /// path itself is imported. References already present in the local store
    /// impose no ordering constraint.
    ///
    /// Independent paths (those with no remaining unresolved deps) are imported
    /// in parallel up to [`PREFETCH_CONCURRENCY`]. Any import failure aborts
    /// the whole prefetch: proceeding with a partial closure would let the
    /// daemon fail later with a confusing "dependency does not exist" error
    /// instead of the real transport/metadata problem. In-flight imports are
    /// cancelled by dropping the `FuturesUnordered`.
    ///
    /// Returns the total number of imports attempted on success.
    async fn import_all(&self, results: Vec<(String, Vec<u8>, CachedPath)>) -> Result<usize> {
        let store = self.store;
        let total = results.len();
        if total == 0 {
            return Ok(0);
        }

        let download_paths: HashSet<String> = results.iter().map(|(p, _, _)| p.clone()).collect();

        let mut payload: HashMap<String, (Vec<u8>, CachedPath)> =
            results.into_iter().map(|(p, b, m)| (p, (b, m))).collect();

        // For each path, the subset of its references that are also in the
        // download set - i.e. the deps we must wait for. Refs already in the
        // local store (and thus not downloaded) aren't tracked here.
        let mut pending_deps: HashMap<String, HashSet<String>> = HashMap::new();
        // Reverse edges: when X imports successfully, promote each entry in
        // `dependents[X]` one step closer to ready.
        let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

        for (path, (_, meta)) in &payload {
            let refs = meta.references.clone().unwrap_or_default();
            let restricted: HashSet<String> = refs
                .into_iter()
                .filter(|r| r != path && download_paths.contains(r))
                .collect();
            for r in &restricted {
                dependents.entry(r.clone()).or_default().push(path.clone());
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
                let (bytes, meta) = payload
                    .remove(&path)
                    .expect("payload present for ready path");
                pending_deps.remove(&path);
                imports.push(async move {
                    let result = import_received_nar(store, &path, bytes, &meta)
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

            if let Some(kids) = dependents.remove(&path) {
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
            // Should not happen: nix store references are acyclic. If it does,
            // we've left paths unimported - log so the build failure is
            // diagnosable.
            warn!(
                remaining = pending_deps.len(),
                "topo import left paths unimported (cycle in references?)"
            );
        }

        Ok(completed)
    }

    /// Run the full prefetch pipeline.
    ///
    /// The drv's declared inputs (`input_sources` + `input_derivation` outputs)
    /// are only the first hop. Each of those paths has its own runtime
    /// `references` - the transitive closure - which must also be in the
    /// local store before the daemon can accept the import of a dependent.
    /// We therefore run `CacheQuery Pull` in a loop: on each iteration we
    /// inspect the references of everything we just fetched and queue any
    /// that are absent locally and haven't been queried yet. The loop ends
    /// when no new references surface.
    ///
    /// A safety cap bounds worst-case iterations so a pathological cycle or
    /// misbehaving upstream cannot loop forever.
    async fn run(&mut self) -> Result<()> {
        self.ensure_self_drv_present().await?;

        let wanted = self.enumerate_inputs().await?;
        if wanted.is_empty() {
            return Ok(());
        }

        let initial_missing = self.filter_missing(wanted).await?;
        if initial_missing.is_empty() {
            debug!(
                build_id = %self.build_id,
                "all inputs already in local store; no prefetch needed"
            );
            return Ok(());
        }
        self.fetch_closure(initial_missing, ClosureMode::FollowOutputs)
            .await
    }

    /// Fetch a seed set of paths plus their transitive closure into the
    /// local nix store. Used both by `run` (for build inputs) and by
    /// `execute_external_cached_task` (for the build outputs of
    /// upstream-substituted derivations the worker needs to repack into the
    /// gradient cache).
    ///
    /// `mode` controls how the walker treats `.drv` content: see [`ClosureMode`].
    async fn fetch_closure(
        &mut self,
        initial_missing: Vec<String>,
        mode: ClosureMode,
    ) -> Result<()> {
        const MAX_ITERATIONS: usize = 1024;

        debug!(
            build_id = %self.build_id,
            missing = initial_missing.len(),
            "prefetching missing paths from server cache (closure-expanding)"
        );

        let mut all_results: Vec<(String, Vec<u8>, CachedPath)> = Vec::new();
        // Every path we've already asked the server about (success or not),
        // so we don't re-query the same one across iterations.
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
            let mut batch = self.fetch_by_request(by_request).await?;
            batch.extend(self.download_by_url(by_url).await?);

            // Collect any references from this batch that we haven't yet
            // queried and that aren't already in the local store.
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

            // For every `.drv` we just fetched, parse it and harvest the
            // full set of closure-walk seeds - outputs (so downstream builds
            // find them), input_derivations (transitive `.drv` prerequisites),
            // and input_sources (plain files the daemon validates as
            // references when accepting the `.drv` NAR). Relying on
            // `cached_path.references` alone is unsafe: the eval worker
            // silently stores `NULL` when its own metadata query fails.
            for (path, bytes, meta) in &batch {
                if !path.ends_with(".drv") {
                    continue;
                }
                let compression = meta
                    .url
                    .as_deref()
                    .map(detect_compression)
                    .unwrap_or(Compression::Zstd);
                for seed in
                    drv_closure_seeds_from_compressed_nar(bytes, compression, path, mode).await
                {
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
                        // Already in the local store - nothing to do; still
                        // record it as queried so we don't revisit.
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

        let imported = self.import_all(all_results).await?;
        debug!(build_id = %self.build_id, imported, "prefetch complete");

        Ok(())
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Ensure every input path the daemon will need to build `task` is present
/// in the local nix store. Asks the server which missing paths it can serve
/// via `CacheQuery { mode: Pull }`, then for each cached path either:
///
/// - downloads from a presigned URL (S3-backed cache) and imports, or
/// - sends `NarRequest` and receives chunked `NarPush` frames over the WS
///   (local-mode cache), then imports.
///
/// Imports run concurrently (capped at [`PREFETCH_CONCURRENCY`]) since each
/// streams an `AddToStoreNar` into the daemon. A failed import aborts the
/// whole prefetch, so a surviving build always has a complete input closure.
///
/// On failure the error is mirrored onto the owning evaluation via
/// `EvalMessage` so operators see infrastructure problems (unreachable
/// upstream, bad narinfo metadata, …) on the evaluation page instead of
/// having to dig into per-build logs.
pub async fn prefetch_inputs(
    store: &LocalNixStore,
    task: &BuildTask,
    updater: &mut JobUpdater,
) -> Result<()> {
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

/// Ensure a single store path (plus its transitive runtime closure) is present
/// in the local nix store, substituting it from the gradient cache when absent.
///
/// Used before evaluating a `FlakeSource::Cached` flake: the fetch ran on a
/// different worker, so the archived source store path is only in the binary
/// cache, not this worker's local store. `nix` won't substitute a `path:` flake
/// ref from a cache, so we pull it in ourselves first. Reuses the same
/// closure-expanding `CacheQuery Pull → download → import` pipeline as
/// [`prefetch_inputs`].
pub async fn ensure_path(
    store: &LocalNixStore,
    path: &str,
    updater: &mut JobUpdater,
) -> Result<()> {
    if store.has_path(path).await? {
        return Ok(());
    }
    InputPrefetcher::for_path(store, path.to_owned(), updater)
        .fetch_closure(vec![path.to_owned()], ClosureMode::FollowOutputs)
        .await
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Result of splitting a `CacheQuery Pull` response into its three categories.
#[derive(Debug, Default)]
struct Classified {
    /// Cached paths the server will serve via a presigned HTTP URL.
    by_url: Vec<CachedPath>,
    /// Cached paths the server will serve via `NarRequest` over the WebSocket.
    by_request: Vec<CachedPath>,
    /// Paths the server reports it does **not** have. These are fatal during
    /// prefetch (see [`InputPrefetcher::query_and_split`] for why).
    uncached: Vec<String>,
}

/// Split a `CacheQuery Pull` response into URL-downloadable, WS-requestable,
/// and uncached buckets. Pure helper, kept out of [`InputPrefetcher`] so the
/// classification is unit-testable without a live WebSocket.
fn classify_cached_entries(entries: Vec<CachedPath>) -> Classified {
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
        let out = classify_cached_entries(vec![
            cached("/nix/store/aaaa-by-url", Some("https://s3.example/x")),
            cached("/nix/store/bbbb-by-ws", None),
        ]);
        assert_eq!(out.by_url.len(), 1);
        assert_eq!(out.by_request.len(), 1);
        assert!(out.uncached.is_empty());
        assert_eq!(out.by_url[0].path, "/nix/store/aaaa-by-url");
        assert_eq!(out.by_request[0].path, "/nix/store/bbbb-by-ws");
    }

    #[test]
    fn classify_collects_uncached_separately() {
        // This is the regression the Stage-3 hard-fail guards against: if the
        // server reports a required input as uncached, we must surface it so
        // we don't silently hand the build a broken closure.
        let out = classify_cached_entries(vec![
            cached("/nix/store/aaaa-ok", None),
            uncached("/nix/store/xxxx-missing-upstream"),
            uncached("/nix/store/yyyy-also-missing"),
        ]);
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
        let out = classify_cached_entries(vec![]);
        assert!(out.by_url.is_empty());
        assert!(out.by_request.is_empty());
        assert!(out.uncached.is_empty());
    }

    #[test]
    fn missing_inputs_message_and_downcast() {
        let paths = vec![
            "/nix/store/g9y0fvqh2c991vjprgz9mvdm0zj7ggij-python3-static".to_string(),
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-other".to_string(),
        ];
        let err = anyhow::Error::new(MissingInputs(paths.clone()));
        let msg = format!("{err}");
        assert!(msg.contains("2 required input path(s)"), "msg: {msg}");
        assert!(msg.contains("python3-static"), "msg: {msg}");

        let recovered = err
            .downcast_ref::<MissingInputs>()
            .expect("MissingInputs survives anyhow boxing");
        assert_eq!(recovered.0, paths);
    }

    #[test]
    fn corrupt_cached_nar_survives_context_wrapping() {
        let path = "/nix/store/97v143jdvzv5rlvdi5jcjvy88czayn43-ruby3.4-delayed_job".to_string();
        let err = anyhow::Error::new(CorruptCachedNar(path.clone()))
            .context("import into local store")
            .context("prefetch import failed");

        let recovered = err
            .chain()
            .find_map(|s| s.downcast_ref::<CorruptCachedNar>())
            .expect("CorruptCachedNar survives anyhow context wrapping");
        assert_eq!(recovered.0, path);
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
        // Genuine misses and terminal client errors must NOT retry: 404/410 are
        // handled as missing inputs, 403/400 are terminal.
        for s in [400, 403, 404, 410] {
            assert!(
                !presigned_status_is_retryable(s),
                "status {s} must not retry"
            );
        }
    }
}
