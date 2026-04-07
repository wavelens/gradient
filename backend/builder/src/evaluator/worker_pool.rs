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
use gradient_core::derivation::{Derivation, parse_drv};
use gradient_core::evaluator::{DerivationResolver, ResolvedDerivation};
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
        self.stdin.flush().await.context("flushing eval worker stdin")?;

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
            if self.healthy {
                if let Ok(mut idle) = self.idle.lock() {
                    idle.push(worker);
                    return;
                }
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

        let mut worker = self.pool.acquire().await?;
        match worker.list(repository, expanded).await {
            Ok(v) => Ok(v),
            Err(e) => {
                worker.mark_dead();
                Err(e)
            }
        }
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
                    let attrs_only: Vec<String> =
                        chunk.iter().map(|(_, a)| a.clone()).collect();
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
                                (None, None) => Err(anyhow::anyhow!(
                                    "eval worker returned empty result"
                                )),
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
        let system: Architecture = drv
            .system
            .as_str()
            .try_into()
            .map_err(|e| anyhow::anyhow!("{} has invalid system architecture: {:?}", drv_path, e))?;
        Ok((system, features))
    }
}
