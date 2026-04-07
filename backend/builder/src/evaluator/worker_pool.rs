/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Parent-side pool of [`super::worker`] subprocesses and the
//! [`DerivationResolver`] impl that drives them.
//!
//! See `worker.rs` for the protocol and the subprocess entry point. The pool
//! itself is modelled on `gradient_core::pool::NixStorePool`: a semaphore
//! gates the maximum number of in-flight workers, and idle workers are
//! returned to a free list on drop. Workers are spawned lazily on demand.

use anyhow::{Context, Result};
use async_trait::async_trait;
use entity::server::Architecture;
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_core::consts::FLAKE_START;
use gradient_core::derivation::{Derivation, parse_drv};
use gradient_core::evaluator::{DerivationResolver, ResolvedDerivation};
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

use super::worker::{EvalRequest, EvalResponse, ResolvedItem};

/// Strips `/nix/store/` and returns just the hash-name component (mirrors the
/// helper in [`super::resolver`]).
fn nix_store_path(hash_name: &str) -> String {
    if hash_name.starts_with('/') {
        hash_name.to_string()
    } else {
        format!("/nix/store/{}", hash_name)
    }
}

/// Handle to a single live eval-worker subprocess.
///
/// Owns the child plus its piped stdin/stdout. Each request writes one JSON
/// line to stdin and reads one JSON line back from stdout — the protocol is
/// strictly request/response so a single buffer is sufficient.
pub struct EvalWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    line: String,
}

impl EvalWorker {
    /// Spawn a new worker by re-execing the current binary with `--eval-worker`.
    async fn spawn() -> Result<Self> {
        let exe = std::env::current_exe().context("locating current executable")?;
        let mut child = Command::new(exe)
            .arg("--eval-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawning eval worker subprocess")?;
        let stdin = child.stdin.take().context("worker stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("worker stdout missing")?);
        Ok(Self {
            child,
            stdin,
            stdout,
            line: String::new(),
        })
    }

    /// Send one request and read one response. Errors here mean the worker is
    /// no longer usable (the caller marks it dead so it gets discarded
    /// instead of being returned to the pool).
    async fn request(&mut self, req: &EvalRequest) -> Result<EvalResponse> {
        let mut bytes = serde_json::to_vec(req).context("serializing request")?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .await
            .context("writing to eval worker stdin")?;
        self.stdin
            .flush()
            .await
            .context("flushing eval worker stdin")?;

        self.line.clear();
        let n = self
            .stdout
            .read_line(&mut self.line)
            .await
            .context("reading eval worker response")?;
        if n == 0 {
            anyhow::bail!("eval worker closed pipe");
        }
        serde_json::from_str(self.line.trim_end()).context("parsing eval worker response")
    }

    async fn list(&mut self, repository: String, wildcards: Vec<String>) -> Result<Vec<String>> {
        match self
            .request(&EvalRequest::List {
                repository,
                wildcards,
            })
            .await?
        {
            EvalResponse::ListOk { attrs } => Ok(attrs),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to List"),
        }
    }

    async fn resolve(
        &mut self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<Vec<ResolvedItem>> {
        match self
            .request(&EvalRequest::Resolve { repository, attrs })
            .await?
        {
            EvalResponse::ResolveOk { items } => Ok(items),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Resolve"),
        }
    }

    async fn attr_names(&mut self, repository: String, path: String) -> Result<Vec<String>> {
        match self
            .request(&EvalRequest::AttrNames { repository, path })
            .await?
        {
            EvalResponse::AttrNamesOk { keys } => Ok(keys),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to AttrNames"),
        }
    }
}

impl std::fmt::Debug for EvalWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalWorker")
            .field("pid", &self.child.id())
            .finish()
    }
}

/// Pool of [`EvalWorker`]s.
///
/// Workers are created lazily on `acquire`. Idle workers are reused; broken
/// ones are discarded so the next `acquire` spawns a fresh replacement.
#[derive(Debug)]
pub struct EvalWorkerPool {
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    semaphore: Arc<Semaphore>,
    max: usize,
}

impl EvalWorkerPool {
    pub fn new(max: usize) -> Self {
        let max = max.max(1);
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    pub fn max(&self) -> usize {
        self.max
    }

    /// Acquire a worker, blocking until one is available. Reuses an idle
    /// worker if any, otherwise spawns a fresh subprocess.
    pub async fn acquire(&self) -> Result<PooledEvalWorker> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("EvalWorkerPool semaphore closed"))?;

        let worker = self.idle.lock().unwrap().pop();
        let worker = match worker {
            Some(w) => w,
            None => EvalWorker::spawn()
                .await
                .context("spawning fresh eval worker")?,
        };

        Ok(PooledEvalWorker {
            worker: Some(worker),
            idle: Arc::clone(&self.idle),
            healthy: true,
            _permit: permit,
        })
    }
}

/// RAII handle returned by [`EvalWorkerPool::acquire`].
///
/// Dereferences to `&mut EvalWorker`. On drop, returns the worker to the pool
/// if [`PooledEvalWorker::healthy`] is `true`, otherwise discards it (the
/// child is killed by `kill_on_drop`).
pub struct PooledEvalWorker {
    worker: Option<EvalWorker>,
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    healthy: bool,
    _permit: OwnedSemaphorePermit,
}

impl PooledEvalWorker {
    /// Mark this worker as broken so it won't be returned to the pool.
    pub fn mark_dead(&mut self) {
        self.healthy = false;
    }
}

impl Deref for PooledEvalWorker {
    type Target = EvalWorker;
    fn deref(&self) -> &Self::Target {
        self.worker.as_ref().unwrap()
    }
}

impl DerefMut for PooledEvalWorker {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.worker.as_mut().unwrap()
    }
}

impl Drop for PooledEvalWorker {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            if self.healthy
                && let Ok(mut idle) = self.idle.lock()
            {
                idle.push(worker);
                return;
            }
            // Unhealthy or poisoned mutex: drop the worker. `kill_on_drop`
            // ensures the child does not linger.
            debug!("discarding eval worker (unhealthy or pool poisoned)");
            drop(worker);
        }
    }
}

/// `DerivationResolver` impl that drives an [`EvalWorkerPool`].
///
/// `list_flake_derivations` and `resolve_derivation_paths` are dispatched to
/// the pool. `get_derivation` and `get_features` parse `.drv` files directly
/// from disk and don't need the embedded evaluator at all.
#[derive(Debug)]
pub struct WorkerPoolResolver {
    pool: Arc<EvalWorkerPool>,
}

impl WorkerPoolResolver {
    pub fn new(workers: usize) -> Self {
        Self {
            pool: Arc::new(EvalWorkerPool::new(workers)),
        }
    }

    /// Splits a single wildcard into multiple, more concrete wildcards so the
    /// pool can dispatch them in parallel:
    ///
    /// - First-segment wildcards are matched against [`FLAKE_START`] (e.g. `*.*`
    ///   → `checks.*`, `packages.*`, …).
    /// - Second-segment wildcards expand by querying the systems present under
    ///   each prefix (e.g. `packages.*.*` → `packages.x86_64-linux.*`,
    ///   `packages.aarch64-linux.*`, …).
    ///
    /// On any failure during system discovery the input wildcard is kept
    /// unchanged so the worker still produces a correct (but less parallel)
    /// result.
    async fn expand_wildcards_for_pool(
        &self,
        repository: &str,
        wildcards: Vec<String>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for w in wildcards {
            let segs = split_attr_path(&w);
            if segs.is_empty() {
                out.push(w);
                continue;
            }

            // Stage 1: expand first-segment wildcards against FLAKE_START.
            let stage1: Vec<Vec<String>> = if segs[0].contains('*') {
                let prefixes = match_pattern(&segs[0], FLAKE_START.iter().copied());
                if prefixes.is_empty() {
                    vec![segs.clone()]
                } else {
                    prefixes
                        .into_iter()
                        .map(|p| {
                            let mut v = vec![p];
                            v.extend_from_slice(&segs[1..]);
                            v
                        })
                        .collect()
                }
            } else {
                vec![segs.clone()]
            };

            // Stage 2: for each first-stage fragment of shape <prefix>.*.<rest>,
            // discover the systems under <prefix> and fan out one wildcard per
            // matching system.
            for frag in stage1 {
                if frag.len() >= 3 && frag[1].contains('*') && !frag[0].contains('*') {
                    let prefix = frag[0].clone();
                    match self.fetch_attr_names(repository, &prefix).await {
                        Ok(systems) => {
                            let matched = match_pattern(&frag[1], systems.iter().map(String::as_str));
                            if matched.is_empty() {
                                out.push(frag.join("."));
                                continue;
                            }
                            for sys in matched {
                                let mut v = vec![frag[0].clone(), quote_if_needed(&sys)];
                                v.extend_from_slice(&frag[2..]);
                                out.push(v.join("."));
                            }
                        }
                        Err(e) => {
                            debug!(prefix = %prefix, error = %e, "system discovery failed; falling back to single-wildcard fragment");
                            out.push(frag.join("."));
                        }
                    }
                } else {
                    out.push(frag.join("."));
                }
            }
        }

        // De-duplicate while preserving the original order.
        let mut seen = HashSet::new();
        out.retain(|w| seen.insert(w.clone()));
        out
    }

    /// One-shot AttrNames query against the worker pool. Acquires a worker,
    /// marks it dead on protocol failure.
    async fn fetch_attr_names(&self, repository: &str, path: &str) -> Result<Vec<String>> {
        let mut worker = self.pool.acquire().await?;
        match worker.attr_names(repository.to_string(), path.to_string()).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
    }
}

/// Splits a Nix attribute path on `.`, respecting double-quoted segments.
/// Mirror of the helper in [`super::flake`] kept here so the pool does not
/// reach into a sibling module's private API.
fn split_attr_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in path.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '.' if !in_quotes => {
                segments.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    segments.push(current);
    segments
}

/// Returns the entries from `candidates` matching a pattern of the form
/// `<prefix>*<suffix>` (only one `*` supported, mirroring the limitation of
/// [`super::flake::discover_derivations`]).
fn match_pattern<'a, I>(pattern: &str, candidates: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() != 2 {
        return Vec::new();
    }
    let (start, end) = (parts[0], parts[1]);
    candidates
        .into_iter()
        .filter(|c| c.starts_with(start) && c.ends_with(end) && c.len() >= start.len() + end.len())
        .map(|c| c.to_string())
        .collect()
}

/// Wrap an attribute name in `"…"` if it contains characters that are not
/// valid in an unquoted Nix attribute path (most commonly `-` or `.`).
fn quote_if_needed(name: &str) -> String {
    let needs_quote = name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '_'));
    if needs_quote {
        format!("\"{}\"", name)
    } else {
        name.to_string()
    }
}

#[async_trait]
impl DerivationResolver for WorkerPoolResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<Vec<String>> {
        // Wildcard expansion: a bare `*` becomes
        // `*.*` and `*.*.*` so we discover both depth-2 (e.g. `formatter.<sys>`)
        // and depth-3 (e.g. `packages.<sys>.hello`) attribute paths.
        let expanded: Vec<String> = wildcards
            .into_iter()
            .flat_map(|w| {
                if w == "*" {
                    vec!["*.*".to_string(), "*.*.*".to_string()]
                } else {
                    vec![w]
                }
            })
            .collect();

        // Split wildcards by FLAKE_START prefix and (where applicable) by system
        // so we can dispatch each fragment to a separate worker in parallel.
        let fragments = self.expand_wildcards_for_pool(&repository, expanded).await;

        if fragments.is_empty() {
            return Ok(vec![]);
        }

        // Round-robin into one chunk per worker.
        let n_workers = self.pool.max().min(fragments.len()).max(1);
        let mut chunks: Vec<Vec<String>> = (0..n_workers).map(|_| Vec::new()).collect();
        for (idx, w) in fragments.into_iter().enumerate() {
            chunks[idx % n_workers].push(w);
        }

        let mut tasks: FuturesUnordered<_> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                let pool = Arc::clone(&self.pool);
                let repository = repository.clone();
                async move {
                    let mut worker = pool.acquire().await?;
                    match worker.list(repository, chunk).await {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            worker.mark_dead();
                            Err(e)
                        }
                    }
                }
            })
            .collect();

        let mut all: HashSet<String> = HashSet::new();
        while let Some(chunk_result) = tasks.next().await {
            match chunk_result {
                Ok(items) => all.extend(items),
                Err(e) => {
                    warn!(error = %e, "eval worker list chunk failed");
                    return Err(e);
                }
            }
        }
        Ok(all.into_iter().collect())
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<Vec<ResolvedDerivation>> {
        if attrs.is_empty() {
            return Ok(vec![]);
        }

        // Round-robin partition into one chunk per worker, preserving the
        // original index so we can re-order at the end.
        let n_workers = self.pool.max().min(attrs.len());
        let mut chunks: Vec<Vec<(usize, String)>> = (0..n_workers).map(|_| Vec::new()).collect();
        for (idx, a) in attrs.into_iter().enumerate() {
            chunks[idx % n_workers].push((idx, a));
        }

        let mut tasks: FuturesUnordered<_> = chunks
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|chunk| {
                let pool = Arc::clone(&self.pool);
                let repository = repository.clone();
                async move {
                    let mut worker = pool.acquire().await?;
                    let attrs_only: Vec<String> = chunk.iter().map(|(_, a)| a.clone()).collect();
                    let items = match worker.resolve(repository, attrs_only).await {
                        Ok(v) => v,
                        Err(e) => {
                            worker.mark_dead();
                            return Err(e);
                        }
                    };

                    // Re-stitch responses to their original indices. The worker
                    // returns items in the order it received them.
                    if items.len() != chunk.len() {
                        anyhow::bail!(
                            "eval worker returned {} items for {} attrs",
                            items.len(),
                            chunk.len()
                        );
                    }
                    let indexed: Vec<(usize, ResolvedDerivation)> = chunk
                        .into_iter()
                        .zip(items)
                        .map(|((idx, attr), item)| {
                            let result = match (item.drv_path, item.error) {
                                (Some(drv), _) => Ok((drv, item.references)),
                                (None, Some(msg)) => Err(anyhow::anyhow!(msg)),
                                (None, None) => {
                                    Err(anyhow::anyhow!("eval worker returned empty result"))
                                }
                            };
                            (idx, (attr, result))
                        })
                        .collect();
                    anyhow::Ok(indexed)
                }
            })
            .collect();

        let mut indexed: Vec<(usize, ResolvedDerivation)> = Vec::new();
        while let Some(chunk_result) = tasks.next().await {
            match chunk_result {
                Ok(items) => indexed.extend(items),
                Err(e) => {
                    warn!(error = %e, "eval worker chunk failed");
                    return Err(e);
                }
            }
        }

        indexed.sort_by_key(|(idx, _)| *idx);
        Ok(indexed.into_iter().map(|(_, r)| r).collect())
    }

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation> {
        let full_path = nix_store_path(&drv_path);
        let bytes = tokio::fs::read(&full_path)
            .await
            .with_context(|| format!("Failed to read derivation file: {}", full_path))?;
        parse_drv(&bytes).with_context(|| format!("Failed to parse derivation {}", drv_path))
    }

    async fn get_features(&self, drv_path: String) -> Result<(Architecture, Vec<String>)> {
        if !drv_path.ends_with(".drv") {
            return Ok((Architecture::BUILTIN, vec![]));
        }
        let drv = self.get_derivation(drv_path.clone()).await?;
        let features = drv.required_system_features();
        let system: Architecture = drv.system.as_str().try_into().map_err(|e| {
            anyhow::anyhow!("{} has invalid system architecture: {:?}", drv_path, e)
        })?;
        Ok((system, features))
    }
}
