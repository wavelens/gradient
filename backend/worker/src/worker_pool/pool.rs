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
use tracing::{debug, error, trace, warn};

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
        trace!(exe = %exe.display(), "spawning eval worker subprocess");
        let mut command = Command::new(&exe);
        command
            .arg("--eval-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        // Match the Nix CLI: bump the subprocess's stack to 64 MiB so libnix's
        // libstdc++ std::regex DFS executor (used by `builtins.match` /
        // `builtins.split`) doesn't overflow on deep patterns. Upstream Nix
        // calls `setStackSize(64 * 1024 * 1024)` in `initNix`; we use the C
        // API directly and inherit the default 8 MiB, which is too small for
        // some flakes.
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                let lim = libc::rlimit {
                    rlim_cur: 64 * 1024 * 1024,
                    rlim_max: 64 * 1024 * 1024,
                };
                if libc::setrlimit(libc::RLIMIT_STACK, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command.spawn().context("spawning eval worker subprocess")?;

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
        trace!(pid = self.child.id(), ?req, "sending eval worker request");
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
            let pid = self.child.id();
            let status =
                match tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait())
                    .await
                {
                    Ok(Ok(s)) => format!("{s}"),
                    Ok(Err(e)) => format!("wait error: {e}"),
                    Err(_) => {
                        let mut diag = String::from("still alive after 2s");
                        if let Some(p) = pid {
                            if let Ok(target) = std::fs::read_link(format!("/proc/{p}/fd/1")) {
                                diag.push_str(&format!("; /proc/{p}/fd/1 -> {}", target.display()));
                            }
                            if let Ok(state) = std::fs::read_to_string(format!("/proc/{p}/status"))
                                && let Some(line) = state.lines().find(|l| l.starts_with("State:"))
                            {
                                diag.push_str(&format!("; {line}"));
                            }
                            if let Ok(wchan) = std::fs::read_to_string(format!("/proc/{p}/wchan")) {
                                diag.push_str(&format!("; wchan={}", wchan.trim()));
                            }
                        }
                        diag
                    }
                };
            anyhow::bail!("eval worker closed pipe (pid={pid:?}, exit={status})");
        }

        trace!(
            pid = self.child.id(),
            bytes = n,
            "received eval worker response"
        );
        serde_json::from_str(self.line.trim_end())
            .inspect_err(|_| {
                error!(
                    "Failed to parse JSON. Raw input: |{}|",
                    self.line.trim_end()
                );
            })
            .context("parsing eval worker response")
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

    /// Send a `Shutdown` request and wait briefly for the child to exit.
    /// Used when the parent is recycling a still-healthy worker so the
    /// subprocess can run libnix's atexit handlers (flush eval-cache
    /// SQLite, release locks, drop temp roots) instead of being SIGKILL'd
    /// by `kill_on_drop`.
    async fn shutdown(mut self) {
        let pid = self.child.id();
        trace!(pid, "sending Shutdown to eval worker");
        let mut bytes = match serde_json::to_vec(&EvalRequest::Shutdown) {
            Ok(b) => b,
            Err(e) => {
                warn!(pid, "failed to serialize Shutdown request: {e}");
                return;
            }
        };
        bytes.push(b'\n');
        if let Err(e) = self.stdin.write_all(&bytes).await {
            debug!(pid, "failed to write Shutdown to eval worker: {e}");
            return;
        }
        let _ = self.stdin.flush().await;
        drop(self.stdin);
        trace!(pid, "Shutdown sent; waiting for eval worker to exit");
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(Ok(status)) => trace!(pid, ?status, "eval worker exited cleanly"),
            Ok(Err(e)) => debug!(pid, "waiting on eval worker exit: {e}"),
            Err(_) => warn!(
                pid,
                "eval worker did not exit within 5s of Shutdown; will be killed"
            ),
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
                if self.healthy {
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        trace!("scheduling graceful shutdown of eval worker");
                        handle.spawn(async move {
                            worker.shutdown().await;
                        });
                        return;
                    }
                    trace!("no tokio runtime available; killing eval worker via Drop");
                }
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
