/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use futures::stream::{FuturesUnordered, StreamExt};
use gradient_util::sync::Mutex;
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, trace};

use super::live_thunks::LiveThunks;
use super::transport::EvalWorker;

const PRESSURE_BACKOFF: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(super) struct EvalWorkerPool {
    idle: Arc<Mutex<Vec<EvalWorker>>>,
    semaphore: Arc<Semaphore>,
    max: usize,
    max_eval_rss: u64,
    eval_cache_dir: String,
    shutting_down: Arc<AtomicBool>,
    live: Arc<Mutex<HashSet<u32>>>,
    min_free_bytes: AtomicU64,
    under_pressure: Arc<AtomicBool>,
    thunks: Arc<LiveThunks>,
}

impl EvalWorkerPool {
    pub(super) fn new(
        max: usize,
        max_eval_rss: u64,
        eval_cache_dir: String,
        thunks: Arc<LiveThunks>,
    ) -> Self {
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
            thunks,
        }
    }

    pub(super) fn max(&self) -> usize {
        self.max
    }

    pub(super) fn max_eval_rss(&self) -> u64 {
        self.max_eval_rss
    }

    pub(super) fn configure_memory_guard(&self, min_free_bytes: u64) {
        self.min_free_bytes.store(min_free_bytes, Ordering::Relaxed);
    }

    pub(super) fn live_pids(&self) -> Vec<u32> {
        self.live.lock().iter().copied().collect()
    }

    pub(super) fn note_pressure(&self, pressured: bool) {
        self.under_pressure.store(pressured, Ordering::Relaxed);
    }

    pub(super) async fn acquire(&self) -> Result<PooledEvalWorker> {
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("EvalWorkerPool semaphore closed"))?;

        while self.min_free_bytes.load(Ordering::Relaxed) > 0
            && self.under_pressure.load(Ordering::Relaxed)
            && self.semaphore.available_permits() + 1 < self.max
        {
            tokio::time::sleep(PRESSURE_BACKOFF).await;
        }

        let worker = loop {
            let candidate = {
                let mut idle = self.idle.lock();
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
                        &self.eval_cache_dir,
                        Arc::clone(&self.live),
                        Arc::clone(&self.thunks),
                    )
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

    pub(super) async fn shutdown(&self) {
        // The flag must be set before closing the semaphore.
        // An in-flight `acquire()` could otherwise fail and skip `PooledEvalWorker::drop`.
        self.shutting_down.store(true, Ordering::SeqCst);
        self.semaphore.close();
        self.release_idle().await;
    }

    pub(super) async fn release_idle(&self) {
        let drained: Vec<EvalWorker> = {
            let mut idle = self.idle.lock();
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
        self.idle.lock().len()
    }

    #[cfg(test)]
    pub(super) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn push_for_test(&self, worker: EvalWorker) {
        self.idle.lock().push(worker);
    }
}

/// The idle subprocess holding the least memory goes first.
/// Growth then spreads over the pool instead of piling up in the subprocess returned last.
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

        // A cancelled caller is leaving its request in flight.
        // The unread response would answer the next request.
        // Such a worker is never healthy, whatever the caller observed.
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
                self.idle.lock().push(worker);
            }
            Disposition::Kill => {
                debug!("discarding eval worker (unhealthy)");
                drop(worker);
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(
        clippy::disallowed_methods,
        reason = "tests stand in for their peers by hand"
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

    #[test]
    fn disposition_covers_all_states() {
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

    pub(in super::super) fn replying_worker(
        resp: &gradient_eval::ipc::EvalResponse,
        tag: &str,
    ) -> EvalWorker {
        let payload = gradient_eval::ipc::encode_response(resp).expect("encode response");
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
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
        let pool = EvalWorkerPool::new(1, 2 * GIB, String::new(), Arc::default());
        pool.push_for_test(silent_worker());

        let mut worker = pool.acquire().await.expect("acquire");
        let call = worker.plan(
            "repo".into(),
            vec![],
            vec![],
            &gradient_sources::RefuseImports("a test"),
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
        use gradient_eval::ipc::EvalResponse;

        let pool = EvalWorkerPool::new(1, 2 * GIB, String::new(), Arc::default());
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
                &gradient_sources::RefuseImports("a test"),
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
        let pool = EvalWorkerPool::new(4, 2 * GIB, String::new(), Arc::default());
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
        live.lock().insert(4242u32);
        {
            let _guard = PidGuard {
                live: Some(Arc::clone(&live)),
                pid: Some(4242),
            };
            assert!(live.lock().contains(&4242));
        }
        assert!(
            !live.lock().contains(&4242),
            "PidGuard must remove its pid from the live registry on drop"
        );
    }

    #[tokio::test]
    async fn release_idle_drains_but_keeps_pool_usable() {
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new(), Arc::default());
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
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new(), Arc::default());
        tokio::time::timeout(Duration::from_secs(1), pool.shutdown())
            .await
            .expect("shutdown should not hang on empty pool");
        assert!(pool.is_shutting_down());
        assert_eq!(pool.idle_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_drains_idle_workers_gracefully() {
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new(), Arc::default());
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
        let pool = EvalWorkerPool::new(2, 2 * GIB, String::new(), Arc::default());
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
            2 * GIB,
            String::new(),
            Arc::default(),
        ));
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
}
