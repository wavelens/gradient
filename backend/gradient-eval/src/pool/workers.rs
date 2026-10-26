/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, trace, warn};

use super::live_thunks::LiveThunks;
use super::lock;
use super::memory::{MemoryGuard, REAP_COOLDOWN, REAPER_INTERVAL, pressured, rss_of_pid, victim};
use super::transport::{EvalWorker, SpawnConfig};

const PRESSURE_BACKOFF: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(super) struct EvalWorkerPool {
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    semaphore: Arc<Semaphore>,
    max: usize,
    max_rss: u64,
    guard: MemoryGuard,
    spawn: SpawnConfig,
    shutting_down: Arc<AtomicBool>,
    live: Arc<Mutex<HashSet<u32>>>,
    under_pressure: Arc<AtomicBool>,
    thunks: Arc<LiveThunks>,
}

impl EvalWorkerPool {
    pub(super) fn new(
        workers: usize,
        max_rss: u64,
        guard: MemoryGuard,
        spawn: SpawnConfig,
        thunks: Arc<LiveThunks>,
    ) -> Self {
        let max = workers.max(1);
        Self {
            idle: Arc::new(Mutex::new(Vec::new())),
            semaphore: Arc::new(Semaphore::new(max)),
            max,
            max_rss,
            guard,
            spawn,
            shutting_down: Arc::new(AtomicBool::new(false)),
            live: Arc::new(Mutex::new(HashSet::new())),
            under_pressure: Arc::new(AtomicBool::new(false)),
            thunks,
        }
    }

    pub(super) fn max(&self) -> usize {
        self.max
    }

    pub(super) fn max_rss(&self) -> u64 {
        self.max_rss
    }

    pub(super) fn live_thunks(&self) -> Arc<LiveThunks> {
        Arc::clone(&self.thunks)
    }

    pub(super) fn live_pids(&self) -> Vec<u32> {
        lock(&self.live).iter().copied().collect()
    }

    pub(super) fn note_pressure(&self, pressured: bool) {
        self.under_pressure.store(pressured, Ordering::Relaxed);
    }

    pub(super) async fn acquire(&self) -> Result<PooledEvalWorker> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("EvalWorkerPool semaphore closed"))?;

        while self.guard.is_active()
            && self.under_pressure.load(Ordering::Relaxed)
            && self.semaphore.available_permits() + 1 < self.max
        {
            tokio::time::sleep(PRESSURE_BACKOFF).await;
        }

        let worker = loop {
            let candidate = {
                let mut idle = lock(&self.idle);
                leanest(&idle).map(|at| idle.swap_remove(at))
            };
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
                    break EvalWorker::spawn(
                        &self.spawn,
                        Arc::clone(&self.live),
                        Arc::clone(&self.thunks),
                    )
                    .await
                    .context("spawn fresh eval worker")?;
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

    pub(super) async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.semaphore.close();
        self.release_idle().await;
    }

    pub(super) async fn release_idle(&self) {
        let drained: Vec<EvalWorker> = {
            let mut idle = lock(&self.idle);
            std::mem::take(&mut *idle)
        };
        if drained.is_empty() {
            return;
        }
        debug!(count = drained.len(), "releasing idle eval workers");
        let mut tasks: FuturesUnordered<_> = drained.into_iter().map(|w| w.shutdown()).collect();
        while tasks.next().await.is_some() {}
    }

    #[cfg(test)]
    pub(super) fn idle_count(&self) -> usize {
        lock(&self.idle).len()
    }

    #[cfg(test)]
    pub(super) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn push_for_test(&self, worker: EvalWorker) {
        lock(&self.idle).push(worker);
    }
}

fn leanest(idle: &[EvalWorker]) -> Option<usize> {
    idle.iter()
        .enumerate()
        .min_by_key(|(_, worker)| worker.rss_bytes())
        .map(|(at, _)| at)
}

#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    GracefulShutdown,
    ReturnToIdle,
    Kill,
}

fn dispose(shutting_down: bool, healthy: bool) -> Disposition {
    match (shutting_down, healthy) {
        (true, true) => Disposition::GracefulShutdown,
        (false, true) => Disposition::ReturnToIdle,
        (_, false) => Disposition::Kill,
    }
}

pub(super) struct PooledEvalWorker {
    worker: Option<EvalWorker>,
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    healthy: bool,
    shutting_down: Arc<AtomicBool>,
    _permit: OwnedSemaphorePermit,
}

impl PooledEvalWorker {
    pub(super) fn mark_dead(&mut self) {
        self.healthy = false;
    }
}

impl Deref for PooledEvalWorker {
    type Target = EvalWorker;
    fn deref(&self) -> &Self::Target {
        self.worker
            .as_ref()
            .expect("the worker is taken only by Drop")
    }
}

impl DerefMut for PooledEvalWorker {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.worker
            .as_mut()
            .expect("the worker is taken only by Drop")
    }
}

impl Drop for PooledEvalWorker {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };

        let healthy = self.healthy && !worker.in_flight();
        match dispose(self.shutting_down.load(Ordering::SeqCst), healthy) {
            Disposition::GracefulShutdown => {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    trace!("pool shutting down; gracefully terminating eval worker");
                    handle.spawn(worker.shutdown());
                } else {
                    trace!("pool shutting down; no tokio runtime - killing eval worker via Drop");
                    drop(worker);
                }
            }
            Disposition::ReturnToIdle => {
                lock(&self.idle).push(worker);
            }
            Disposition::Kill => {
                debug!("discarding eval worker (unhealthy)");
                drop(worker);
            }
        }
    }
}

/// A cooldown between reaps, keeping a host under pressure out of a restart storm.
pub(super) async fn reap_loop(pool: Weak<EvalWorkerPool>, guard: MemoryGuard) {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    if !guard.is_active() {
        return;
    }
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut interval = tokio::time::interval(REAPER_INTERVAL);
    let mut last_reap: Option<Instant> = None;
    let mut reported_no_victim = false;
    loop {
        interval.tick().await;
        let Some(pool) = pool.upgrade() else {
            return;
        };

        sys.refresh_memory();
        let available = sys.available_memory();
        let candidates: Vec<(u32, u64)> = pool
            .live_pids()
            .into_iter()
            .filter_map(|pid| rss_of_pid(pid).map(|rss| (pid, rss)))
            .collect();
        let over_guard = pressured(&guard, &candidates, available);
        pool.note_pressure(over_guard);
        if !over_guard {
            reported_no_victim = false;
            continue;
        }
        if last_reap.is_some_and(|at| at.elapsed() < REAP_COOLDOWN) {
            continue;
        }

        let Some((pid, rss)) = victim(&guard, &candidates, available) else {
            if !reported_no_victim {
                reported_no_victim = true;
                debug!(
                    available_mb = available / (1024 * 1024),
                    candidates = candidates.len(),
                    "memory over the guard, but no eval subprocess is holding enough to recover \
                     it; not reaping"
                );
            }
            continue;
        };

        warn!(
            pid,
            rss_mb = rss / (1024 * 1024),
            available_mb = available / (1024 * 1024),
            "reaping the eval subprocess that can recover the memory guard"
        );
        if let Err(err) = kill(Pid::from_raw(pid as i32), Signal::SIGKILL) {
            debug!(pid, %err, "eval subprocess already gone before the reap");
        }
        last_reap = Some(Instant::now());
        reported_no_victim = false;
    }
}

/// Cap the pool at 16 workers, each subprocess with up to `max_rss` resident.
/// Cap the pool at 16 subprocesses, each holding up to `max_rss` resident.
pub fn default_workers() -> NonZeroUsize {
    let cores = std::thread::available_parallelism().map_or(4, NonZeroUsize::get);
    NonZeroUsize::new(cores.min(16)).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for peers by hand"
    )]

    use super::*;
    use std::time::Duration;
    use tokio::process::Command;

    fn fake_worker() -> EvalWorker {
        EvalWorker::from_command(Command::new("cat"), Arc::default()).expect("spawn cat")
    }

    async fn dead_worker() -> EvalWorker {
        let mut w = fake_worker();
        w.child_mut().start_kill().expect("kill cat");
        w.child_mut().wait().await.expect("reap cat");
        w
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    fn pool(workers: usize, guard: MemoryGuard) -> EvalWorkerPool {
        EvalWorkerPool::new(
            workers,
            2 * GIB,
            guard,
            SpawnConfig {
                cache_dir: String::new(),
                trace_dir: None,
                metrics: true,
                fetch_through_parent: false,
            },
            Arc::default(),
        )
    }

    #[test]
    fn disposition_for_every_state_pair() {
        assert_eq!(dispose(true, true), Disposition::GracefulShutdown);
        assert_eq!(dispose(false, true), Disposition::ReturnToIdle);
        assert_eq!(dispose(true, false), Disposition::Kill);
        assert_eq!(dispose(false, false), Disposition::Kill);
    }

    fn silent_worker() -> EvalWorker {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("cat >/dev/null");
        EvalWorker::from_command(cmd, Arc::default()).expect("spawn sh")
    }

    pub(in crate::pool) fn replying_worker(
        resp: &crate::ipc::EvalResponse,
        tag: &str,
    ) -> EvalWorker {
        replying_worker_times(resp, tag, 1)
    }

    pub(in crate::pool) fn replying_worker_times(
        resp: &crate::ipc::EvalResponse,
        tag: &str,
        times: usize,
    ) -> EvalWorker {
        let payload = crate::ipc::encode_response(resp).expect("encode response");
        let mut frame = u32::try_from(payload.len())
            .expect("frame fits")
            .to_le_bytes()
            .to_vec();
        frame.extend_from_slice(&payload);
        let frame = frame.repeat(times);
        let path = std::env::temp_dir().join(format!(
            "gradient-pool-test-{}-{tag}.frame",
            std::process::id()
        ));
        std::fs::write(&path, &frame).expect("write response frame");

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("cat '{}'; cat >/dev/null", path.display()));
        EvalWorker::from_command(cmd, Arc::default()).expect("spawn sh")
    }

    #[tokio::test]
    async fn mid_call_cancelled_worker_is_discarded_not_pooled() {
        let pool = pool(1, MemoryGuard::None);
        pool.push_for_test(silent_worker());

        let mut worker = pool.acquire().await.expect("acquire");
        let call = worker.plan(
            "repo".into(),
            vec![],
            vec![],
            &super::super::RefuseDependencies("a test"),
        );
        tokio::time::timeout(Duration::from_millis(200), call)
            .await
            .expect_err("a silent child must keep the call pending");
        drop(worker);

        assert_eq!(
            pool.idle_count(),
            0,
            "a worker dropped with a request in flight has an unread response \
             on the wire and must not be returned to the pool"
        );
    }

    #[tokio::test]
    async fn completed_call_worker_returns_to_idle() {
        use crate::ipc::EvalResponse;

        let pool = pool(1, MemoryGuard::None);
        pool.push_for_test(replying_worker(
            &EvalResponse::PlanOk {
                shards: vec![],
                errors: vec![],
            },
            "planok",
        ));

        let mut worker = pool.acquire().await.expect("acquire");
        let (shards, errors) = worker
            .plan(
                "repo".into(),
                vec![],
                vec![],
                &super::super::RefuseDependencies("a test"),
            )
            .await
            .expect("plan");
        assert!(shards.is_empty() && errors.is_empty());
        drop(worker);

        assert_eq!(
            pool.idle_count(),
            1,
            "a worker whose call completed in lockstep is reusable"
        );
    }

    #[tokio::test]
    async fn acquire_skips_dead_idle_worker() {
        let pool = pool(4, MemoryGuard::None);
        let live = fake_worker();
        let live_pid = live.pid();
        assert!(live_pid.is_some());

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
        lock(&live).insert(4242u32);
        {
            let _guard = PidGuard {
                live: Some(Arc::clone(&live)),
                pid: Some(4242),
            };
            assert!(lock(&live).contains(&4242));
        }
        assert!(
            !lock(&live).contains(&4242),
            "PidGuard must remove its pid from the live registry on drop"
        );
    }

    #[tokio::test]
    async fn release_idle_drains_but_keeps_pool_usable() {
        let pool = pool(2, MemoryGuard::None);
        pool.push_for_test(fake_worker());
        pool.push_for_test(fake_worker());
        assert_eq!(pool.idle_count(), 2);

        tokio::time::timeout(Duration::from_secs(6), pool.release_idle())
            .await
            .expect("release completes within the per-worker grace budget");
        assert_eq!(pool.idle_count(), 0, "release drains the idle workers");
        assert!(
            !pool.is_shutting_down(),
            "release must leave the pool usable (semaphore open)"
        );

        pool.push_for_test(fake_worker());
        let w = pool.acquire().await.expect("acquire after release");
        assert!(w.pid().is_some());
    }

    #[tokio::test]
    async fn shutdown_with_no_idle_workers_returns_immediately() {
        let pool = pool(2, MemoryGuard::None);
        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("shutdown should not hang on empty pool");
        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_idle_workers_gracefully() {
        let pool = pool(2, MemoryGuard::None);
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
        let pool = pool(2, MemoryGuard::None);
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
        let pool = Arc::new(pool(1, MemoryGuard::None));
        pool.push_for_test(fake_worker());

        let pooled = pool.acquire().await.expect("acquire");
        assert_eq!(pool.idle_count(), 0);

        let pool2 = Arc::clone(&pool);
        let shutdown = tokio::spawn(async move { pool2.shutdown().await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(pooled);

        tokio::time::timeout(Duration::from_secs(6), shutdown)
            .await
            .expect("shutdown timed out")
            .expect("shutdown task panicked");

        assert!(pool.is_shutting_down());
        assert_eq!(
            pool.idle_count(),
            0,
            "in-flight worker must not be returned to idle once pool is shutting down"
        );
    }

    #[tokio::test]
    async fn acquire_waits_only_while_a_guard_reports_pressure() {
        let pool = pool(2, MemoryGuard::MinFreeRam(1024));
        pool.push_for_test(fake_worker());
        pool.push_for_test(fake_worker());
        let _held = pool.acquire().await.expect("first permit");
        pool.note_pressure(true);

        let blocked = tokio::time::timeout(Duration::from_millis(200), pool.acquire()).await;
        assert!(
            blocked.is_err(),
            "a checkout under an active guard must back off"
        );

        pool.note_pressure(false);
        let next = tokio::time::timeout(Duration::from_secs(1), pool.acquire()).await;
        assert!(next.is_ok(), "clearing the pressure lets the pool go on");
    }

    #[tokio::test]
    async fn an_unguarded_pool_ignores_pressure() {
        let pool = pool(2, MemoryGuard::None);
        pool.push_for_test(fake_worker());
        pool.push_for_test(fake_worker());
        let _held = pool.acquire().await.expect("first permit");
        pool.note_pressure(true);

        let next = tokio::time::timeout(Duration::from_millis(500), pool.acquire()).await;
        assert!(next.is_ok(), "no guard means no backoff");
    }
}
