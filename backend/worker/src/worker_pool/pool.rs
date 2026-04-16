/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use std::ops::{Deref, DerefMut};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::debug;

use crate::nix::eval_worker::{EvalRequest, EvalResponse, ResolvedItem};

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
    /// Number of `list` / `resolve` calls served since spawn. The pool
    /// uses this to recycle the subprocess after a configurable number
    /// of evaluations so Nix's Boehm-GC-allocated memory (which is
    /// never released back to the OS) cannot grow unbounded.
    pub(super) evaluations_served: usize,
}

impl EvalWorker {
    /// Spawn a new worker by re-execing the current binary with `--eval-worker`.
    pub(super) async fn spawn() -> Result<Self> {
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

        // Mark the subprocess as the preferred OOM-kill target so that the kernel
        // sacrifices eval workers (which hold large Nix/Boehm-GC heaps) before
        // the parent process or other services when memory runs low.
        #[cfg(target_os = "linux")]
        if let Some(pid) = child.id() {
            let path = format!("/proc/{pid}/oom_score_adj");
            if let Err(e) = std::fs::write(&path, "600") {
                tracing::warn!(pid, "failed to set oom_score_adj for eval worker: {e}");
            }
        }

        Ok(Self {
            child,
            stdin,
            stdout,
            line: String::new(),
            evaluations_served: 0,
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

    pub(super) async fn list(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::List {
                repository,
                wildcards,
            })
            .await?
        {
            EvalResponse::ListOk { attrs, warnings } => Ok((attrs, warnings)),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to List"),
        }
    }

    pub(super) async fn resolve(
        &mut self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedItem>, Vec<String>)> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::Resolve { repository, attrs })
            .await?
        {
            EvalResponse::ResolveOk { items, warnings } => Ok((items, warnings)),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Resolve"),
        }
    }

    pub(super) async fn attr_names(
        &mut self,
        repository: String,
        path: String,
    ) -> Result<Vec<String>> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::AttrNames { repository, path })
            .await?
        {
            EvalResponse::AttrNamesOk { keys, .. } => Ok(keys),
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
    /// Recycle an `EvalWorker` subprocess after it has served this many
    /// `list` / `resolve` calls. Nix's Boehm GC never shrinks the
    /// process heap, so long-lived workers grow monotonically; killing
    /// and respawning the subprocess is the only reliable way to
    /// release evaluation memory back to the OS. `0` disables
    /// recycling.
    max_evaluations_per_worker: usize,
}

impl EvalWorkerPool {
    pub fn new(max: usize, max_evaluations_per_worker: usize) -> Self {
        let max = max.max(1);
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
            max,
            max_evaluations_per_worker,
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
            max_evaluations_per_worker: self.max_evaluations_per_worker,
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
    max_evaluations_per_worker: usize,
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
            // Recycle after the configured eval count to reclaim
            // Boehm-GC memory held by the subprocess.
            let overused = self.max_evaluations_per_worker > 0
                && worker.evaluations_served >= self.max_evaluations_per_worker;
            if overused {
                debug!(
                    evaluations = worker.evaluations_served,
                    "recycling eval worker (max evaluations reached)"
                );
                drop(worker);
                return;
            }

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
