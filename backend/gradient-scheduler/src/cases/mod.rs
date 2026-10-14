/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod case_layer;
mod invariants;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gradient_core::ServerState;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_build;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::Shape;
use gradient_graph::writer::GraphMsg;
use gradient_graph::{Graph, testing};
use gradient_pool::session_port::SessionSignal;
use gradient_test_support::state::TestState;
use gradient_types::RuntimeConfig;
use gradient_types::ids::ProjectId;
use gradient_wire::types::GradientCapabilities;
use ractor::ActorRef;
use tempfile::NamedTempFile;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::Scheduler;
use crate::actor::SchedulerMsg;
use crate::test_jobs;
use invariants::{Observed, Rows};

const SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const SETTLE_POLL: Duration = Duration::from_millis(10);

pub(crate) struct SchedulerCase {
    pub db: Arc<TestDatabase>,
    pub state: Arc<ServerState>,
    pub scheduler: Arc<Scheduler>,
    pub graph: Arc<Graph>,
    writer: ActorRef<GraphMsg>,
    core: ActorRef<SchedulerMsg>,
    inboxes: Mutex<HashMap<String, UnboundedReceiver<SessionSignal>>>,
    connected_workers: Mutex<HashSet<String>>,
    observed: Mutex<Observed>,
    secret: Arc<NamedTempFile>,
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
        enter_seeded_queued_builds(&state, &db).await;

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

        Self {
            db,
            state,
            scheduler,
            graph,
            writer,
            core,
            inboxes: Mutex::default(),
            connected_workers: Mutex::default(),
            observed,
            secret,
        }
    }

    pub async fn connect(&self, worker: &str, caps: GradientCapabilities, projects: &[ProjectId]) {
        let (session, inbox) = test_jobs::port();
        self.scheduler
            .register_worker(worker, caps, projects.iter().copied().collect(), session)
            .await
            .expect("the worker registers");
        lock(&self.inboxes).insert(worker.to_owned(), inbox);
        lock(&self.connected_workers).insert(worker.to_owned());
    }

    pub fn signals(&self, worker: &str) -> Vec<SessionSignal> {
        let mut inboxes = lock(&self.inboxes);
        let inbox = inboxes
            .get_mut(worker)
            .unwrap_or_else(|| panic!("connect {worker} first"));
        std::iter::from_fn(|| inbox.try_recv().ok()).collect()
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
        self.core
            .stop_and_wait(None, None)
            .await
            .expect("the scheduler core stops");
        self.writer
            .stop_and_wait(None, None)
            .await
            .expect("the graph writer stops");
        self.check_rows("the last step").await;

        let Self {
            db,
            state,
            scheduler,
            graph,
            ..
        } = self;
        drop((state, scheduler, graph));
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

async fn enter_seeded_queued_builds(state: &ServerState, db: &TestDatabase) {
    let queued = db
        .rows::<derivation_build::Entity>()
        .await
        .into_iter()
        .filter(|build| build.status == BuildStatus::Queued)
        .map(|build| build.derivation);
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
