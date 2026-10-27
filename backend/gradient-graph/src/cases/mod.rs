/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod batch;
mod exec;
mod failure;
mod gc;
mod nar;
mod need;
mod order;
mod passthrough;
mod probe;
mod queue;
mod realisation;
mod retire;
mod scenarios;
mod sharing;
mod signing;
mod transitions;
mod verdicts;
mod walk;
mod writer;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gradient_db::evaluations::counters::eval_counters;
use gradient_db::graph::consistency::ConsistencyReport;
use gradient_db::{DbContext, DbMemory, ProbeRequests};
use gradient_entity::StorePathMapping;
use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{
    CacheId, CachedPathSignatureId, DerivationBuildId, DerivationId, DispatchedJobId, EvaluationId,
};
use gradient_entity::{
    build_attempt, build_job, cache, cached_path, cached_path_signature, derivation,
    derivation_build, derivation_dependency, derivation_output, dispatched_job, entry_point,
    evaluation,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::{Shape, build_assignment, build_job_key};
use gradient_fixtures::test_date;
use gradient_types::RuntimeConfig;
use gradient_wire::types::{BuildFailureKind, DiscoveredDerivation};
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::messages::{
    DemoteReport, Demotion, GcReport, GcRequest, NarCommit, NarCommitted, OrphanCause, RamShortage,
    RequeueScope, SignTargets, Transition, TransitionReport, UpstreamHit,
};
use crate::test_ctx::{
    Batch, BatchReport, apply_in, claim_build, ctx_with_crypt_file, secret_file, send, signing_key,
    worker_loss,
};
use crate::writer::{GraphMsg, transact};
use crate::{Graph, testing};

pub(crate) use shared::*;

pub(crate) type GraphRows = BTreeMap<&'static str, Vec<String>>;

pub(crate) struct Case {
    pub ctx: DbContext,
    pub db: TestDatabase,
    pub graph: Arc<Graph>,
    writer: ractor::ActorRef<GraphMsg>,
    probe_inbox: Mutex<UnboundedReceiver<Vec<DerivationId>>>,
    claims: Mutex<HashMap<DerivationBuildId, (DispatchedJobId, bool)>>,
}

thread_local! {
    static SECRET: (tempfile::NamedTempFile, String) = secret_file();
}

impl Case {
    pub fn key() -> String {
        SECRET.with(|(_, path)| signing_key(path))
    }

    pub async fn start(shape: Shape) -> Self {
        let (case, report) = Self::start_with_drift(shape).await;
        assert_eq!(report.total(), 0, "drift after the seed: {report:?}");
        case
    }

    pub async fn start_with_unstored_outputs(shape: Shape, unstored: i64) -> Self {
        let (case, report) = Self::start_with_drift(shape).await;
        assert_only_unstored_outputs(report, unstored, "the seed");
        case
    }

    pub async fn start_with_mapping(shape: Shape, mapping: Arc<dyn StorePathMapping>) -> Self {
        let (case, report) = Self::start_configured(shape, Some(mapping), |_| {}).await;
        assert_eq!(report.total(), 0, "drift after the seed: {report:?}");
        case
    }

    pub async fn start_without_exec_jobs(shape: Shape) -> Self {
        let (case, report) =
            Self::start_configured(shape, None, |config| config.exec.enable = false).await;
        assert_eq!(report.total(), 0, "drift after the seed: {report:?}");
        case
    }

    pub async fn start_with_drift(shape: Shape) -> (Self, ConsistencyReport) {
        Self::start_configured(shape, None, |_| {}).await
    }

    async fn start_configured(
        shape: Shape,
        mapping: Option<Arc<dyn StorePathMapping>>,
        configure: impl FnOnce(&mut RuntimeConfig),
    ) -> (Self, ConsistencyReport) {
        let declared = shape.declared_statuses();
        let secret = SECRET.with(|(_, path)| path.clone());
        let db = shape.into_database().await;
        let probe_requests = ProbeRequests::channel();
        let probe_inbox = probe_requests
            .take_inbox()
            .expect("a fresh channel has an inbox");
        let ctx = ctx_with_crypt_file(db.connection(), &secret).await;
        let mut config = (*ctx.config).clone();
        configure(&mut config);
        let ctx = DbContext {
            config: Arc::new(config),
            memory: DbMemory {
                probe_requests,
                ..ctx.memory.clone()
            },
            store_path_mapping: mapping.unwrap_or_else(|| Arc::clone(&ctx.store_path_mapping)),
            ..ctx
        };

        testing::settle_seed(&ctx, &db, declared).await;

        let graph = Graph::new();
        let writer = graph
            .spawn(ctx.clone(), None, None)
            .await
            .expect("the graph writer starts");

        let case = Self {
            ctx,
            db,
            graph,
            writer,
            probe_inbox: Mutex::new(probe_inbox),
            claims: Mutex::new(HashMap::new()),
        };

        let report = case.drift().await;
        case.probe_requests();
        (case, report)
    }

    pub fn probe_requests(&self) -> BTreeSet<DerivationId> {
        let mut inbox = self.probe_inbox.lock().expect("the probe inbox");
        let mut asked = BTreeSet::new();
        while let Ok(derivations) = inbox.try_recv() {
            asked.extend(derivations);
        }

        asked
    }

    pub async fn walk(&self, batch: Batch) -> BatchReport {
        self.walk_result(batch).await.expect("the batch is written")
    }

    pub async fn walk_result(&self, batch: Batch) -> anyhow::Result<BatchReport> {
        send(&self.graph, batch).await
    }

    pub async fn commit_nar(&self, commit: NarCommit) -> NarCommitted {
        self.commit_nar_result(commit)
            .await
            .expect("the NAR is committed")
    }

    pub async fn commit_nar_result(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        self.graph.commit_nar(commit).await
    }

    pub async fn project_caches(&self, evaluation: EvaluationId) -> SignTargets {
        SignTargets::ProjectCaches(self.db.project_of(evaluation).await)
    }

    pub async fn seed_signature(&self, hash: &str, cache: CacheId, signature: Option<Vec<u8>>) {
        cached_path_signature::Model {
            id: CachedPathSignatureId::now_v7(),
            cached_path: self.cached_path(hash).await.id,
            cache,
            signature,
            created_at: test_date(),
            ..Default::default()
        }
        .into_active_model()
        .insert(&self.db.connection())
        .await
        .expect("the signature row is written");
    }

    pub async fn sb(&self, path: &str) -> derivation_build::Model {
        self.db.shared_build(path).await
    }

    pub async fn status(&self, path: &str) -> BuildStatus {
        self.sb(path).await.status
    }

    pub async fn queue_state(&self, path: &str) -> (BuildStatus, i32) {
        let build = self.sb(path).await;
        (build.status, build.blocking_deps)
    }

    pub async fn closure_state(&self, path: &str) -> (bool, i32) {
        let build = self.sb(path).await;
        (build.fetchable, build.missing_runtime_deps)
    }

    pub async fn edge_kind(&self, from: &str, to: &str) -> Option<EdgeKind> {
        self.db.edge(from, to).await.map(|edge| edge.kind)
    }

    pub async fn evaluation_status(&self, id: EvaluationId) -> EvaluationStatus {
        self.evaluation(id).await.status
    }

    pub async fn cached_path(&self, hash: &str) -> cached_path::Model {
        self.find_cached_path(hash)
            .await
            .unwrap_or_else(|| panic!("no cached path {hash}"))
    }

    pub async fn find_cached_path(&self, hash: &str) -> Option<cached_path::Model> {
        self.db.cached_path(hash).await
    }

    pub async fn signatures(&self, hash: &str) -> Vec<cached_path_signature::Model> {
        self.cached_path(hash).await;
        self.db.signatures(hash).await
    }

    pub async fn signature(
        &self,
        hash: &str,
        cache: CacheId,
    ) -> Option<cached_path_signature::Model> {
        self.signatures(hash)
            .await
            .into_iter()
            .find(|s| s.cache == cache)
    }

    pub async fn verifies(
        &self,
        signature: &cached_path_signature::Model,
        content: &NarCommit,
    ) -> bool {
        let Some(bytes) = &signature.signature else {
            return false;
        };

        let cache = cache::Entity::find_by_id(signature.cache)
            .one(&self.db.connection())
            .await
            .expect("the cache reads")
            .unwrap_or_else(|| panic!("no cache {}", signature.cache));

        let (secret, url) = (
            &self.ctx.config.secrets.crypt_file,
            &self.ctx.config.server.serve_url,
        );
        let token = gradient_sources::full_signature_token(bytes, url, &cache.name);
        let key = gradient_sources::format_cache_public_key(
            secret,
            cache::Model {
                public_key: String::new(),
                ..cache
            },
            url.clone(),
        )
        .expect("the public key of the cache");

        let narinfo = format!(
            "StorePath: {}\nNarHash: {}\nNarSize: {}\nReferences: {}\nSig: {token}\n",
            content.store_path,
            content.nar_hash,
            content.nar_size,
            content.references.join(" "),
        );

        gradient_sources::verify_narinfo_signature(&key, &narinfo)
    }

    pub async fn drift(&self) -> ConsistencyReport {
        testing::drift(&self.ctx).await
    }

    pub async fn zero_drift(&self, after: &str) {
        testing::zero_drift(&self.ctx, after).await;
    }

    pub async fn finish(self) {
        self.stop_writer().await;
        self.zero_drift("the last step").await;
        self.drop_database().await;
    }

    async fn stop_writer(&self) {
        self.writer
            .stop_and_wait(None, None)
            .await
            .expect("the graph writer stops");
    }

    async fn drop_database(self) {
        drop((self.ctx, self.graph));
        self.db.finish().await;
    }

    pub async fn finish_with_drift(self) -> ConsistencyReport {
        self.stop_writer().await;
        let report = self.drift().await;
        self.drop_database().await;
        report
    }

    pub async fn tasked(
        &self,
        evaluation: EvaluationId,
        derivations: Vec<DiscoveredDerivation>,
    ) -> Batch {
        Batch {
            derivations,
            task: Some(self.db.task_of(evaluation).await),
            ..batch(evaluation)
        }
    }

    pub async fn walk_in(scoped: &DbContext, batch: &Batch) -> anyhow::Result<BatchReport> {
        apply_in(scoped, batch).await
    }

    pub async fn with_budget_ms<T, F, Fut>(&self, budget_ms: u64, work: F) -> anyhow::Result<T>
    where
        F: Fn(DbContext) -> Fut,
        Fut: Future<Output = anyhow::Result<T>>,
    {
        transact(&self.ctx, Duration::from_millis(budget_ms), work).await
    }

    pub async fn commit_nar_outside_tx(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        crate::nar::commit(&self.ctx, &commit).await
    }

    pub async fn store_object(&self, hash: &str) {
        self.ctx
            .storage
            .nar_storage
            .put(hash, vec![0])
            .await
            .expect("the NAR object is written");
    }

    pub async fn upstream_hits(&self, hits: &[(&str, UpstreamHit)]) -> BTreeSet<DerivationId> {
        self.probe_requests();
        let hits: HashMap<String, UpstreamHit> = hits
            .iter()
            .map(|(hash, hit)| ((*hash).to_owned(), hit.clone()))
            .collect();

        self.graph
            .upstream_hits(hits)
            .await
            .expect("the hits are written");

        self.probe_requests()
    }

    pub async fn upstream_probed(&self, paths: &[&str]) -> BTreeSet<DerivationId> {
        self.probe_requests();
        let ids = self.ids(paths).await.into_iter().collect();
        self.graph
            .upstream_probed(ids)
            .await
            .expect("the probe answer is written");

        self.probe_requests()
    }

    pub async fn transition(&self, transition: Transition) -> TransitionReport {
        self.graph
            .transition(transition)
            .await
            .expect("the transition is written")
    }

    pub async fn assigned(
        &self,
        evaluation: EvaluationId,
        path: &str,
        substitute: bool,
    ) -> Option<DispatchedJobId> {
        let shared_build = self.sb(path).await.id;
        let assignment = build_assignment(
            evaluation,
            self.db.project_of(evaluation).await,
            self.db.task_of(evaluation).await,
            shared_build,
            gradient_types::now(),
        );
        let dispatched_job = assignment.id;
        if !claim_build(&self.ctx.worker_db, assignment, shared_build, substitute).await {
            return None;
        }

        self.transition(Transition::Assigned { shared_build }).await;
        self.claims
            .lock()
            .expect("the claims")
            .insert(shared_build, (dispatched_job, substitute));

        Some(dispatched_job)
    }

    pub async fn close_assignment(&self, path: &str) {
        let shared_build = self.sb(path).await.id;
        let closed = self
            .finish_assignments(&[shared_build], DispatchedJobOutcome::Completed)
            .await;

        assert_eq!(closed, 1, "no open assignment of {path}");
    }

    async fn finish_assignments(
        &self,
        shared_builds: &[DerivationBuildId],
        outcome: DispatchedJobOutcome,
    ) -> u64 {
        let jobs: Vec<String> = shared_builds.iter().copied().map(build_job_key).collect();
        dispatched_job::Entity::update_many()
            .col_expr(
                dispatched_job::Column::FinishedAt,
                Expr::value(gradient_types::now()),
            )
            .col_expr(
                dispatched_job::Column::Outcome,
                Expr::value(i16::from(outcome)),
            )
            .filter(dispatched_job::Column::JobId.is_in(jobs))
            .filter(dispatched_job::Column::FinishedAt.is_null())
            .exec(&self.db.connection())
            .await
            .expect("the assignments are closed")
            .rows_affected
    }

    pub async fn build_started(&self, path: &str) -> TransitionReport {
        let shared_build = self.sb(path).await.id;
        let claim = self
            .claims
            .lock()
            .expect("the claims")
            .remove(&shared_build);
        let unclaimed = (DispatchedJobId::now_v7(), false);
        let (dispatched_job, substitute) = claim.unwrap_or(unclaimed);
        self.transition(Transition::BuildStarted {
            shared_build,
            dispatched_job,
            substitute,
            build_context: serde_json::json!({}),
        })
        .await
    }

    pub async fn build_output(&self, path: &str, substituted: bool) -> TransitionReport {
        self.built_outputs(path, Vec::new(), substituted).await
    }

    pub async fn built_outputs(
        &self,
        path: &str,
        outputs: Vec<gradient_wire::types::BuildOutput>,
        substituted: bool,
    ) -> TransitionReport {
        self.transition(Transition::BuildOutput {
            shared_build: self.sb(path).await.id,
            outputs,
            metrics: None,
            substituted,
        })
        .await
    }

    pub async fn build_completed(&self, path: &str) -> TransitionReport {
        let shared_build = self.sb(path).await.id;
        self.transition(Transition::BuildCompleted { shared_build })
            .await
    }

    pub async fn build_failed(
        &self,
        path: &str,
        kind: BuildFailureKind,
        missing_paths: &[String],
    ) -> TransitionReport {
        self.transition(Transition::BuildFailed {
            shared_build: self.sb(path).await.id,
            error: format!("the build of {path} failed"),
            log_banner: String::new(),
            kind,
            missing_paths: missing_paths.to_vec(),
            metrics: None,
        })
        .await
    }

    pub async fn builds_lost(&self, paths: &[&str], cause: OrphanCause) -> TransitionReport {
        let shared_builds = self.sb_ids(paths).await;
        if cause == OrphanCause::WorkerLost {
            self.finish_assignments(&shared_builds, DispatchedJobOutcome::Abandoned)
                .await;
        }

        self.transition(worker_loss(shared_builds, cause)).await
    }

    pub async fn abort_build(&self, evaluation: EvaluationId, path: &str) -> TransitionReport {
        self.transition(Transition::AbortBuild {
            evaluation,
            shared_build: self.sb(path).await.id,
        })
        .await
    }

    pub async fn retry_build(&self, evaluation: EvaluationId, path: &str) -> TransitionReport {
        self.transition(Transition::RetryBuild {
            evaluation,
            shared_build: self.sb(path).await.id,
        })
        .await
    }

    pub async fn prioritize_build(&self, path: &str) -> TransitionReport {
        let shared_build = self.sb(path).await.id;
        self.transition(Transition::PrioritizeBuild { shared_build })
            .await
    }

    pub async fn not_enough_resources(
        &self,
        evaluation: EvaluationId,
        path: &str,
        needed_mb: u64,
    ) -> TransitionReport {
        self.transition(Transition::NotEnoughResources {
            shortages: vec![RamShortage {
                shared_build: self.sb(path).await.id,
                evaluation,
                needed_mb,
                largest_available_mb: needed_mb / 2,
            }],
        })
        .await
    }

    pub async fn requeue_imports(
        &self,
        evaluation: EvaluationId,
        paths: &[&str],
    ) -> TransitionReport {
        let derivations = self.ids(paths).await.into_iter().collect();
        self.transition(Transition::RequeueImports {
            evaluation,
            derivations,
            without_output_paths: Vec::new(),
        })
        .await
    }

    pub async fn requeue(&self) -> u64 {
        self.graph
            .requeue(RequeueScope::TransientRetries)
            .await
            .expect("the requeue runs")
    }

    pub async fn advance_clock(&self, path: &str) {
        self.override_sb(path, |build| build.updated_at -= chrono::Duration::days(30))
            .await;
    }

    pub async fn demote(&self, demotion: Demotion) -> DemoteReport {
        self.graph
            .demote(demotion)
            .await
            .expect("the demotion runs")
    }

    pub async fn gc(&self, request: GcRequest) -> GcReport {
        self.graph.gc(request).await.expect("the collection runs")
    }

    pub async fn known(&self, paths: &[&str]) -> Vec<String> {
        let hashes = paths
            .iter()
            .map(|path| {
                path.split_once('-')
                    .map_or(*path, |(hash, _)| hash)
                    .to_owned()
            })
            .collect();

        let mut known: Vec<String> = self
            .graph
            .known_derivations(hashes)
            .await
            .expect("the known derivations read")
            .iter()
            .map(|path| path.trim_start_matches("/nix/store/").to_owned())
            .collect();

        known.sort();
        known
    }

    pub async fn set_evaluation_status(&self, id: EvaluationId, status: EvaluationStatus) {
        evaluation::ActiveModel {
            id: Set(id),
            status: Set(status),
            ..Default::default()
        }
        .update(&self.db.connection())
        .await
        .expect("the evaluation status is written");
    }

    pub async fn override_sb(&self, path: &str, change: impl FnOnce(&mut derivation_build::Model)) {
        let mut row = self.sb(path).await;
        change(&mut row);
        row.into_active_model()
            .reset_all()
            .update(&self.db.connection())
            .await
            .expect("the shared build row is written");
    }

    pub async fn override_job(
        &self,
        evaluation: EvaluationId,
        path: &str,
        change: impl FnOnce(&mut build_job::Model),
    ) {
        let mut row = self
            .job(evaluation, path)
            .await
            .unwrap_or_else(|| panic!("no job of {path} in {evaluation}"));

        change(&mut row);
        row.into_active_model()
            .reset_all()
            .update(&self.db.connection())
            .await
            .expect("the job row is written");
    }

    pub async fn sql(&self, statement: &str) {
        self.db.execute(statement).await;
    }

    pub async fn ids(&self, paths: &[&str]) -> BTreeSet<DerivationId> {
        let mut ids = BTreeSet::new();
        for path in paths {
            ids.insert(self.db.id_of(path).await);
        }

        ids
    }

    async fn sb_ids(&self, paths: &[&str]) -> Vec<DerivationBuildId> {
        let mut ids = Vec::new();
        for path in paths {
            ids.push(self.sb(path).await.id);
        }

        ids
    }

    pub async fn derivation(&self, path: &str) -> derivation::Model {
        self.db.derivation(path).await
    }

    pub async fn find_derivation(&self, path: &str) -> Option<derivation::Model> {
        self.db.find_derivation(path).await
    }

    pub async fn edge(&self, from: &str, to: &str) -> Option<derivation_dependency::Model> {
        self.db.edge(from, to).await
    }

    pub async fn output_of(&self, path: &str, name: &str) -> derivation_output::Model {
        self.db.output_of(path, name).await
    }

    pub async fn output_stored(&self, path: &str, name: &str) -> bool {
        self.db.output_stored(path, name).await
    }

    pub async fn job(&self, evaluation: EvaluationId, path: &str) -> Option<build_job::Model> {
        self.db.job(evaluation, path).await
    }

    pub async fn jobs(&self, evaluation: EvaluationId) -> Vec<build_job::Model> {
        self.db.jobs(evaluation).await
    }

    pub async fn entry_point(
        &self,
        evaluation: EvaluationId,
        attr: &str,
    ) -> Option<entry_point::Model> {
        self.db.entry_point(evaluation, attr).await
    }

    pub async fn entry_points(&self, evaluation: EvaluationId) -> Vec<entry_point::Model> {
        self.db.entry_points(evaluation).await
    }

    pub async fn evaluation(&self, id: EvaluationId) -> evaluation::Model {
        self.db
            .find::<evaluation::Entity>(id)
            .await
            .unwrap_or_else(|| panic!("no evaluation {id}"))
    }

    pub async fn errors(&self, id: EvaluationId) -> Vec<String> {
        self.db.error_messages(id).await
    }

    pub async fn counters(&self, id: EvaluationId) -> [i64; 5] {
        let c = eval_counters(&self.db.connection(), id)
            .await
            .expect("the counters read")
            .unwrap_or_else(|| panic!("no counters of {id}"));

        [c.named, c.active, c.failed, c.queued, c.building]
    }

    pub async fn gv(&self, id: EvaluationId) -> i64 {
        self.evaluation(id).await.graph_version
    }

    pub async fn attempts(&self, path: &str) -> Vec<build_attempt::Model> {
        self.db.attempts(path).await
    }

    pub async fn attempt(&self, path: &str, number: usize) -> build_attempt::Model {
        self.attempts(path)
            .await
            .into_iter()
            .nth(number - 1)
            .unwrap_or_else(|| panic!("no attempt {number} of {path}"))
    }

    pub async fn latest_attempt(&self, path: &str) -> build_attempt::Model {
        self.attempts(path)
            .await
            .pop()
            .unwrap_or_else(|| panic!("no attempt of {path}"))
    }

    pub async fn open_assignment(&self, path: &str) -> Option<dispatched_job::Model> {
        self.db.open_assignment(path).await
    }

    pub async fn rows(&self) -> GraphRows {
        let mut shared_builds = self.db.rows::<derivation_build::Entity>().await;
        for build in &mut shared_builds {
            build.updated_at = test_date();
        }

        BTreeMap::from([
            (
                "derivation",
                printed(self.db.rows::<derivation::Entity>().await),
            ),
            ("derivation_build", printed(shared_builds)),
            (
                "derivation_dependency",
                printed(self.db.rows::<derivation_dependency::Entity>().await),
            ),
            (
                "derivation_output",
                printed(self.db.rows::<derivation_output::Entity>().await),
            ),
            (
                "build_job",
                printed(self.db.rows::<build_job::Entity>().await),
            ),
            (
                "entry_point",
                printed(self.db.rows::<entry_point::Entity>().await),
            ),
            (
                "cached_path",
                printed(self.db.rows::<cached_path::Entity>().await),
            ),
            (
                "cached_path_signature",
                printed(self.db.rows::<cached_path_signature::Entity>().await),
            ),
            (
                "build_attempt",
                printed(self.db.rows::<build_attempt::Entity>().await),
            ),
        ])
    }
}

fn printed<M: std::fmt::Debug>(rows: Vec<M>) -> Vec<String> {
    let mut printed: Vec<String> = rows.iter().map(|row| format!("{row:?}")).collect();
    printed.sort();
    printed
}

mod shared {
    use std::collections::BTreeSet;

    use chrono::NaiveDateTime;
    use gradient_entity::build::BuildStatus;
    use gradient_entity::build_attempt::AttemptFailureReason;
    use gradient_entity::ids::{DerivationId, EvaluationId};
    use gradient_fixtures::shape::{Shape, shape};
    use gradient_fixtures::test_date;
    use gradient_wire::types::{DerivationOutput, DiscoveredDerivation};

    use crate::messages::{NarCommit, UpstreamHit};
    use crate::test_ctx::Batch;

    pub(crate) const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
    pub(crate) const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";
    pub(crate) const C: &str = "cccccccccccccccccccccccccccccccc-c.drv";
    pub(crate) const D: &str = "dddddddddddddddddddddddddddddddd-d.drv";
    pub(crate) const F: &str = "ffffffffffffffffffffffffffffffff-f.drv";
    pub(crate) const G: &str = "gggggggggggggggggggggggggggggggg-g.drv";
    pub(crate) const H: &str = "hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh-h.drv";
    pub(crate) const I: &str = "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii-i.drv";
    pub(crate) const M: &str = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm-m.drv";
    pub(crate) const N: &str = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn-n.drv";
    pub(crate) const O: &str = "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj-o.drv";
    pub(crate) const P: &str = "pppppppppppppppppppppppppppppppp-p.drv";
    pub(crate) const Q: &str = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq-q.drv";
    pub(crate) const R: &str = "rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr-r.drv";
    pub(crate) const S: &str = "ssssssssssssssssssssssssssssssss-s.drv";
    pub(crate) const T: &str = "77777777777777777777777777777777-t.drv";
    pub(crate) const X: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-x.drv";
    pub(crate) const Y: &str = "yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy-y.drv";
    pub(crate) const Z: &str = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-z.drv";
    pub(crate) const A1: &str = "11111111111111111111111111111111-a1.drv";
    pub(crate) const A2: &str = "22222222222222222222222222222222-a2.drv";
    pub(crate) const BD: &str = "wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww-bd.drv";
    pub(crate) const F1: &str = "33333333333333333333333333333333-f1.drv";
    pub(crate) const F2: &str = "44444444444444444444444444444444-f2.drv";
    pub(crate) const G1: &str = "55555555555555555555555555555555-g1.drv";
    pub(crate) const H1: &str = "66666666666666666666666666666666-h1.drv";
    pub(crate) const H2: &str = "88888888888888888888888888888888-h2.drv";
    pub(crate) const O2: &str = "99999999999999999999999999999999-o2.drv";

    pub(crate) const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    pub(crate) const B_OUT: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
    pub(crate) const C_OUT: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
    pub(crate) const D_OUT: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
    pub(crate) const F_OUT: &str = "f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0";
    pub(crate) const G_OUT: &str = "g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0";
    pub(crate) const H_OUT: &str = "h0h0h0h0h0h0h0h0h0h0h0h0h0h0h0h0";
    pub(crate) const I_OUT: &str = "i0i0i0i0i0i0i0i0i0i0i0i0i0i0i0i0";
    pub(crate) const M_OUT: &str = "m0m0m0m0m0m0m0m0m0m0m0m0m0m0m0m0";
    pub(crate) const N_OUT: &str = "n0n0n0n0n0n0n0n0n0n0n0n0n0n0n0n0";
    pub(crate) const O_OUT: &str = "j0j0j0j0j0j0j0j0j0j0j0j0j0j0j0j0";
    pub(crate) const P_OUT: &str = "p0p0p0p0p0p0p0p0p0p0p0p0p0p0p0p0";
    pub(crate) const Q_OUT: &str = "q0q0q0q0q0q0q0q0q0q0q0q0q0q0q0q0";
    pub(crate) const R_OUT: &str = "r0r0r0r0r0r0r0r0r0r0r0r0r0r0r0r0";
    pub(crate) const S_OUT: &str = "s0s0s0s0s0s0s0s0s0s0s0s0s0s0s0s0";
    pub(crate) const T_OUT: &str = "70707070707070707070707070707070";
    pub(crate) const X_OUT: &str = "x0x0x0x0x0x0x0x0x0x0x0x0x0x0x0x0";
    pub(crate) const Y_OUT: &str = "y0y0y0y0y0y0y0y0y0y0y0y0y0y0y0y0";
    pub(crate) const Z_OUT: &str = "z0z0z0z0z0z0z0z0z0z0z0z0z0z0z0z0";
    pub(crate) const A1_OUT: &str = "10101010101010101010101010101010";
    pub(crate) const A2_OUT: &str = "20202020202020202020202020202020";
    pub(crate) const BD_OUT: &str = "w0w0w0w0w0w0w0w0w0w0w0w0w0w0w0w0";
    pub(crate) const F1_OUT: &str = "30303030303030303030303030303030";
    pub(crate) const G1_OUT: &str = "50505050505050505050505050505050";
    pub(crate) const H1_OUT: &str = "60606060606060606060606060606060";
    pub(crate) const H2_OUT: &str = "80808080808080808080808080808080";
    pub(crate) const O2_OUT: &str = "90909090909090909090909090909090";
    pub(crate) const A_OUT2: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    pub(crate) const B_OUT2: &str = "b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";
    pub(crate) const SOURCE: &str = "k0k0k0k0k0k0k0k0k0k0k0k0k0k0k0k0";
    pub(crate) const UNKNOWN_OUT: &str = "l0l0l0l0l0l0l0l0l0l0l0l0l0l0l0l0";

    pub(crate) const BUILDER_EXIT: Option<AttemptFailureReason> =
        Some(AttemptFailureReason::BuilderNonzero);

    pub(crate) use crate::testing::assert_only_unstored_outputs;

    pub(crate) fn t0() -> NaiveDateTime {
        test_date() + chrono::Duration::days(30)
    }

    pub(crate) fn chain(evaluation: EvaluationId) -> Shape {
        shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Queued)
    }

    pub(crate) fn chain_done(evaluation: EvaluationId) -> Shape {
        shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(B)
    }

    pub(crate) fn diamond(evaluation: EvaluationId) -> Shape {
        shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .depends_on(C)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .depends_on(D)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .depends_on(D)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::Queued)
    }

    pub(crate) fn pass(evaluation: EvaluationId) -> Shape {
        shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .output(A_OUT)
            .cache_available()
            .depends_on(B)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Skipped)
    }

    pub(crate) fn batch(evaluation: EvaluationId) -> Batch {
        Batch {
            evaluation,
            ..Default::default()
        }
    }

    pub(crate) fn drv(path: &str, dependencies: &[&str]) -> DiscoveredDerivation {
        DiscoveredDerivation {
            drv_path: path.to_owned(),
            dependencies: dependencies.iter().map(|d| (*d).to_owned()).collect(),
            architecture: "x86_64-linux".to_owned(),
            allow_substitutes: true,
            ..Default::default()
        }
    }

    pub(crate) fn with_output(
        mut derivation: DiscoveredDerivation,
        hash: &str,
    ) -> DiscoveredDerivation {
        let name = derivation
            .drv_path
            .split_once('-')
            .map_or("out", |(_, name)| name.trim_end_matches(".drv"))
            .to_owned();

        derivation.outputs.push(DerivationOutput {
            name: "out".into(),
            path: store_path(hash, &name),
        });

        derivation
    }

    pub(crate) fn with_attr(
        mut derivation: DiscoveredDerivation,
        attr: &str,
    ) -> DiscoveredDerivation {
        derivation.attr = attr.to_owned();
        derivation
    }

    pub(crate) fn with_limits(
        mut derivation: DiscoveredDerivation,
        timeout_secs: u64,
        max_silent_secs: u64,
    ) -> DiscoveredDerivation {
        derivation.timeout_secs = Some(timeout_secs);
        derivation.max_silent_secs = Some(max_silent_secs);
        derivation
    }

    pub(crate) fn with_ifd(mut derivation: DiscoveredDerivation) -> DiscoveredDerivation {
        derivation.ifd = true;
        derivation
    }

    pub(crate) fn store_path(hash: &str, package: &str) -> String {
        format!("/nix/store/{hash}-{package}")
    }

    pub(crate) fn nar(hash: &str, package: &str) -> NarCommit {
        NarCommit {
            store_path: store_path(hash, package),
            file_hash: "sha256:abc".into(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def".into(),
            confirmed: true,
            ..Default::default()
        }
    }

    pub(crate) fn drv_nar(path: &str) -> NarCommit {
        let (hash, package) = path
            .split_once('-')
            .unwrap_or_else(|| panic!("{path} is no drv path"));

        nar(hash, package)
    }

    pub(crate) fn with_references(mut commit: NarCommit, paths: &[(&str, &str)]) -> NarCommit {
        commit.references = stored_names(paths);
        commit
    }

    pub(crate) fn commit_of(hash: &str, package: &str, references: &[(&str, &str)]) -> NarCommit {
        with_references(nar(hash, package), references)
    }

    pub(crate) fn narinfo(references: &[(&str, &str)]) -> UpstreamHit {
        UpstreamHit {
            url: Some("u".into()),
            nar_hash: Some("nh".into()),
            file_hash: Some("fh".into()),
            file_size: Some(1),
            nar_size: Some(2),
            references: Some(stored_names(references).join(" ")),
            ..Default::default()
        }
    }

    pub(crate) fn as_set(ids: &[DerivationId]) -> BTreeSet<DerivationId> {
        ids.iter().copied().collect()
    }

    fn stored_names(paths: &[(&str, &str)]) -> Vec<String> {
        paths
            .iter()
            .map(|(hash, package)| format!("{hash}-{package}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use gradient_entity::build::BuildStatus;
    use gradient_fixtures::shape::shape;

    use super::*;

    #[tokio::test]
    #[should_panic(expected = "declared Queued")]
    async fn a_seed_production_would_repair_fails_before_the_first_step() {
        Case::start(
            shape()
                .evaluation(EvaluationId::now_v7())
                .derivation(A)
                .build_status(BuildStatus::Queued),
        )
        .await;
    }

    #[tokio::test]
    async fn an_empty_shape_is_a_consistent_seed() {
        Case::start(shape()).await.finish().await;
    }
}
