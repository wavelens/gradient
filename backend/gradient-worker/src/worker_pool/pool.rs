/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pool lifecycle for eval-worker subprocesses: lazy spawn, test-on-borrow
//! checkout, RAII return, and graceful shutdown. The subprocess handle and
//! wire transport live in [`super::transport`], the RAM math and reaper in
//! [`super::memory`].

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, trace};

use super::transport::EvalWorker;

/// How long `acquire` sleeps between pressure checks while the host is below
/// the free-RAM margin.
const PRESSURE_BACKOFF: Duration = Duration::from_millis(200);

/// Pool of [`EvalWorker`]s.
///
/// Workers are created lazily on `acquire`. Idle workers are reused; broken
/// ones are discarded so the next `acquire` spawns a fresh replacement.
#[derive(Debug)]
pub(super) struct EvalWorkerPool {
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
    pub(super) fn new(max: usize, max_eval_rss: u64, eval_cache_dir: String) -> Self {
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

    pub(super) fn max(&self) -> usize {
        self.max
    }

    pub(super) fn max_eval_rss(&self) -> u64 {
        self.max_eval_rss
    }

    /// Arm the memory guard: the back-pressure margin read by `acquire`. The
    /// reaper loop uses the same value. `0` leaves it disabled.
    pub(super) fn configure_memory_guard(&self, min_free_bytes: u64) {
        self.min_free_bytes.store(min_free_bytes, Ordering::Relaxed);
    }

    /// Snapshot of every live eval-subprocess pid.
    pub(super) fn live_pids(&self) -> Vec<u32> {
        self.live.lock().unwrap().iter().copied().collect()
    }

    /// Reaper feedback: latches the pressure flag `acquire` throttles on.
    pub(super) fn note_pressure(&self, pressured: bool) {
        self.under_pressure.store(pressured, Ordering::Relaxed);
    }

    /// Acquire a worker, blocking until one is available. Reuses an idle
    /// worker if any, otherwise spawns a fresh subprocess.
    pub(super) async fn acquire(&self) -> Result<PooledEvalWorker> {
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
            tokio::time::sleep(PRESSURE_BACKOFF).await;
        }

        // Test-on-borrow: an idle subprocess can die while pooled (memory
        // reaper SIGKILL, kernel OOM via the elevated oom_score_adj, or crash).
        // Skip such corpses so we never hand out a worker whose first stdin
        // write fails with a broken pipe; spawn fresh once the idle vec drains.
        let worker = loop {
            let candidate = self.idle.lock().unwrap().pop();
            match candidate {
                Some(mut w) => {
                    let pid = w.pid();
                    if w.is_alive() {
                        break w;
                    }
                    debug!(?pid, "discarding dead idle eval worker on checkout");
                    drop(w);
                }
                None => {
                    break EvalWorker::spawn(&self.eval_cache_dir, Arc::clone(&self.live))
                        .await
                        .context("spawning fresh eval worker")?;
                }
            }
        };

        Ok(PooledEvalWorker {
            worker: Some(worker),
            idle: Arc::clone(&self.idle),
            healthy: true,
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
    /// waits for each within the transport's grace period.
    ///
    /// Idempotent.
    pub(super) async fn shutdown(&self) {
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

/// What `PooledEvalWorker::drop` does with its worker, computed once from
/// the pool state + health so the action code exists exactly once.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// Pool is shutting down and the worker is healthy: let the subprocess
    /// run libnix's atexit handlers instead of SIGKILL via `kill_on_drop`.
    GracefulShutdown,
    /// Healthy worker, pool still open: back into the idle vec for reuse.
    ReturnToIdle,
    /// Broken (or the pool cannot take it back): drop it, `kill_on_drop`
    /// ensures the child does not linger.
    Kill,
}

fn dispose(shutting_down: bool, healthy: bool) -> Disposition {
    match (shutting_down, healthy) {
        (true, true) => Disposition::GracefulShutdown,
        (false, true) => Disposition::ReturnToIdle,
        (_, false) => Disposition::Kill,
    }
}

/// RAII handle returned by [`EvalWorkerPool::acquire`].
///
/// Dereferences to `&mut EvalWorker`. On drop, returns the worker to the pool
/// if [`PooledEvalWorker::healthy`] is `true`, otherwise discards it (the
/// child is killed by `kill_on_drop`).
pub(super) struct PooledEvalWorker {
    worker: Option<EvalWorker>,
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    healthy: bool,
    shutting_down: Arc<AtomicBool>,
    _permit: OwnedSemaphorePermit,
}

impl PooledEvalWorker {
    /// Mark this worker as broken so it won't be returned to the pool.
    pub(super) fn mark_dead(&mut self) {
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
        let Some(worker) = self.worker.take() else {
            return;
        };

        match dispose(self.shutting_down.load(Ordering::SeqCst), self.healthy) {
            Disposition::GracefulShutdown => {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    trace!("pool shutting down; gracefully terminating eval worker");
                    handle.spawn(worker.shutdown());
                } else {
                    // No runtime to drive the graceful path: kill_on_drop it.
                    trace!("pool shutting down; no tokio runtime - killing eval worker via Drop");
                    drop(worker);
                }
            }
            Disposition::ReturnToIdle => {
                if let Ok(mut idle) = self.idle.lock() {
                    idle.push(worker);
                } else {
                    debug!("discarding eval worker (idle mutex poisoned)");
                    drop(worker);
                }
            }
            Disposition::Kill => {
                debug!("discarding eval worker (unhealthy)");
                drop(worker);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::process::Command;

    /// Spawn a `cat` subprocess wrapped as an `EvalWorker`. `cat` echoes
    /// stdin to stdout and exits cleanly when stdin closes - exactly the
    /// behaviour `EvalWorker::shutdown` relies on (it writes the Shutdown
    /// frame then drops stdin).
    fn fake_worker() -> EvalWorker {
        EvalWorker::from_command(Command::new("cat")).expect("spawn cat")
    }

    /// A worker whose subprocess has already been killed and reaped - stands in
    /// for an idle worker the memory reaper or kernel OOM-killer took out while
    /// it sat in the pool.
    async fn dead_worker() -> EvalWorker {
        let mut w = fake_worker();
        w.child_mut().start_kill().expect("kill cat");
        w.child_mut().wait().await.expect("reap cat");
        w
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn disposition_covers_all_states() {
        assert_eq!(dispose(true, true), Disposition::GracefulShutdown);
        assert_eq!(dispose(false, true), Disposition::ReturnToIdle);
        assert_eq!(dispose(true, false), Disposition::Kill);
        assert_eq!(dispose(false, false), Disposition::Kill);
    }

    #[tokio::test]
    async fn acquire_skips_dead_idle_worker() {
        let pool = EvalWorkerPool::new(4, 2 * GIB, String::new());
        let live = fake_worker();
        let live_pid = live.pid();
        assert!(live_pid.is_some());

        // idle is a stack: push the live worker first so the dead corpse (pushed
        // last) is popped first and must be skipped.
        pool.push_for_test(live);
        pool.push_for_test(dead_worker().await);

        let worker = pool.acquire().await.expect("acquire a live worker");
        assert_eq!(
            worker.pid(),
            live_pid,
            "acquire must skip the dead idle corpse and return the live worker"
        );
    }

    #[test]
    fn pid_guard_deregisters_pid_on_drop() {
        use super::super::transport::PidGuard;

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
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new());
        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("shutdown should not hang on empty pool");
        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_idle_workers_gracefully() {
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new());
        pool.push_for_test(fake_worker());
        pool.push_for_test(fake_worker());
        assert_eq!(pool.idle_count(), 2);

        tokio::time::timeout(Duration::from_secs(6), pool.shutdown())
            .await
            .expect("shutdown should complete within the per-worker grace budget");

        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0, "idle vec must be drained");
    }

    #[tokio::test]
    async fn acquire_after_shutdown_errors() {
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new());
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
        let pool = Arc::new(EvalWorkerPool::new(1, 2 * GIB, String::new()));
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
