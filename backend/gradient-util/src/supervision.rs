/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Supervision tree for the server's long-lived loops.
//!
//! A `Root` actor owns every loop as a linked child. A child that panics or
//! exits unexpectedly is respawned after an exponential backoff; a child whose
//! pass exceeds its budget is cancelled in place and ticks again. Shutdown stops
//! the whole tree through the shared [`Shutdown`] token.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ractor::{
    Actor, ActorCell, ActorId, ActorProcessingErr, ActorRef, RpcReplyPort, SpawnErr,
    SupervisionEvent,
};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

pub type PassError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type PassFuture = Pin<Box<dyn Future<Output = Result<(), PassError>> + Send>>;
pub type PassFn = Arc<dyn Fn() -> PassFuture + Send + Sync>;
pub type SpawnFuture = Pin<Box<dyn Future<Output = Result<ActorCell, SpawnErr>> + Send>>;
pub type SpawnFn = Arc<dyn Fn(ChildCtx) -> SpawnFuture + Send + Sync>;

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const HEALTHY_RESET: Duration = Duration::from_secs(300);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A pass that runs every `period`, cancelled in place past `budget`.
#[derive(Clone)]
pub struct PeriodicSpec {
    pub name: &'static str,
    pub period: Duration,
    pub budget: Duration,
    pub run: PassFn,
}

/// What the root supervises: a periodic pass, or any actor spawned by a factory.
#[derive(Clone)]
pub enum ChildSpec {
    Periodic(PeriodicSpec),
    Custom { name: &'static str, spawn: SpawnFn },
}

impl ChildSpec {
    pub fn periodic<F, Fut>(name: &'static str, period: Duration, budget: Duration, run: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), PassError>> + Send + 'static,
    {
        Self::Periodic(PeriodicSpec {
            name,
            period,
            budget,
            run: Arc::new(move || Box::pin(run())),
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Periodic(p) => p.name,
            Self::Custom { name, .. } => name,
        }
    }
}

/// Everything a child needs from the tree: its parent cell, the shared health
/// registry and the shutdown token.
#[derive(Clone)]
pub struct ChildCtx {
    pub parent: ActorCell,
    pub health: Arc<SupervisorHealth>,
    pub cancel: CancellationToken,
}

#[derive(Clone, Debug, Default)]
pub struct LoopHealth {
    pub restarts: u32,
    pub pass_errors: u64,
    pub pass_timeouts: u64,
    pub last_ok_at: Option<Instant>,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct SupervisorHealth {
    loops: Mutex<HashMap<&'static str, LoopHealth>>,
}

impl SupervisorHealth {
    /// Give `name` a row so the snapshot lists a child before its first pass.
    pub fn register(&self, name: &'static str) {
        self.with(name, |_| {});
    }

    pub fn with(&self, name: &'static str, f: impl FnOnce(&mut LoopHealth)) {
        let mut map = self.loops.lock().unwrap_or_else(|e| e.into_inner());
        f(map.entry(name).or_default());
    }

    pub fn snapshot(&self) -> Vec<(&'static str, LoopHealth)> {
        let map = self.loops.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<_> = map.iter().map(|(k, v)| (*k, v.clone())).collect();
        rows.sort_by_key(|(k, _)| *k);
        rows
    }

    pub fn get(&self, name: &str) -> LoopHealth {
        let map = self.loops.lock().unwrap_or_else(|e| e.into_inner());
        map.get(name).cloned().unwrap_or_default()
    }
}

/// Run one pass under the shutdown token and the budget, recording the outcome.
/// Returns `false` when shutdown was observed, so the caller stops instead of rescheduling.
pub async fn run_pass(
    name: &'static str,
    budget: Duration,
    cancel: &CancellationToken,
    health: &SupervisorHealth,
    pass: PassFuture,
) -> bool {
    let started = Instant::now();
    let outcome = tokio::select! {
        _ = cancel.cancelled() => return false,
        r = tokio::time::timeout(budget, pass) => r,
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match outcome {
        Ok(Ok(())) => health.with(name, |h| h.last_ok_at = Some(Instant::now())),
        Ok(Err(e)) => {
            warn!(loop_name = name, elapsed_ms, error = %e, "pass failed");
            health.with(name, |h| {
                h.pass_errors += 1;
                h.last_error = Some(e.to_string());
            });
        }
        Err(_) => {
            warn!(
                loop_name = name,
                elapsed_ms, "pass exceeded its budget and was cancelled"
            );
            health.with(name, |h| h.pass_timeouts += 1);
        }
    }
    true
}

pub struct Periodic;

pub enum PeriodicMsg {
    Tick,
}

pub struct PeriodicArgs {
    pub spec: PeriodicSpec,
    pub ctx: ChildCtx,
}

impl Actor for Periodic {
    type Msg = PeriodicMsg;
    type State = PeriodicArgs;
    type Arguments = PeriodicArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        myself.send_after(args.spec.period, || PeriodicMsg::Tick);
        Ok(args)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        PeriodicMsg::Tick: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let spec = &state.spec;
        let alive = run_pass(
            spec.name,
            spec.budget,
            &state.ctx.cancel,
            &state.ctx.health,
            (spec.run)(),
        )
        .await;
        if alive {
            myself.send_after(spec.period, || PeriodicMsg::Tick);
        } else {
            myself.stop(Some("shutdown".into()));
        }
        Ok(())
    }
}

pub struct Root;

pub enum RootMsg {
    Add(ChildSpec, RpcReplyPort<Result<(), String>>),
    Respawn(&'static str),
}

pub struct RootArgs {
    pub children: Vec<ChildSpec>,
    pub health: Arc<SupervisorHealth>,
    pub cancel: CancellationToken,
}

pub struct RootState {
    args: RootArgs,
    live: HashMap<ActorId, &'static str>,
    failures: HashMap<&'static str, (u32, Instant)>,
}

impl RootState {
    fn spec(&self, name: &str) -> Option<ChildSpec> {
        self.args
            .children
            .iter()
            .find(|c| c.name() == name)
            .cloned()
    }

    fn backoff(&mut self, name: &'static str) -> Duration {
        let now = Instant::now();
        let entry = self.failures.entry(name).or_insert((0, now));
        if now.duration_since(entry.1) > HEALTHY_RESET {
            entry.0 = 0;
        }
        let delay = BACKOFF_BASE
            .saturating_mul(1u32 << entry.0.min(6))
            .min(BACKOFF_MAX);
        *entry = (entry.0.saturating_add(1), now);
        delay
    }
}

async fn spawn_child(
    myself: &ActorRef<RootMsg>,
    spec: &ChildSpec,
    state: &mut RootState,
) -> Result<(), SpawnErr> {
    let ctx = ChildCtx {
        parent: myself.get_cell(),
        health: Arc::clone(&state.args.health),
        cancel: state.args.cancel.clone(),
    };
    let cell = match spec {
        ChildSpec::Periodic(p) => {
            let args = PeriodicArgs {
                spec: p.clone(),
                ctx: ctx.clone(),
            };
            let (child, _) = Actor::spawn_linked(None, Periodic, args, ctx.parent).await?;
            child.get_cell()
        }
        ChildSpec::Custom { spawn, .. } => spawn(ctx).await?,
    };
    state.live.insert(cell.get_id(), spec.name());
    state.args.health.register(spec.name());
    info!(loop_name = spec.name(), "supervised loop started");
    Ok(())
}

impl Actor for Root {
    type Msg = RootMsg;
    type State = RootState;
    type Arguments = RootArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let mut state = RootState {
            args,
            live: HashMap::new(),
            failures: HashMap::new(),
        };
        for spec in state.args.children.clone() {
            spawn_child(&myself, &spec, &mut state).await?;
        }
        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            RootMsg::Add(spec, reply) => {
                let result = spawn_child(&myself, &spec, state)
                    .await
                    .map_err(|e| e.to_string());
                if result.is_ok() {
                    state.args.children.push(spec);
                }
                let _ = reply.send(result);
            }
            RootMsg::Respawn(name) => {
                if state.args.cancel.is_cancelled() {
                    return Ok(());
                }
                if let Some(spec) = state.spec(name) {
                    spawn_child(&myself, &spec, state).await?;
                }
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let (cell, reason) = match event {
            SupervisionEvent::ActorFailed(cell, err) => (cell, err.to_string()),
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                (cell, reason.unwrap_or_else(|| "exited".into()))
            }
            _ => return Ok(()),
        };
        let Some(name) = state.live.remove(&cell.get_id()) else {
            return Ok(());
        };
        if state.args.cancel.is_cancelled() {
            return Ok(());
        }
        let delay = state.backoff(name);
        warn!(loop_name = name, %reason, restart_in_ms = delay.as_millis() as u64, "supervised loop died; restarting");
        state.args.health.with(name, |h| {
            h.restarts += 1;
            h.last_error = Some(reason);
        });
        myself.send_after(delay, move || RootMsg::Respawn(name));
        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        myself
            .get_cell()
            .stop_children_and_wait(Some("shutdown".into()), Some(STOP_TIMEOUT))
            .await;
        Ok(())
    }
}

/// Handle to a running tree: the root and its health registry.
#[derive(Clone)]
pub struct Supervisor {
    root: ActorRef<RootMsg>,
    health: Arc<SupervisorHealth>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Spawn an empty root that stops when `token` is cancelled. The stop task
    /// is tracked so a drain waits for the tree.
    pub async fn start(token: CancellationToken, tracker: TaskTracker) -> Result<Self, SpawnErr> {
        let health = Arc::new(SupervisorHealth::default());
        let args = RootArgs {
            children: Vec::new(),
            health: Arc::clone(&health),
            cancel: token.clone(),
        };
        let (root, _) = Actor::spawn(None, Root, args).await?;
        let stop = root.clone();
        tracker.spawn(async move {
            token.cancelled().await;
            if let Err(e) = stop
                .stop_and_wait(Some("shutdown".into()), Some(STOP_TIMEOUT))
                .await
            {
                warn!(error = %e, "supervision tree did not stop cleanly");
            }
        });
        Ok(Self { root, health })
    }

    /// Add a child and wait until its first instance is running.
    pub async fn add(&self, spec: ChildSpec) -> Result<(), String> {
        match ractor::call!(self.root, RootMsg::Add, spec) {
            Ok(result) => result,
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn health(&self) -> Arc<SupervisorHealth> {
        Arc::clone(&self.health)
    }

    pub fn root_status(&self) -> ractor::ActorStatus {
        self.root.get_cell().get_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown::Shutdown;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    fn counting_pass(calls: Arc<AtomicU32>, panic_on: Option<u32>) -> ChildSpec {
        ChildSpec::periodic(
            "t",
            Duration::from_millis(10),
            Duration::from_millis(50),
            move || {
                let calls = Arc::clone(&calls);
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if panic_on == Some(n) {
                        panic!("boom on call {n}");
                    }
                    Ok(())
                }
            },
        )
    }

    #[tokio::test]
    async fn a_panicking_pass_is_restarted_and_keeps_ticking() {
        let shutdown = Shutdown::new();
        let calls = Arc::new(AtomicU32::new(0));
        shutdown
            .supervise_now(counting_pass(Arc::clone(&calls), Some(2)))
            .await
            .expect("supervise");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let h = shutdown.supervision_health().expect("tree").get("t");
        assert_eq!(h.restarts, 1, "one restart after the panic: {h:?}");
        assert!(
            calls.load(Ordering::SeqCst) > 3,
            "ticking resumed after the restart"
        );
        assert!(h.last_ok_at.is_some());
        shutdown.cancel_and_drain(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn a_child_is_listed_in_health_before_its_first_pass() {
        let shutdown = Shutdown::new();
        let spec = ChildSpec::periodic(
            "hourly",
            Duration::from_secs(3600),
            Duration::from_secs(60),
            || async { Ok(()) },
        );
        shutdown.supervise_now(spec).await.expect("supervise");
        let health = shutdown.supervision_health().expect("tree");
        let names: Vec<_> = health.snapshot().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["hourly"]);
        let h = health.get("hourly");
        assert!(h.last_ok_at.is_none() && h.restarts == 0, "{h:?}");
        shutdown.cancel_and_drain(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn a_pass_past_its_budget_is_cancelled_and_counted() {
        let shutdown = Shutdown::new();
        let spec = ChildSpec::periodic(
            "slow",
            Duration::from_millis(10),
            Duration::from_millis(20),
            || async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            },
        );
        shutdown.supervise_now(spec).await.expect("supervise");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let h = shutdown.supervision_health().expect("tree").get("slow");
        assert!(h.pass_timeouts >= 2, "{h:?}");
        assert_eq!(h.restarts, 0);
        shutdown.cancel_and_drain(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn shutdown_stops_the_tree_and_never_restarts() {
        let shutdown = Shutdown::new();
        let calls = Arc::new(AtomicU32::new(0));
        shutdown
            .supervise_now(counting_pass(Arc::clone(&calls), None))
            .await
            .expect("supervise");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(shutdown.cancel_and_drain(Duration::from_secs(2)).await);
        let after = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            after,
            "no ticks after shutdown"
        );
        assert_eq!(
            shutdown.tree().expect("tree").root_status(),
            ractor::ActorStatus::Stopped
        );
    }

    #[tokio::test]
    async fn supervise_from_a_sync_context_lands_in_the_tree() {
        let shutdown = Shutdown::new();
        let calls = Arc::new(AtomicU32::new(0));
        shutdown.supervise(counting_pass(Arc::clone(&calls), None));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(calls.load(Ordering::SeqCst) > 0, "the loop ticked");
        assert_eq!(
            shutdown
                .supervision_health()
                .expect("tree")
                .snapshot()
                .len(),
            1
        );
        shutdown.cancel_and_drain(Duration::from_secs(2)).await;
    }

    struct Slow;

    enum SlowMsg {
        Work,
        Ping(ractor::RpcReplyPort<u64>),
    }

    impl Actor for Slow {
        type Msg = SlowMsg;
        type State = Arc<AtomicU64>;
        type Arguments = Arc<AtomicU64>;

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            done: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(done)
        }

        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            msg: Self::Msg,
            done: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            if let SlowMsg::Ping(reply) = msg {
                let _ = reply.send(n);
            }
            Ok(())
        }
    }

    /// The mailbox is unbounded: a burst of casts is accepted instantly and a
    /// later call waits behind the whole backlog (FIFO, no priority lanes).
    #[tokio::test]
    async fn casts_queue_without_bound_and_a_call_waits_behind_them() {
        let done = Arc::new(AtomicU64::new(0));
        let (actor, _) = Actor::spawn(None, Slow, Arc::clone(&done))
            .await
            .expect("spawn");
        let burst = 2000u64;
        let started = Instant::now();
        for _ in 0..burst {
            actor.send_message(SlowMsg::Work).expect("cast");
        }
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "casts never wait"
        );
        let queued = burst - done.load(Ordering::SeqCst);
        assert!(
            queued > burst / 2,
            "{queued} of {burst} still queued after the burst"
        );
        let processed = ractor::call!(actor, SlowMsg::Ping).expect("call");
        assert_eq!(
            processed,
            burst + 1,
            "the call was answered only after the backlog"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(1000),
            "head-of-line wait behind the burst"
        );
        actor.stop(None);
    }

    #[tokio::test]
    async fn backoff_grows_and_caps() {
        let cancel = CancellationToken::new();
        let mut st = RootState {
            args: RootArgs {
                children: vec![],
                health: Arc::default(),
                cancel,
            },
            live: HashMap::new(),
            failures: HashMap::new(),
        };
        let delays: Vec<u64> = (0..8).map(|_| st.backoff("x").as_secs()).collect();
        assert_eq!(delays, vec![1, 2, 4, 8, 16, 32, 60, 60]);
    }
}
