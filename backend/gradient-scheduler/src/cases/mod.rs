/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod abort;
mod aliases;
mod case_layer;
mod claims;
mod clusters;
mod control;
mod estimates;
mod eval_queue;
mod imports;
mod inputs;
mod invariants;
mod outside;
mod probe;
mod readers;
mod reports;
mod scenarios;
mod scoring;
mod session;
mod shapes;
mod triggers;
mod workers;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gradient_db::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_build;
use gradient_entity::ids::DispatchedJobId;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::Shape;
use gradient_graph::writer::GraphMsg;
use gradient_graph::{Graph, testing};
use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_test_support::state::TestState;
use gradient_types::ids::ProjectId;
use gradient_types::{BuildArgs, RuntimeConfig};
use gradient_util::shutdown::Shutdown;
use gradient_wire::types::GradientCapabilities;
use ractor::ActorRef;
use tempfile::NamedTempFile;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::jobs::PendingJob;
use crate::test_jobs;
use invariants::{Observed, Rows};

pub(crate) use crate::cluster::SlotKind;
pub(crate) use outside::{GitRepo, UpstreamServer};
pub(crate) use readers::{Totals, UpstreamTally, job_ids, planned_targets};
pub(crate) use session::{ClusterAssignment, Failure, failure};
pub(crate) use shapes::*;

const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const SETTLE_POLL: Duration = Duration::from_millis(10);

pub(crate) struct SchedulerCase {
    pub db: Arc<TestDatabase>,
    pub ctx: DbContext,
    started_at: DateTime<Utc>,
    shutdown: Shutdown,
    pub scheduler: Arc<Scheduler>,
    pub graph: Arc<Graph>,
    writer: Mutex<ActorRef<GraphMsg>>,
    core: Mutex<ActorRef<SchedulerMsg>>,
    sessions: Mutex<HashMap<String, Session>>,
    connected_workers: Mutex<HashSet<String>>,
    connected_on_any_instance: Arc<Mutex<HashSet<String>>>,
    observed: Mutex<Observed>,
    held_for_reclaim: Mutex<HashSet<String>>,
    unstored_outputs: AtomicI64,
    secret: Arc<NamedTempFile>,
    probe: crate::probe::UpstreamProbe,
}

struct Session {
    port: Arc<dyn SessionPort>,
    inbox: UnboundedReceiver<SessionSignal>,
    caps: GradientCapabilities,
    projects: HashSet<ProjectId>,
    last_seen: Arc<AtomicI64>,
    jobs: HashMap<String, SessionJob>,
}

#[derive(Clone)]
struct SessionJob {
    assignment_id: DispatchedJobId,
    job: PendingJob,
}

impl SchedulerCase {
    pub async fn start(shape: Shape) -> Self {
        Self::start_with(shape, |_| {}).await
    }

    pub async fn start_with(
        shape: Shape,
        config: impl FnOnce(&mut RuntimeConfig) + 'static,
    ) -> Self {
        let case = Self::start_seeded(shape, config).await;
        case.assert_drift("the seed").await;
        case
    }

    pub async fn start_with_unstored_outputs(shape: Shape, unstored: i64) -> Self {
        let case = Self::start_seeded(shape, |_| {}).await;
        case.set_unstored_outputs(unstored);
        case.assert_drift("the seed").await;
        case
    }

    pub fn set_unstored_outputs(&self, unstored: i64) {
        self.unstored_outputs.store(unstored, Ordering::SeqCst);
    }

    async fn assert_drift(&self, after: &str) {
        let unstored = self.unstored_outputs.load(Ordering::SeqCst);
        testing::assert_only_unstored_outputs(testing::drift(&self.ctx).await, unstored, after);
    }

    async fn start_seeded(shape: Shape, config: impl FnOnce(&mut RuntimeConfig) + 'static) -> Self {
        let started_at = Utc::now();
        let declared = shape.declared_statuses();
        let db = shape.into_database().await;
        let (secret, _) = testing::secret_file();
        let secret = Arc::new(secret);
        let graph = Graph::new();
        let instance = instance(&db, &graph, started_at, &secret, config);

        testing::settle_seed(&instance.ctx, &db, declared).await;
        enter_queued_builds(&instance.ctx, &db, None).await;

        Self::spawn(Arc::new(db), instance, graph, secret, Arc::default()).await
    }

    pub async fn second_instance(&self) -> SchedulerCase {
        let config = RuntimeConfig::clone(&self.ctx.config);
        let graph = Graph::new();
        let instance = instance(&self.db, &graph, Utc::now(), &self.secret, move |c| {
            *c = config;
        });
        Self::spawn(
            Arc::clone(&self.db),
            instance,
            graph,
            Arc::clone(&self.secret),
            Arc::clone(&self.connected_on_any_instance),
        )
        .await
    }

    pub fn signing_key() -> String {
        let (secret, _) = testing::secret_file();
        testing::signing_key(&secret.path().to_string_lossy())
    }

    async fn spawn(
        db: Arc<TestDatabase>,
        instance: Instance,
        graph: Arc<Graph>,
        secret: Arc<NamedTempFile>,
        connected_on_any_instance: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let Instance {
            ctx,
            started_at,
            shutdown,
            scheduler,
        } = instance;
        let writer = graph
            .spawn(ctx.clone(), None, None)
            .await
            .expect("the graph writer starts");
        let core = scheduler
            .spawn_core(None)
            .await
            .expect("the scheduler core starts");
        let observed = Mutex::new(Observed::of(&Rows::read(&db).await));
        let probe = crate::probe::UpstreamProbe::new(&ctx, &graph, &scheduler.upstream_query);

        Self {
            db,
            ctx,
            started_at,
            shutdown,
            scheduler,
            graph,
            writer: Mutex::new(writer),
            core: Mutex::new(core),
            sessions: Mutex::default(),
            connected_workers: Mutex::default(),
            connected_on_any_instance,
            observed,
            held_for_reclaim: Mutex::default(),
            unstored_outputs: AtomicI64::new(0),
            secret,
            probe,
        }
    }

    pub async fn seed(&self, shape: Shape) {
        let declared = shape.declared_statuses();
        shape.insert_into(&self.db.connection()).await;
        enter_queued_builds(&self.ctx, &self.db, Some(declared)).await;
    }

    pub fn signals(&self, worker: &str) -> Vec<SessionSignal> {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(worker) else {
            return Vec::new();
        };
        std::iter::from_fn(|| session.inbox.try_recv().ok()).collect()
    }

    fn workers_of_other_instances(&self) -> HashSet<String> {
        let own = lock(&self.connected_workers).clone();
        lock(&self.connected_on_any_instance)
            .iter()
            .filter(|worker| !own.contains(*worker))
            .cloned()
            .collect()
    }

    pub async fn settled(&self) {
        let shutdown = &self.shutdown;
        let drained = tokio::time::timeout(SETTLE_TIMEOUT, async {
            while shutdown.pending() > 0 {
                tokio::time::sleep(SETTLE_POLL).await;
            }
        })
        .await;
        assert!(
            drained.is_ok(),
            "detached tasks still running after {SETTLE_TIMEOUT:?}: {}",
            shutdown.pending()
        );
    }

    pub async fn check(&self, after: &str) {
        self.check_rows(after).await;
        self.check_core(after).await;
    }

    async fn check_rows(&self, after: &str) {
        self.settled().await;
        self.assert_drift(after).await;

        let rows = Rows::read(&self.db).await;
        rows.assert_invariants(after, &self.held_for_reclaim().await);
        lock(&self.observed).advance(&rows, after);
        let connected = lock(&self.connected_workers).clone();
        rows.s7_no_open_row_before_start(&connected, self.started_at.naive_utc(), after);
    }

    async fn held_for_reclaim(&self) -> HashSet<String> {
        match self
            .scheduler
            .call(|reply| SchedulerMsg::TrackedKeys { reply })
            .await
        {
            Ok(tracked) => {
                let held: HashSet<String> = tracked.awaiting_reclaim.into_iter().collect();
                lock(&self.held_for_reclaim).clone_from(&held);
                held
            }
            Err(_) => lock(&self.held_for_reclaim).clone(),
        }
    }

    async fn check_core(&self, after: &str) {
        self.settled().await;
        let tracked = self
            .scheduler
            .call(|reply| SchedulerMsg::TrackedKeys { reply })
            .await
            .expect("the scheduler core lists its job keys");
        invariants::s11_a_single_job_list_per_key(&tracked, after);

        let rows = Rows::read(&self.db).await;
        rows.s8_open_builds_within_slots(&self.scheduler.board_workers().await, after);
        rows.s12_no_open_job_pending(&self.scheduler, &self.workers_of_other_instances(), after)
            .await;
    }

    pub async fn finish(self) {
        self.check_core("the last step").await;
        let core = lock(&self.core).clone();
        core.stop_and_wait(None, None)
            .await
            .expect("the scheduler core stops");
        let writer = lock(&self.writer).clone();
        writer
            .stop_and_wait(None, None)
            .await
            .expect("the graph writer stops");
        self.check_rows("the last step").await;

        let Self {
            db,
            ctx,
            scheduler,
            graph,
            probe,
            ..
        } = self;
        drop((ctx, scheduler, graph, probe));
        drop_database_if_last(db).await;
    }
}

struct Instance {
    ctx: DbContext,
    started_at: DateTime<Utc>,
    shutdown: Shutdown,
    scheduler: Arc<Scheduler>,
}

fn instance(
    db: &TestDatabase,
    graph: &Arc<Graph>,
    started_at: DateTime<Utc>,
    secret: &NamedTempFile,
    config: impl FnOnce(&mut RuntimeConfig) + 'static,
) -> Instance {
    let crypt_file = secret.path().to_string_lossy().into_owned();
    let state = TestState::new()
        .worker_db(db.connection())
        .web_db(db.connection())
        .cache_db(db.connection())
        .cli(|cli| {
            cli.secrets.crypt_file = crypt_file;
            cli.build = BuildArgs::default();
        })
        .graph(Arc::clone(graph))
        .started_at(started_at)
        .config(config)
        .build();

    Instance {
        ctx: state.db(),
        started_at: state.started_at,
        shutdown: state.shutdown.clone(),
        scheduler: Arc::new(Scheduler::new(state)),
    }
}

async fn enter_queued_builds(
    ctx: &DbContext,
    db: &TestDatabase,
    declared: Option<Vec<(String, BuildStatus)>>,
) {
    let only = match declared {
        Some(declared) => {
            let mut ids = HashSet::new();
            for (path, _) in declared {
                ids.insert(db.id_of(&path).await);
            }
            Some(ids)
        }
        None => None,
    };
    let queued = db
        .rows::<derivation_build::Entity>()
        .await
        .into_iter()
        .filter(|build| build.status == BuildStatus::Queued)
        .map(|build| build.derivation)
        .filter(|derivation| only.as_ref().is_none_or(|ids| ids.contains(derivation)));
    ctx.memory.startable_set.enter(queued);
}

async fn drop_database_if_last(db: Arc<TestDatabase>) {
    if let Ok(db) = Arc::try_unwrap(db) {
        db.finish().await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn without_offers(signals: Vec<SessionSignal>) -> Vec<SessionSignal> {
    signals
        .into_iter()
        .filter(|signal| !matches!(signal, SessionSignal::Offers(_)))
        .collect()
}

fn new_port() -> (Arc<dyn SessionPort>, UnboundedReceiver<SessionSignal>) {
    test_jobs::port()
}
