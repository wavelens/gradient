/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![allow(
    dead_code,
    unused_imports,
    reason = "the theme files of the scheduler cases call the shared layer"
)]

mod abort;
mod aliases;
mod case_layer;
mod claims;
mod clusters;
mod control;
mod eval_queue;
mod imports;
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
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_build;
use gradient_entity::ids::DispatchedJobId;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::Shape;
use gradient_graph::writer::GraphMsg;
use gradient_graph::{Graph, testing};
use gradient_pool::session_port::{SessionPort, SessionSignal};
use gradient_test_support::state::TestState;
use gradient_types::RuntimeConfig;
use gradient_types::ids::ProjectId;
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
pub(crate) use readers::{Totals, UpstreamTally, planned_targets};
pub(crate) use session::{ClusterAssignment, Failure, failure};
pub(crate) use shapes::*;

const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const SETTLE_POLL: Duration = Duration::from_millis(10);

pub(crate) struct SchedulerCase {
    pub db: Arc<TestDatabase>,
    pub state: Arc<ServerState>,
    pub scheduler: Arc<Scheduler>,
    pub graph: Arc<Graph>,
    writer: Mutex<ActorRef<GraphMsg>>,
    core: Mutex<ActorRef<SchedulerMsg>>,
    sessions: Mutex<HashMap<String, Session>>,
    connected_workers: Mutex<HashSet<String>>,
    observed: Mutex<Observed>,
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
        let started_at = Utc::now();
        let declared = shape.declared_statuses();
        let db = shape.into_database().await;
        let (secret, _) = testing::secret_file();
        let secret = Arc::new(secret);
        let graph = Graph::new();
        let state = server_state(&db, &graph, started_at, &secret, config);

        testing::settle_seed(&state.db(), &db, declared).await;
        enter_queued_builds(&state, &db, None).await;

        let case = Self::spawn(Arc::new(db), state, graph, secret).await;
        testing::zero_drift(&case.state.db(), "the seed").await;
        case
    }

    pub async fn second_instance(&self) -> SchedulerCase {
        let config = RuntimeConfig::clone(&self.state.config);
        let graph = Graph::new();
        let state = server_state(&self.db, &graph, Utc::now(), &self.secret, move |c| {
            *c = config;
        });
        Self::spawn(Arc::clone(&self.db), state, graph, Arc::clone(&self.secret)).await
    }

    async fn spawn(
        db: Arc<TestDatabase>,
        state: Arc<ServerState>,
        graph: Arc<Graph>,
        secret: Arc<NamedTempFile>,
    ) -> Self {
        let writer = graph
            .spawn(state.db(), None, None)
            .await
            .expect("the graph writer starts");
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&state)));
        let core = scheduler
            .spawn_core(None)
            .await
            .expect("the scheduler core starts");
        let observed = Mutex::new(Observed::of(&Rows::read(&db).await));
        let probe =
            crate::probe::UpstreamProbe::new(&state.db(), &graph, &scheduler.upstream_query);

        Self {
            db,
            state,
            scheduler,
            graph,
            writer: Mutex::new(writer),
            core: Mutex::new(core),
            sessions: Mutex::default(),
            connected_workers: Mutex::default(),
            observed,
            secret,
            probe,
        }
    }

    pub async fn seed(&self, shape: Shape) {
        let declared = shape.declared_statuses();
        shape.insert_into(&self.db.connection()).await;
        enter_queued_builds(&self.state, &self.db, Some(declared)).await;
    }

    pub fn signals(&self, worker: &str) -> Vec<SessionSignal> {
        let mut sessions = lock(&self.sessions);
        let Some(session) = sessions.get_mut(worker) else {
            return Vec::new();
        };
        std::iter::from_fn(|| session.inbox.try_recv().ok()).collect()
    }

    pub async fn settled(&self) {
        let shutdown = &self.state.shutdown;
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
        testing::zero_drift(&self.state.db(), after).await;

        let rows = Rows::read(&self.db).await;
        rows.assert_invariants(after);
        lock(&self.observed).advance(&rows, after);
        let connected = lock(&self.connected_workers).clone();
        rows.s7_no_open_row_before_start(&connected, self.state.started_at.naive_utc(), after);
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
        rows.s12_no_open_job_pending(&self.scheduler, after).await;
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
            state,
            scheduler,
            graph,
            probe,
            ..
        } = self;
        drop((state, scheduler, graph, probe));
        drop_database_if_last(db).await;
    }
}

fn server_state(
    db: &TestDatabase,
    graph: &Arc<Graph>,
    started_at: DateTime<Utc>,
    secret: &NamedTempFile,
    config: impl FnOnce(&mut RuntimeConfig) + 'static,
) -> Arc<ServerState> {
    let crypt_file = secret.path().to_string_lossy().into_owned();
    TestState::new()
        .worker_db(db.connection())
        .web_db(db.connection())
        .cache_db(db.connection())
        .cli(|cli| cli.secrets.crypt_file = crypt_file)
        .graph(Arc::clone(graph))
        .started_at(started_at)
        .config(config)
        .build()
}

async fn enter_queued_builds(
    state: &ServerState,
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
    state.memory.db.startable_set.enter(queued);
}

async fn drop_database_if_last(db: Arc<TestDatabase>) {
    if let Ok(db) = Arc::try_unwrap(db) {
        db.finish().await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn new_port() -> (Arc<dyn SessionPort>, UnboundedReceiver<SessionSignal>) {
    test_jobs::port()
}
