/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, error, trace, warn};

use crate::nix::eval_worker::{EvalRequest, EvalResponse, ResolvedItem};
use crate::worker_pool::eval_stats::StatsDelta;

/// Handle to a single live eval-worker subprocess.
///
/// Owns the child plus its piped stdin/stdout. Each request writes one JSON
/// line to stdin and reads one JSON line back from stdout - the protocol is
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
    /// RAII deregistration of this subprocess's pid from the pool's live
    /// registry. A field (rather than `impl Drop for EvalWorker`) so `shutdown`
    /// can still move individual fields out of `self`.
    pid_guard: PidGuard,
}

/// Removes an eval subprocess pid from the pool's live registry on drop, so the
/// memory reaper never targets a worker we have already discarded. The child
/// itself is reaped by `kill_on_drop`.
struct PidGuard {
    /// Live-pid registry of the owning pool. `None` for test workers built via
    /// `from_command` (no pool, nothing to deregister from).
    live: Option<Arc<Mutex<HashSet<u32>>>>,
    pid: Option<u32>,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let (Some(live), Some(pid)) = (&self.live, self.pid) {
            live.lock().unwrap().remove(&pid);
        }
    }
}

impl EvalWorker {
    /// Spawn a new worker by re-execing the current binary with `--eval-worker`.
    /// The subprocess is single-threaded and does not fork; pool size is the
    /// eval concurrency and RSS is bounded parent-side (see [`Self::rss_bytes`]).
    ///
    /// `eval_cache_dir` is exported as `NIX_CACHE_HOME` so parent and worker
    /// agree on where Nix's `eval-cache-v6/<fingerprint>.sqlite` lives.
    pub(super) async fn spawn(
        eval_cache_dir: &str,
        live: Arc<Mutex<HashSet<u32>>>,
    ) -> Result<Self> {
        let exe = std::env::current_exe().context("locating current executable")?;
        trace!(exe = %exe.display(), "spawning eval worker subprocess");
        let mut command = Command::new(&exe);
        command.arg("--eval-worker");
        command.env("NIX_CACHE_HOME", eval_cache_dir);
        for &(k, v) in super::eval_stats::eval_worker_stats_env(super::eval_stats::metrics_enabled()) {
            command.env(k, v);
        }

        // Match the Nix CLI: bump the subprocess's stack to 64 MiB so libnix's
        // libstdc++ std::regex DFS executor (used by `builtins.match` /
        // `builtins.split`) doesn't overflow on deep patterns. Upstream Nix
        // calls `setStackSize(64 * 1024 * 1024)` in `initNix`; we use the C
        // API directly and inherit the default 8 MiB, which is too small for
        // some flakes.
        // SAFETY: `pre_exec` runs in the forked child before `exec`, so its body
        // must be async-signal-safe; it only builds an `rlimit` and calls
        // `setrlimit`, both of which are signal-safe.
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

        let mut worker = Self::from_command(command)?;

        // Register the pid so the memory reaper can find this subprocess even
        // while it is checked out of the pool. Deregistered by `PidGuard`.
        if let Some(pid) = worker.pid_guard.pid {
            live.lock().unwrap().insert(pid);
        }
        worker.pid_guard.live = Some(live);

        // Mark the subprocess as the preferred OOM-kill target so that the kernel
        // sacrifices eval workers (which hold large Nix/Boehm-GC heaps) before
        // the parent process or other services when memory runs low.
        #[cfg(target_os = "linux")]
        if let Some(pid) = worker.child.id() {
            let path = format!("/proc/{pid}/oom_score_adj");
            if let Err(e) = std::fs::write(&path, "600") {
                tracing::warn!(pid, error = %e, "failed to set oom_score_adj for eval worker");
            }
        }

        Ok(worker)
    }

    /// Spawn the given pre-configured command and wrap its stdin/stdout into
    /// an [`EvalWorker`]. Test seam used by pool tests to stand up a
    /// controllable subprocess (e.g. `cat`) without depending on libnix.
    fn from_command(mut command: Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().context("spawning eval worker subprocess")?;
        let pid = child.id();
        let stdin = child.stdin.take().context("worker stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("worker stdout missing")?);
        Ok(Self {
            child,
            stdin,
            stdout,
            line: String::new(),
            evaluations_served: 0,
            pid_guard: PidGuard { live: None, pid },
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
            .inspect_err(|error| {
                error!(
                    %error,
                    raw_input = self.line.trim_end(),
                    "Failed to parse eval worker JSON response"
                );
            })
            .context("parsing eval worker response")
    }

    pub(super) async fn plan(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<Vec<String>> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::Plan {
                repository,
                wildcards,
            })
            .await?
        {
            EvalResponse::PlanOk { sub_patterns } => Ok(sub_patterns),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Plan"),
        }
    }

    pub(super) async fn list(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>, Option<StatsDelta>)> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::List {
                repository,
                wildcards,
            })
            .await?
        {
            EvalResponse::ListOk {
                attrs,
                warnings,
                stats,
            } => Ok((attrs, warnings, stats)),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to List"),
        }
    }

    pub(super) async fn fingerprint(&mut self, repository: String) -> Result<Option<String>> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::Fingerprint { repository })
            .await?
        {
            EvalResponse::FingerprintOk { fingerprint } => Ok(fingerprint),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Fingerprint"),
        }
    }

    pub(super) async fn checkpoint(&mut self, repository: String) -> Result<()> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::Checkpoint { repository })
            .await?
        {
            EvalResponse::CheckpointOk => Ok(()),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Checkpoint"),
        }
    }

    /// Send a `Shutdown` request and wait briefly for the child to exit.
    /// Used when the parent is recycling a still-healthy worker so the
    /// subprocess can run libnix's atexit handlers (flush eval-cache
    /// SQLite, release locks, drop temp roots) instead of being SIGKILL'd
    /// by `kill_on_drop`.
    pub(super) async fn shutdown(mut self) {
        let pid = self.child.id();
        trace!(pid, "sending Shutdown to eval worker");
        let mut bytes = match serde_json::to_vec(&EvalRequest::Shutdown) {
            Ok(b) => b,
            Err(e) => {
                warn!(pid, error = %e, "failed to serialize Shutdown request");
                return;
            }
        };
        bytes.push(b'\n');
        if let Err(e) = self.stdin.write_all(&bytes).await {
            debug!(pid, error = %e, "failed to write Shutdown to eval worker");
            return;
        }
        let _ = self.stdin.flush().await;
        drop(self.stdin);
        trace!(pid, "Shutdown sent; waiting for eval worker to exit");
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(Ok(status)) => trace!(pid, ?status, "eval worker exited cleanly"),
            Ok(Err(e)) => debug!(pid, error = %e, "waiting on eval worker exit failed"),
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
    ) -> Result<(Vec<ResolvedItem>, Vec<String>, Option<StatsDelta>)> {
        self.evaluations_served += 1;
        match self
            .request(&EvalRequest::Resolve { repository, attrs })
            .await?
        {
            EvalResponse::ResolveOk {
                items,
                warnings,
                stats,
            } => Ok((items, warnings, stats)),
            EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {}", message)),
            _ => anyhow::bail!("eval worker: unexpected response to Resolve"),
        }
    }

    /// Resident set size of the subprocess in bytes, read from
    /// `/proc/<pid>/statm` (field 2 = resident pages × 4 KiB). Returns 0 if
    /// the pid is gone or the read fails so the pool never panics on it.
    #[cfg(target_os = "linux")]
    pub(super) fn rss_bytes(&self) -> u64 {
        let Some(pid) = self.child.id() else {
            return 0;
        };

        let Ok(statm) = std::fs::read_to_string(format!("/proc/{pid}/statm")) else {
            return 0;
        };

        statm
            .split_whitespace()
            .nth(1)
            .and_then(|pages| pages.parse::<u64>().ok())
            .map(|pages| pages * 4096)
            .unwrap_or(0)
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn rss_bytes(&self) -> u64 {
        0
    }
}

impl std::fmt::Debug for EvalWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalWorker")
            .field("pid", &self.child.id())
            .finish()
    }
}

/// Eval-pool size that keeps `size * max_eval_rss` within `ram_budget` (the
/// no-OOM invariant), capped at the configured `fork_workers` and floored at 1
/// so even a tiny host still evaluates - one shard at a time, slower, but it
/// completes. Lowering `max_eval_rss` therefore trades parallelism for a smaller
/// footprint, never the ability to finish.
pub fn budgeted_pool_size(fork_workers: usize, max_eval_rss: u64, ram_budget: u64) -> usize {
    let mem_bound = (ram_budget / max_eval_rss.max(1)).max(1) as usize;

    fork_workers.min(mem_bound).max(1)
}

/// Adaptive free-RAM margin (bytes): the configured `min_free_ram_mb` if set,
/// else `max(1 GiB, 10% of total RAM)`. Below this the reaper acts and `acquire`
/// back-pressures. Lifted out for unit testing.
pub fn memory_guard_bytes(min_free_ram_mb: u64, total_ram_bytes: u64) -> u64 {
    if min_free_ram_mb > 0 {
        min_free_ram_mb * 1024 * 1024
    } else {
        (total_ram_bytes / 10).max(1024 * 1024 * 1024)
    }
}

/// RSS (bytes) of an arbitrary pid from `/proc/<pid>/statm` (field 2 = resident
/// pages × 4 KiB). `None` if the pid is gone or the read fails.
#[cfg(target_os = "linux")]
fn rss_of_pid(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    statm
        .split_whitespace()
        .nth(1)
        .and_then(|pages| pages.parse::<u64>().ok())
        .map(|pages| pages * 4096)
}

#[cfg(not(target_os = "linux"))]
fn rss_of_pid(_pid: u32) -> Option<u64> {
    None
}

/// Background memory guard: when host `MemAvailable` drops below
/// `min_free_bytes`, SIGKILL the largest live eval subprocess so a runaway
/// evaluation cannot take the whole host down. The victim's parent task then
/// sees its pipe close and reports the eval failed - converting a would-be host
/// OOM (which could kill the worker itself and strand the job, since the server
/// only learns of a clean disconnect) into a single bounded eval failure.
///
/// Exits when the pool is dropped (worker shutdown). A no-op when disabled.
pub(super) async fn memory_reaper_loop(pool: std::sync::Weak<EvalWorkerPool>, min_free_bytes: u64) {
    if min_free_bytes == 0 {
        return;
    }

    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        let Some(pool) = pool.upgrade() else {
            return;
        };

        sys.refresh_memory();
        let available = sys.available_memory();
        let pressured = available < min_free_bytes;
        pool.under_pressure.store(pressured, Ordering::Relaxed);
        if !pressured {
            continue;
        }

        let victim = pool
            .live_pids()
            .into_iter()
            .filter_map(|pid| rss_of_pid(pid).map(|rss| (pid, rss)))
            .max_by_key(|&(_, rss)| rss);
        if let Some((pid, rss)) = victim {
            warn!(
                pid,
                rss_mb = rss / (1024 * 1024),
                available_mb = available / (1024 * 1024),
                min_free_mb = min_free_bytes / (1024 * 1024),
                "host memory below safety margin; reaping largest eval subprocess to avoid OOM"
            );
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
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
    /// RSS ceiling (bytes) above which a released worker is discarded so the
    /// next `acquire` spawns a fresh subprocess (parent-side recycling).
    max_eval_rss: u64,
    /// Exported as `NIX_CACHE_HOME` for every spawned worker so the on-disk
    /// eval cache lands where the parent expects it.
    eval_cache_dir: String,
    /// Set by [`EvalWorkerPool::shutdown`]. Causes `PooledEvalWorker::drop`
    /// to gracefully shut its worker down instead of returning it to the
    /// (now-closed) idle vec.
    shutting_down: Arc<AtomicBool>,
    /// Pids of every live eval subprocess (idle or checked out), so the memory
    /// reaper can target the largest under host memory pressure.
    live: Arc<Mutex<HashSet<u32>>>,
    /// Free-RAM margin (bytes) below which the reaper acts and `acquire`
    /// back-pressures. `0` disables both. Set by [`Self::configure_memory_guard`].
    min_free_bytes: AtomicU64,
    /// Latched by the reaper each tick when host `MemAvailable` is below the
    /// margin, read by `acquire` to throttle new evaluations.
    under_pressure: Arc<AtomicBool>,
}

impl EvalWorkerPool {
    pub fn new(max: usize, max_eval_rss: u64, eval_cache_dir: String) -> Self {
        let max = max.max(1);
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
            max,
            max_eval_rss,
            eval_cache_dir,
            shutting_down: Arc::new(AtomicBool::new(false)),
            live: Arc::new(Mutex::new(HashSet::new())),
            min_free_bytes: AtomicU64::new(0),
            under_pressure: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn max(&self) -> usize {
        self.max
    }

    pub fn max_eval_rss(&self) -> u64 {
        self.max_eval_rss
    }

    /// Arm the memory guard: the back-pressure margin read by `acquire`. The
    /// reaper loop uses the same value. `0` leaves it disabled.
    pub fn configure_memory_guard(&self, min_free_bytes: u64) {
        self.min_free_bytes.store(min_free_bytes, Ordering::Relaxed);
    }

    /// Snapshot of every live eval-subprocess pid.
    pub(super) fn live_pids(&self) -> Vec<u32> {
        self.live.lock().unwrap().iter().copied().collect()
    }

    /// Acquire a worker, blocking until one is available. Reuses an idle
    /// worker if any, otherwise spawns a fresh subprocess.
    pub async fn acquire(&self) -> Result<PooledEvalWorker> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("EvalWorkerPool semaphore closed"))?;

        // Back-pressure: under host memory pressure, don't pile a new eval onto
        // others - wait for it to clear. We always let a lone eval proceed
        // (available_permits + 1 == max means this is the only one), so the
        // pool can never deadlock under sustained pressure; it just serialises.
        while self.min_free_bytes.load(Ordering::Relaxed) > 0
            && self.under_pressure.load(Ordering::Relaxed)
            && self.semaphore.available_permits() + 1 < self.max
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        let worker = self.idle.lock().unwrap().pop();
        let worker = match worker {
            Some(w) => w,
            None => EvalWorker::spawn(&self.eval_cache_dir, Arc::clone(&self.live))
                .await
                .context("spawning fresh eval worker")?,
        };

        Ok(PooledEvalWorker {
            worker: Some(worker),
            idle: Arc::clone(&self.idle),
            healthy: true,
            recycle_after: 0,
            shutting_down: Arc::clone(&self.shutting_down),
            _permit: permit,
        })
    }

    /// Gracefully shut the pool down.
    ///
    /// Closes the semaphore (so future [`acquire`](Self::acquire) calls
    /// fail), flips the `shutting_down` flag (so any [`PooledEvalWorker`]
    /// that gets dropped after this point gracefully terminates its
    /// subprocess instead of being returned to the idle vec), then
    /// concurrently sends `Shutdown` to every currently-idle worker and
    /// waits up to 5 s per worker for it to exit.
    ///
    /// Idempotent.
    pub async fn shutdown(&self) {
        // Order matters: set the flag BEFORE closing the semaphore. If we
        // closed first, an in-flight `acquire().await` could resolve with
        // an Err, bypass `PooledEvalWorker::drop` entirely, and the parent
        // task could re-enter the pool path before the flag was visible.
        self.shutting_down.store(true, Ordering::SeqCst);
        self.semaphore.close();

        let drained: Vec<EvalWorker> = {
            let mut idle = self.idle.lock().unwrap();
            std::mem::take(&mut *idle)
        };
        if drained.is_empty() {
            return;
        }
        debug!(
            count = drained.len(),
            "gracefully shutting down idle eval workers"
        );
        let mut tasks: FuturesUnordered<_> = drained.into_iter().map(|w| w.shutdown()).collect();
        while tasks.next().await.is_some() {}
    }

    #[cfg(test)]
    pub(super) fn idle_count(&self) -> usize {
        self.idle.lock().unwrap().len()
    }

    #[cfg(test)]
    pub(super) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Test seam: push a pre-built worker into the idle vec so subsequent
    /// `acquire()` calls reuse it instead of spawning a fresh one.
    #[cfg(test)]
    pub(super) fn push_for_test(&self, worker: EvalWorker) {
        self.idle.lock().unwrap().push(worker);
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
    recycle_after: usize,
    shutting_down: Arc<AtomicBool>,
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
            // Pool is shutting down: gracefully terminate the subprocess so
            // it can run libnix's atexit handlers instead of being SIGKILL'd
            // by `kill_on_drop`. Mirrors the recycle path below.
            if self.shutting_down.load(Ordering::SeqCst) {
                if self.healthy {
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        trace!("pool shutting down; gracefully terminating eval worker");
                        handle.spawn(async move {
                            worker.shutdown().await;
                        });
                        return;
                    }
                    trace!("pool shutting down; no tokio runtime - killing eval worker via Drop");
                }
                drop(worker);
                return;
            }

            // recycle disabled; RSS-based reclamation lands in a later task
            let overused =
                self.recycle_after > 0 && worker.evaluations_served >= self.recycle_after;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Spawn a `cat` subprocess wrapped as an `EvalWorker`. `cat` echoes
    /// stdin to stdout and exits cleanly when stdin closes - exactly the
    /// behaviour `EvalWorker::shutdown` relies on (it writes the Shutdown
    /// JSON line then drops stdin).
    fn fake_worker() -> EvalWorker {
        EvalWorker::from_command(Command::new("cat")).expect("spawn cat")
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn budgeted_pool_size_caps_by_memory() {
        // 8 GiB box, 2 GiB cap, 75% budget (6 GiB) -> 3 shards, capped at cores.
        assert_eq!(budgeted_pool_size(16, 2 * GIB, 6 * GIB), 3);
        // Plenty of RAM -> the configured worker count wins.
        assert_eq!(budgeted_pool_size(8, 2 * GIB, 256 * GIB), 8);
        // Cap >= budget -> still one worker (slower, but never zero).
        assert_eq!(budgeted_pool_size(16, 8 * GIB, 6 * GIB), 1);
        // Degenerate cap never divides by zero.
        assert_eq!(budgeted_pool_size(4, 0, 6 * GIB), 4);
    }

    #[test]
    fn memory_guard_bytes_configured_and_adaptive() {
        // A configured margin wins, converted MiB -> bytes.
        assert_eq!(memory_guard_bytes(2048, 64 * GIB), 2048 * 1024 * 1024);
        // Adaptive: 10% of total when that clears the 1 GiB floor.
        assert_eq!(memory_guard_bytes(0, 64 * GIB), 64 * GIB / 10);
        // Adaptive floor: at least 1 GiB on a small host (10% of 4 GiB < 1 GiB).
        assert_eq!(memory_guard_bytes(0, 4 * GIB), GIB);
    }

    #[test]
    fn pid_guard_deregisters_pid_on_drop() {
        let live = Arc::new(Mutex::new(HashSet::new()));
        live.lock().unwrap().insert(4242u32);
        {
            let _guard = PidGuard {
                live: Some(Arc::clone(&live)),
                pid: Some(4242),
            };
            assert!(live.lock().unwrap().contains(&4242));
        }
        assert!(
            !live.lock().unwrap().contains(&4242),
            "PidGuard must remove its pid from the live registry on drop"
        );
    }

    #[tokio::test]
    async fn shutdown_with_no_idle_workers_returns_immediately() {
        let pool = EvalWorkerPool::new(2, 2 * 1024 * 1024 * 1024, String::new());
        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("shutdown should not hang on empty pool");
        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_idle_workers_gracefully() {
        let pool = EvalWorkerPool::new(2, 2 * 1024 * 1024 * 1024, String::new());
        pool.push_for_test(fake_worker());
        pool.push_for_test(fake_worker());
        assert_eq!(pool.idle_count(), 2);

        tokio::time::timeout(Duration::from_secs(6), pool.shutdown())
            .await
            .expect("shutdown should complete within the per-worker 5s budget");

        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0, "idle vec must be drained");
    }

    #[tokio::test]
    async fn acquire_after_shutdown_errors() {
        let pool = EvalWorkerPool::new(2, 2 * 1024 * 1024 * 1024, String::new());
        pool.shutdown().await;
        match pool.acquire().await {
            Ok(_) => panic!("acquire after shutdown must fail"),
            Err(e) => assert!(
                e.to_string().contains("semaphore closed"),
                "unexpected error: {e}"
            ),
        }
    }

    #[tokio::test]
    async fn inflight_worker_shuts_down_gracefully_on_pool_shutdown() {
        let pool = Arc::new(EvalWorkerPool::new(
            1,
            2 * 1024 * 1024 * 1024,
            String::new(),
        ));
        pool.push_for_test(fake_worker());

        let pooled = pool.acquire().await.expect("acquire");
        assert_eq!(pool.idle_count(), 0);

        // Drive shutdown concurrently with releasing the in-flight worker.
        let pool2 = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { pool2.shutdown().await });

        // Give shutdown a chance to flip the flag before we drop the handle.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(pooled);

        tokio::time::timeout(Duration::from_secs(6), shutdown)
            .await
            .expect("shutdown timed out")
            .expect("shutdown task panicked");

        assert!(pool.is_shutting_down());
        // The dropped in-flight worker must NOT have been pushed back into
        // idle - it should have taken the graceful-shutdown branch.
        assert_eq!(
            pool.idle_count(),
            0,
            "in-flight worker must not be returned to idle once pool is shutting down"
        );
    }
}
