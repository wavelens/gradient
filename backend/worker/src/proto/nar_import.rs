/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Stream a server-supplied NAR straight into the local nix-daemon.
//!
//! Receives the (still zstd-compressed) NAR bytes plus the cache metadata
//! that came back in `CacheStatus`, decompresses in-memory, constructs a
//! [`ValidPathInfo`], and calls harmonia's `add_to_store_nar` over the
//! daemon socket. No on-disk staging, no `nix copy` subprocess, no
//! signature/key configuration on the worker — the WS transport itself is
//! authenticated, so we pass `dont_check_sigs: true`.
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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read as _;
use std::pin::pin;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt as _};
use gradient_core::db::parse_drv;
use gradient_core::executer::path_utils::nix_store_path;
use gradient_core::types::CachedPathInfo;
use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
use harmonia_store_core::signature::Signature;
use harmonia_store_core::store_path::{StoreDir, StorePath};
use harmonia_store_remote::DaemonStore as _;
use harmonia_utils_hash::Hash;
use harmonia_utils_hash::fmt::Any;
use proto::messages::{BuildTask, CachedPath, QueryMode};
use sha2::{Digest as _, Sha256};
use tracing::{debug, error, info, warn};

use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;

/// Time budget for a single HTTP NAR download (presigned-URL path). Keep in
/// the same ballpark as the WS `NarRequest` waiter timeout so the slowest
/// import path is bounded the same way.
const HTTP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// How many missing inputs to download + import in parallel before invoking
/// the build. Conservative — each one streams a NAR into the local daemon
/// and we don't want to swamp the AddToStoreNar queue.
const PREFETCH_CONCURRENCY: usize = 8;

// ── InputPrefetcher ───────────────────────────────────────────────────────────

/// Drives the five-stage pipeline that ensures every input path a build needs
/// is present in the local nix store before the build is handed off.
///
/// Created by [`prefetch_inputs`] from a [`BuildTask`] + store + updater.
struct InputPrefetcher<'a> {
    store: &'a LocalNixStore,
    /// Derivation path of the build task (used for logging and cache queries).
    drv_path: &'a str,
    /// Build ID (used for logging only).
    build_id: &'a str,
    /// Live WS connection back to the server (used for `CacheQuery` /
    /// `NarRequest`). Requires `&mut` because sending advances the framing state.
    updater: &'a mut JobUpdater,
}

impl<'a> InputPrefetcher<'a> {
    fn new(store: &'a LocalNixStore, task: &'a BuildTask, updater: &'a mut JobUpdater) -> Self {
        Self {
            store,
            drv_path: &task.drv_path,
            build_id: &task.build_id,
            updater,
        }
    }

    /// Stage 1 — collect every input store path declared by this derivation.
    ///
    /// Reads `input_sources` (plain paths) and the output paths of each
    /// `input_derivation` by parsing their `.drv` files.
    async fn enumerate_inputs(&self) -> Result<HashSet<String>> {
        let full_drv_path = nix_store_path(self.drv_path);
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
                    debug!(drv = %input_full, error = %e, "input .drv not present locally; will need it from cache");
                }
            }
        }

        Ok(wanted)
    }

    /// Stage 2 — filter `wanted` down to paths absent from the local store.
    ///
    /// A `has_path` failure means we can't tell whether the daemon already
    /// holds a path — proceeding would either skip an actually-missing input
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
                    return Err(anyhow::anyhow!(
                        "store.has_path failed for {}: {}",
                        p,
                        e
                    ));
                }
            }
        }
        Ok(missing)
    }

    /// Stage 3 — ask the server which missing paths it can serve, then split
    /// them into two buckets: presigned-URL downloads and `NarRequest` transfers.
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

        let mut by_url: Vec<CachedPath> = Vec::new();
        let mut by_request: Vec<CachedPath> = Vec::new();
        for cp in cached_entries {
            let has_url = match cp.as_info() {
                CachedPathInfo::Uncached { .. } => continue, // server can't serve this either
                CachedPathInfo::Cached { download_url, .. } => download_url.is_some(),
            };
            if has_url {
                by_url.push(cp);
            } else {
                by_request.push(cp);
            }
        }

        Ok((by_url, by_request))
    }

    /// Stage 4a — fetch NARs from the server via `NarRequest` (local-mode cache).
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

    /// Stage 4b — download NARs from presigned S3 URLs (S3-backed cache).
    async fn download_by_url(&self, by_url: Vec<CachedPath>) -> Vec<(String, Vec<u8>, CachedPath)> {
        if by_url.is_empty() {
            return vec![];
        }

        let http = match reqwest::Client::builder()
            .timeout(HTTP_DOWNLOAD_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to build reqwest client for presigned downloads");
                return vec![];
            }
        };

        let mut futs = by_url
            .into_iter()
            .map(|cp| {
                let http = http.clone();
                async move {
                    let url = cp.url.clone().expect("by_url entries have a URL");
                    let resp = http
                        .get(&url)
                        .send()
                        .await
                        .with_context(|| format!("HTTP GET {} (path {})", url, cp.path))?
                        .error_for_status()
                        .with_context(|| format!("HTTP {} returned non-2xx", url))?;
                    let bytes = resp
                        .bytes()
                        .await
                        .with_context(|| format!("read body of {}", url))?
                        .to_vec();
                    Ok::<_, anyhow::Error>((cp.path.clone(), bytes, cp))
                }
            })
            .collect::<FuturesUnordered<_>>();

        let mut results = Vec::new();
        while let Some(r) = futs.next().await {
            match r {
                Ok(triple) => results.push(triple),
                Err(e) => {
                    warn!(error = %e, "presigned NAR download failed; build may need to refetch")
                }
            }
        }
        results
    }

    /// Stage 5 — import every downloaded NAR into the local nix-daemon in
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

        let download_paths: HashSet<String> =
            results.iter().map(|(p, _, _)| p.clone()).collect();

        let mut payload: HashMap<String, (Vec<u8>, CachedPath)> = results
            .into_iter()
            .map(|(p, b, m)| (p, (b, m)))
            .collect();

        // For each path, the subset of its references that are also in the
        // download set — i.e. the deps we must wait for. Refs already in the
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
                let (bytes, meta) = payload.remove(&path).expect("payload present for ready path");
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
                error!(path = %path, error = %e, "dep NAR import failed; aborting prefetch");
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
            // we've left paths unimported — log so the build failure is
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
    /// `references` — the transitive closure — which must also be in the
    /// local store before the daemon can accept the import of a dependent.
    /// We therefore run `CacheQuery Pull` in a loop: on each iteration we
    /// inspect the references of everything we just fetched and queue any
    /// that are absent locally and haven't been queried yet. The loop ends
    /// when no new references surface.
    ///
    /// A safety cap bounds worst-case iterations so a pathological cycle or
    /// misbehaving upstream cannot loop forever.
    async fn run(&mut self) -> Result<()> {
        const MAX_ITERATIONS: usize = 1024;

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

        info!(
            build_id = %self.build_id,
            missing = initial_missing.len(),
            "prefetching missing inputs from server cache (closure-expanding)"
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
            batch.extend(self.download_by_url(by_url).await);

            // Collect any references from this batch that we haven't yet
            // queried and that aren't already in the local store.
            let refs: HashSet<String> = batch
                .iter()
                .flat_map(|(_, _, meta)| meta.references.clone().unwrap_or_default())
                .filter(|r| !queried.contains(r))
                .collect();

            all_results.extend(batch);

            let mut next_batch = Vec::with_capacity(refs.len());
            for r in refs {
                match self.store.has_path(&r).await {
                    Ok(true) => {
                        // Already in the local store — nothing to do; still
                        // record it as queried so we don't revisit.
                        queried.insert(r);
                    }
                    Ok(false) => {
                        queried.insert(r.clone());
                        next_batch.push(r);
                    }
                    Err(e) => {
                        error!(
                            path = %r,
                            error = %e,
                            "store.has_path failed during closure expansion; aborting build"
                        );
                        return Err(anyhow::anyhow!(
                            "store.has_path failed for {}: {}",
                            r,
                            e
                        ));
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
        info!(build_id = %self.build_id, imported, "prefetch complete");

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
/// streams an `AddToStoreNar` into the daemon. Errors per-path are warnings
/// — the build itself will fail loudly if a critical input is still missing
/// when we hand off to `build_derivation`.
pub async fn prefetch_inputs(
    store: &LocalNixStore,
    task: &BuildTask,
    updater: &mut JobUpdater,
) -> Result<()> {
    InputPrefetcher::new(store, task, updater).run().await
}

// ── NarImporter ───────────────────────────────────────────────────────────────

/// Decompresses a single server-supplied NAR, verifies its integrity, builds
/// the [`ValidPathInfo`] the daemon expects, and streams it via
/// `add_to_store_nar`. Created by [`import_received_nar`].
struct NarImporter<'a> {
    store: &'a LocalNixStore,
    store_path: &'a str,
    meta: &'a CachedPath,
}

impl<'a> NarImporter<'a> {
    fn new(store: &'a LocalNixStore, store_path: &'a str, meta: &'a CachedPath) -> Self {
        Self {
            store,
            store_path,
            meta,
        }
    }

    fn decompress(&self, compressed: &[u8]) -> Result<Vec<u8>> {
        // Compression is inferred from the `URL:` field in the narinfo that
        // was rewritten into `meta.url`. When the bytes came in via
        // `NarRequest` (WebSocket, no URL), we default to zstd — that's
        // the only format our own cache ever produces.
        let kind = self
            .meta
            .url
            .as_deref()
            .map(detect_compression)
            .unwrap_or(Compression::Zstd);
        decompress(compressed, kind)
            .with_context(|| format!("{kind:?} decompress failed for {}", self.store_path))
    }

    fn verify_size(&self, decompressed: &[u8]) -> Result<()> {
        if let Some(expected) = self.meta.nar_size
            && decompressed.len() as u64 != expected
        {
            anyhow::bail!(
                "NAR size mismatch for {}: expected {}, got {}",
                self.store_path,
                expected,
                decompressed.len()
            );
        }
        Ok(())
    }

    fn verify_hash(&self, decompressed: &[u8]) -> Result<()> {
        if let Some(claimed_nar_hash) = self.meta.nar_hash.as_deref() {
            let actual_nar_hash: [u8; 32] = Sha256::digest(decompressed).into();
            let claimed = parse_nar_hash_to_bytes(claimed_nar_hash)
                .with_context(|| format!("invalid nar_hash for {}", self.store_path))?;

            if actual_nar_hash != claimed {
                anyhow::bail!(
                    "NAR hash mismatch for {}: server said {}, computed sha256:<...>",
                    self.store_path,
                    claimed_nar_hash
                );
            }
        }
        Ok(())
    }

    fn build_path_info(&self, nar_size: u64) -> Result<ValidPathInfo> {
        let path_base = self
            .store_path
            .strip_prefix("/nix/store/")
            .unwrap_or(self.store_path);

        let path = StorePath::from_base_path(path_base)
            .map_err(|e| anyhow::anyhow!("invalid store path {}: {}", self.store_path, e))?;

        let info = build_unkeyed_path_info(self.store_path, self.meta, nar_size)?;
        Ok(ValidPathInfo { path, info })
    }

    async fn stream_to_daemon(
        &self,
        decompressed: &[u8],
        valid_info: &ValidPathInfo,
    ) -> Result<()> {
        let mut guard = self
            .store
            .pool()
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("acquire local store for import: {}", e))?;

        let logs = guard.client().add_to_store_nar(
            valid_info,
            decompressed,
            false, // repair
            true,  // dont_check_sigs — we trust the authenticated WS transport
        );

        let mut logs = pin!(logs);
        while let Some(_msg) = logs.next().await {
            // Daemon log frames during import are noisy and not user-facing — drop them.
        }

        logs.await.map_err(|e| {
            anyhow::anyhow!("daemon add_to_store_nar({}) failed: {}", self.store_path, e)
        })
    }

    async fn import(&self, compressed_nar: Vec<u8>) -> Result<()> {
        let decompressed = self.decompress(&compressed_nar)?;
        self.verify_size(&decompressed)?;
        self.verify_hash(&decompressed)?;
        let valid_info = self.build_path_info(decompressed.len() as u64)?;
        self.stream_to_daemon(&decompressed, &valid_info).await?;
        debug!(%self.store_path, bytes = compressed_nar.len(), "imported NAR into local store");
        Ok(())
    }
}

// ── Public import entry point ─────────────────────────────────────────────────

/// Decompress + import a single NAR delivered via `NarPush` (or downloaded
/// from a presigned URL) into the worker's local nix-daemon.
pub async fn import_received_nar(
    store: &LocalNixStore,
    store_path: &str,
    compressed_nar: Vec<u8>,
    meta: &CachedPath,
) -> Result<()> {
    NarImporter::new(store, store_path, meta)
        .import(compressed_nar)
        .await
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Compression format for a NAR as declared by the cache it came from.
/// Identified by filename extension on the `URL:` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Zstd,
    Xz,
    Bzip2,
}

/// Infer a NAR's compression format from the URL extension. Unknown or
/// missing extension → `Zstd`, since our own cache always produces zstd;
/// this keeps the `NarRequest` / S3 path correct while letting upstream
/// URLs like `.nar.xz` dispatch accordingly.
fn detect_compression(url: &str) -> Compression {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".nar.xz") || lower.ends_with(".xz") {
        Compression::Xz
    } else if lower.ends_with(".nar.bz2") || lower.ends_with(".bz2") {
        Compression::Bzip2
    } else if lower.ends_with(".nar.zst") || lower.ends_with(".zst") {
        Compression::Zstd
    } else if lower.ends_with(".nar") {
        Compression::None
    } else {
        Compression::Zstd
    }
}

/// Decompress a NAR payload per its compression format. Synchronous; NAR
/// payloads are bounded by `nar_size` from the path info, so memory
/// pressure is predictable.
fn decompress(compressed: &[u8], kind: Compression) -> Result<Vec<u8>> {
    match kind {
        Compression::None => Ok(compressed.to_vec()),
        Compression::Zstd => decompress_zstd(compressed),
        Compression::Xz => decompress_xz(compressed),
        Compression::Bzip2 => decompress_bzip2(compressed),
    }
}

fn decompress_zstd(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(compressed))
        .context("init zstd decoder")?;

    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder.read_to_end(&mut out).context("read zstd stream")?;

    Ok(out)
}

fn decompress_xz(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = xz2::read::XzDecoder::new(std::io::Cursor::new(compressed));
    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder.read_to_end(&mut out).context("read xz stream")?;
    Ok(out)
}

fn decompress_bzip2(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = bzip2::read::BzDecoder::new(std::io::Cursor::new(compressed));
    let mut out = Vec::with_capacity(compressed.len() * 4);
    decoder
        .read_to_end(&mut out)
        .context("read bzip2 stream")?;
    Ok(out)
}

/// Parse a `sha256:<...>` (or `sha256-<base64>` SRI) hash into the raw 32-byte
/// digest expected for byte-wise comparison against `Sha256::digest`.
fn parse_nar_hash_to_bytes(s: &str) -> Result<[u8; 32]> {
    let hash_any = s
        .parse::<Any<Hash>>()
        .map_err(|e| anyhow::anyhow!("parse hash {}: {}", s, e))?;

    let hash: Hash = hash_any.into_hash();
    let bytes = hash.digest_bytes();
    if bytes.len() != 32 {
        anyhow::bail!("expected 32-byte SHA-256 digest, got {}", bytes.len());
    }

    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    Ok(out)
}

/// Build the `UnkeyedValidPathInfo` for `add_to_store_nar` from the cache
/// metadata. Falls back to a default `ca = None` / `deriver = None` /
/// `signatures = {}` when the server didn't supply them.
fn build_unkeyed_path_info(
    store_path: &str,
    meta: &CachedPath,
    nar_size: u64,
) -> Result<UnkeyedValidPathInfo> {
    let nar_hash_str = meta
        .nar_hash
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cache metadata missing nar_hash for {}", store_path))?;

    let hash_any = nar_hash_str
        .parse::<Any<Hash>>()
        .map_err(|e| anyhow::anyhow!("parse nar_hash '{}': {}", nar_hash_str, e))?;

    let nar_hash = hash_any
        .into_hash()
        .try_into()
        .map_err(|e| anyhow::anyhow!("convert nar_hash '{}' to NarHash: {}", nar_hash_str, e))?;

    let mut references: BTreeSet<StorePath> = BTreeSet::new();
    if let Some(refs) = meta.references.as_ref() {
        for r in refs {
            let base = r.strip_prefix("/nix/store/").unwrap_or(r);
            match StorePath::from_base_path(base) {
                Ok(sp) => {
                    references.insert(sp);
                }
                Err(e) => {
                    warn!(reference = %r, error = %e, "skipping invalid reference");
                }
            }
        }
    }

    let deriver = meta.deriver.as_ref().and_then(|d| {
        let base = d.strip_prefix("/nix/store/").unwrap_or(d);
        match StorePath::from_base_path(base) {
            Ok(sp) => Some(sp),
            Err(e) => {
                warn!(deriver = %d, error = %e, "skipping invalid deriver");
                None
            }
        }
    });

    let mut signatures: BTreeSet<Signature> = BTreeSet::new();
    if let Some(sigs) = meta.signatures.as_ref() {
        for s in sigs {
            match s.parse::<Signature>() {
                Ok(sig) => {
                    signatures.insert(sig);
                }
                Err(e) => {
                    warn!(signature = %s, error = %e, "skipping unparseable signature");
                }
            }
        }
    }

    let ca = meta.ca.as_ref().and_then(|c| match c.parse() {
        Ok(parsed) => Some(parsed),
        Err(_) => {
            warn!(ca = %c, "skipping unparseable content-address");
            None
        }
    });

    Ok(UnkeyedValidPathInfo {
        deriver,
        nar_hash,
        references,
        registration_time: None,
        nar_size,
        ultimate: false,
        signatures,
        ca,
        store_dir: StoreDir::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_compression_from_url_extensions() {
        assert_eq!(
            detect_compression("https://cache.nixos.org/nar/abc.nar.xz"),
            Compression::Xz
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar.bz2"),
            Compression::Bzip2
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar.zst"),
            Compression::Zstd
        );
        assert_eq!(
            detect_compression("https://cache.example/nar/abc.nar"),
            Compression::None
        );
        // S3 presigned URLs carry a query string — must not confuse the matcher.
        assert_eq!(
            detect_compression("https://s3.example/abc.nar.xz?sig=XYZ&exp=1"),
            Compression::Xz
        );
        // Unknown / no extension defaults to zstd (our own cache).
        assert_eq!(
            detect_compression("https://example/some/opaque"),
            Compression::Zstd
        );
    }

    #[test]
    fn decompress_none_passthrough() {
        let raw = b"raw NAR bytes".to_vec();
        let out = decompress(&raw, Compression::None).unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn decompress_roundtrip_xz() {
        use std::io::Write;
        let payload = b"hello gradient xz world";
        let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let out = decompress(&compressed, Compression::Xz).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn decompress_roundtrip_bzip2() {
        use std::io::Write;
        let payload = b"hello gradient bzip2 world";
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder.write_all(payload).unwrap();
        let compressed = encoder.finish().unwrap();
        let out = decompress(&compressed, Compression::Bzip2).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn parse_sha256_nix32_roundtrip() {
        // SHA-256 of the empty string in nix32 form.
        let nix32 = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
        let bytes = parse_nar_hash_to_bytes(nix32).unwrap();
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn build_unkeyed_minimal_meta() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(123),
            url: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 123).unwrap();
        assert_eq!(info.nar_size, 123);
        assert!(info.references.is_empty());
        assert!(info.signatures.is_empty());
        assert!(info.deriver.is_none());
        assert!(info.ca.is_none());
        assert!(!info.ultimate);
    }

    #[test]
    fn build_unkeyed_collects_references_and_signatures() {
        // Nix store path hashes are exactly 32 chars in nix32 (160 bits).
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            references: Some(vec![
                "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-y".into(),
                "/nix/store/cccccccccccccccccccccccccccccccc-z".into(),
            ]),
            signatures: Some(vec![
                // Both malformed (Ed25519 sigs are 88 base64 chars); should be
                // dropped without aborting the path-info construction.
                "cache.example.com-1:tooShort".into(),
                "garbage-no-colon".into(),
            ]),
            deriver: Some("/nix/store/dddddddddddddddddddddddddddddddd-x.drv".into()),
            ca: None,
        };
        let info = build_unkeyed_path_info(&meta.path, &meta, 0).unwrap();
        assert_eq!(info.references.len(), 2);
        assert!(info.deriver.is_some());
        // Both signatures were malformed and should have been skipped.
        assert_eq!(info.signatures.len(), 0);
    }

    #[test]
    fn missing_nar_hash_is_an_error() {
        let meta = CachedPath {
            path: "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x".into(),
            cached: true,
            file_size: None,
            nar_size: Some(0),
            url: None,
            nar_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        };
        assert!(build_unkeyed_path_info(&meta.path, &meta, 0).is_err());
    }
}
