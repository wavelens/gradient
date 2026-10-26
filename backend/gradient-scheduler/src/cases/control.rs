/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDateTime;
use gradient_ci::ApplyOutcome;
use gradient_db::deliveries::pending::{self, Outcome, PendingDeliveryKind};
use gradient_db::deliveries::trigger_fire::{TriggerFire, enqueue_trigger_fires};
use gradient_db::status::BuildRefusal;
use gradient_entity::StorePathMapping;
use gradient_entity::ids::{DerivationId, DispatchedJobId, EvaluationId, TaskTriggerId};
use gradient_entity::{dispatched_job, evaluation, pending_delivery, task};
use gradient_fixtures::shape::build_assignment;
use gradient_graph::{
    Demotion, NarCommit, NarCommitted, RequeueScope, SignTargets, Transition, TransitionReport,
    UpstreamHit,
};
use gradient_pool::score::InstanceContext;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect,
};

use super::{SchedulerCase, aliases, lock};
use crate::instance::{EvalHistory, InstanceCounts};
use crate::probe::ProbePlan;

const DELIVERY_BATCH: usize = 64;

impl SchedulerCase {
    pub async fn admit_startable_moves(&self) {
        crate::loops::admit_startable_moves(&self.scheduler)
            .await
            .expect("the admission runs");
    }

    pub async fn resync_startable_set(&self) {
        crate::loops::resync_startable_set(&self.scheduler)
            .await
            .expect("the resync runs");
    }

    pub async fn build_assign_pass(&self, timer_tick: bool, resync: bool) {
        crate::loops::build_assign_pass(&self.scheduler, timer_tick, resync).await;
        self.scheduler.counts().await;
    }

    pub async fn assign_queued_evals(&self) {
        crate::loops::assign_queued_evals(&self.scheduler)
            .await
            .expect("the eval pass runs");
    }

    pub async fn refresh_waiting_state(&self) {
        self.scheduler
            .refresh_waiting_state()
            .await
            .expect("the waiting pass runs");
    }

    pub async fn recover_drv_stuck_evals(&self) {
        crate::waiting_state::recover_drv_stuck_evals(&self.scheduler)
            .await
            .expect("the drv recovery runs");
    }

    pub async fn reheal_graph_stuck_evals(&self) {
        crate::waiting_state::reheal_graph_stuck_evals(&self.scheduler.db.worker_db, &self.graph)
            .await
            .expect("the reheal runs");
    }

    pub async fn worker_liveness(&self) {
        crate::loops::worker_liveness_pass(&self.scheduler)
            .await
            .expect("the liveness pass runs");
    }

    pub fn liveness_period(&self) -> Option<Duration> {
        crate::loops::liveness_period(&self.ctx.config)
    }

    pub async fn sample_workers(&self) {
        crate::loops::worker_sample_pass(&self.scheduler)
            .await
            .expect("the sampling pass runs");
    }

    pub async fn abandoned_dispatch_sweep(&self) {
        crate::loops::abandoned_assignment_pass(&self.scheduler)
            .await
            .expect("the abandoned-dispatch sweep runs");
    }

    pub async fn stranded_build_sweep(&self) {
        crate::loops::stranded_build_pass(&self.scheduler)
            .await
            .expect("the stranded-build sweep runs");
    }

    pub async fn eval_completion_watchdog(&self) {
        crate::loops::eval_completion_watchdog_pass(&self.scheduler)
            .await
            .expect("the completion watchdog runs");
    }

    pub async fn reap_overdue_aborts(&self, grace: Duration) -> Vec<String> {
        self.scheduler.reap_overdue_aborts(grace).await
    }

    pub async fn abort_evaluation(&self, evaluation: EvaluationId) {
        let row = self.evaluation(evaluation).await;
        self.scheduler.abort_evaluation(row).await;
        self.settled().await;
    }

    pub async fn abort_build(
        &self,
        evaluation: EvaluationId,
        path: &str,
    ) -> Result<(), BuildRefusal> {
        let shared_build = self.sb(path).await.id;
        self.scheduler
            .abort_build(evaluation, shared_build)
            .await
            .expect("the graph writer answers the abort")
    }

    pub async fn retry_build(
        &self,
        evaluation: EvaluationId,
        path: &str,
    ) -> Result<(), BuildRefusal> {
        let shared_build = self.sb(path).await.id;
        self.scheduler
            .retry_build(evaluation, shared_build)
            .await
            .expect("the graph writer answers the retry")
    }

    pub async fn cancel_evaluation_jobs(&self, evaluation: EvaluationId, paths: &[&str]) {
        let mut shared_builds = Vec::new();
        for path in paths {
            shared_builds.push(self.sb(path).await.id);
        }
        self.scheduler
            .cancel_evaluation_jobs(evaluation, &shared_builds)
            .await;
    }

    pub async fn prioritize_evaluation(&self, evaluation: EvaluationId) {
        self.scheduler
            .prioritize_evaluation(evaluation)
            .await
            .expect("the evaluation is prioritized");
    }

    pub async fn prioritize_build(&self, path: &str) {
        let shared_build = self.sb(path).await.id;
        self.scheduler
            .prioritize_build(shared_build)
            .await
            .expect("the build is prioritized");
    }

    pub async fn drain_server(&self, on: bool) {
        self.scheduler.draining.set(on).await;
        let db = &self.scheduler.db.worker_db;
        if on {
            gradient_db::evaluations::draining::wait_active_evals_for_draining(db)
                .await
                .expect("the active evaluations wait for the drain");
        } else {
            gradient_db::evaluations::draining::resume_drained_evals(db)
                .await
                .expect("the draining evaluations return");
        }
    }

    pub async fn startup_recovery(&self) -> gradient_db::maintenance::recovery::RecoveryReport {
        let db = &self.scheduler.db.worker_db;
        let report = gradient_db::maintenance::recovery::recover_interrupted_work(db)
            .await
            .expect("the startup recovery runs");
        gradient_db::evaluations::draining::resume_drained_evals(db)
            .await
            .expect("the draining evaluations return");
        report
    }

    pub async fn plan_clusters(&self) {
        self.scheduler
            .plan_clusters()
            .await
            .expect("the cluster planning pass runs");
        self.settled().await;
    }

    pub async fn fire_once(&self) {
        crate::trigger_firing::fire_once(&self.scheduler.db)
            .await
            .expect("the trigger pass runs");
    }

    pub async fn deliver_trigger_fires(&self) -> usize {
        let db = &self.scheduler.db.worker_db;
        let rows = self.claim_due_trigger_fires().await;
        let mut fired = 0;
        for row in &rows {
            let outcome = match self.fire_payload(&row.payload).await {
                Ok(()) => Outcome::Delivered,
                Err(e) => Outcome::Retry(format!("{e:#}")),
            };
            pending::mark(db, row, &outcome)
                .await
                .expect("the delivery is marked");
            fired += 1;
        }
        self.settled().await;
        fired
    }

    pub async fn deliver_trigger_fire_again(&self, row: &pending_delivery::Model) {
        self.fire_payload(&row.payload)
            .await
            .expect("the firing applies");
        self.settled().await;
    }

    async fn fire_payload(&self, payload: &serde_json::Value) -> anyhow::Result<()> {
        let fire: TriggerFire =
            serde_json::from_value(payload.clone()).expect("a trigger fire payload");
        crate::trigger_firing::fire(&self.scheduler, &fire).await
    }

    async fn claim_due_trigger_fires(&self) -> Vec<pending::PendingDelivery> {
        let db = &self.scheduler.db.worker_db;
        let now = gradient_types::now();
        let due = pending_delivery::Entity::find()
            .filter(pending_delivery::Column::Kind.eq(PendingDeliveryKind::TriggerFire))
            .filter(pending_delivery::Column::DeliveredAt.is_null())
            .filter(pending_delivery::Column::FailedAt.is_null())
            .filter(pending_delivery::Column::NextAttemptAt.lte(now))
            .order_by_asc(pending_delivery::Column::NextAttemptAt)
            .order_by_asc(pending_delivery::Column::Id)
            .limit(DELIVERY_BATCH as u64)
            .all(db)
            .await
            .expect("the due trigger fires read");
        let leased = now + chrono::Duration::seconds(pending::CLAIM_LEASE_SECS);
        let mut claimed = Vec::with_capacity(due.len());
        for row in due {
            let delivery = pending::PendingDelivery {
                id: row.id,
                kind: row.kind,
                key: row.key.clone(),
                payload: row.payload.clone(),
                attempts: row.attempts,
            };
            pending_delivery::Model {
                next_attempt_at: leased,
                ..row
            }
            .into_active_model()
            .reset_all()
            .update(db)
            .await
            .expect("the trigger fire is leased");
            claimed.push(delivery);
        }
        claimed
    }

    pub async fn apply_trigger(
        &self,
        trigger: TaskTriggerId,
        branch: &str,
        commit: &[u8],
    ) -> ApplyOutcome {
        let row = self.trigger(trigger).await;
        let task = self
            .db
            .find::<task::Entity>(row.task)
            .await
            .expect("the task of the trigger");
        let input = aliases::trigger_input(
            trigger,
            row.trigger_type,
            branch,
            commit,
            self.scheduler.db.config.cache.max_storage_gb,
        );
        let outcome = gradient_ci::apply_trigger(&self.scheduler.db.worker_db, &task, input)
            .await
            .expect("the trigger applies");
        if let ApplyOutcome::Created {
            aborted_evaluation: Some(aborted),
            hard_abort,
            ..
        } = &outcome
        {
            self.scheduler
                .stop_aborted_evaluation(*aborted, *hard_abort)
                .await;
        }
        self.settled().await;
        outcome
    }

    pub async fn enqueue_trigger_fire(
        &self,
        trigger: TaskTriggerId,
        last_fired_at: Option<NaiveDateTime>,
    ) {
        enqueue_trigger_fires(
            &self.scheduler.db.worker_db,
            &[TriggerFire {
                trigger,
                last_fired_at,
            }],
        )
        .await
        .expect("the trigger fire is queued");
    }

    pub async fn probe_round(&self, paths: &[&str]) {
        let ids = self.derivation_ids(paths).await;
        self.probe.round(ids).await.expect("the probe round runs");
    }

    pub async fn probe_round_mapped(&self, mapping: Arc<dyn StorePathMapping>, paths: &[&str]) {
        let mut ctx = self.ctx.clone();
        ctx.store_path_mapping = mapping;
        let probe =
            crate::probe::UpstreamProbe::new(&ctx, &self.graph, &self.scheduler.upstream_query);
        let ids = self.derivation_ids(paths).await;
        probe.round(ids).await.expect("the probe round runs");
    }

    pub async fn plan_probes(&self, paths: &[&str]) -> ProbePlan {
        let ids = self.derivation_ids(paths).await;
        crate::probe::planned_round(&self.scheduler.db.worker_db, &ids)
            .await
            .expect("the probe plan reads")
    }

    pub async fn probe_sweep(&self) {
        self.probe.sweep().await.expect("the probe sweep runs");
    }

    async fn derivation_ids(&self, paths: &[&str]) -> Vec<DerivationId> {
        let mut ids = Vec::new();
        for path in paths {
            ids.push(self.db.id_of(path).await);
        }
        ids
    }

    pub async fn instance_context(&self, now: NaiveDateTime) -> InstanceContext {
        crate::instance::compute_instance_context(
            &self.scheduler.db.worker_db,
            InstanceCounts {
                active_builds: 0,
                pending_builds: 0,
                total_workers: 0,
                idle_workers: 0,
                cpu_core_score_mean: None,
                upload_speed_mean_mbps: None,
                download_speed_mean_mbps: None,
                download_slots: 1,
                upload_slots: 1,
            },
            now,
        )
        .await
    }

    pub async fn refit_build_history(&self) {
        let history = &self.scheduler.scoring.build_history;
        history
            .refresh(&self.scheduler.db.worker_db)
            .await
            .expect("the build history reads the metric rows");
        history.refit(0);
    }

    pub async fn eval_history(&self, now: NaiveDateTime) -> EvalHistory {
        crate::instance::compute_eval_history(&self.scheduler.db.worker_db, now, None).await
    }

    pub async fn commit_nar(&self, commit: NarCommit) -> NarCommitted {
        let committed = self
            .graph
            .commit_nar(commit)
            .await
            .expect("the NAR is committed");
        self.settled().await;
        committed
    }

    pub async fn project_caches(&self, evaluation: EvaluationId) -> SignTargets {
        SignTargets::ProjectCaches(self.db.project_of(evaluation).await)
    }

    pub async fn upstream_hits(&self, hits: &[(&str, UpstreamHit)]) {
        let hits = hits
            .iter()
            .map(|(hash, hit)| ((*hash).to_owned(), hit.clone()))
            .collect();
        self.graph
            .upstream_hits(hits)
            .await
            .expect("the hits are written");
        self.settled().await;
    }

    pub async fn upstream_probed(&self, paths: &[&str]) {
        let ids = self.derivation_ids(paths).await;
        self.graph
            .upstream_probed(ids)
            .await
            .expect("the probe answer is written");
        self.settled().await;
    }

    pub async fn demote_path(&self, path: &str) {
        let hash = path.split_once('-').map_or(path, |(hash, _)| hash);
        self.demote(Demotion::Path { hash: hash.into() }).await;
    }

    pub async fn demote(&self, demotion: Demotion) {
        self.graph
            .demote(demotion)
            .await
            .expect("the demotion runs");
        self.settled().await;
    }

    pub async fn transition(&self, transition: Transition) -> TransitionReport {
        let report = self
            .graph
            .transition(transition)
            .await
            .expect("the transition is written");
        self.settled().await;
        report
    }

    pub async fn requeue_transient_retries(&self) -> u64 {
        let requeued = self
            .graph
            .requeue(RequeueScope::TransientRetries)
            .await
            .expect("the requeue runs");
        self.settled().await;
        requeued
    }

    pub async fn backdate_shared_build(&self, path: &str, updated_at: NaiveDateTime) {
        let mut row = self.sb(path).await;
        row.updated_at = updated_at;
        row.into_active_model()
            .reset_all()
            .update(&self.db.connection())
            .await
            .expect("the shared build row is written");
    }

    pub async fn backdate_evaluation(&self, evaluation: EvaluationId, updated_at: NaiveDateTime) {
        let row = self.evaluation(evaluation).await;
        evaluation::Model { updated_at, ..row }
            .into_active_model()
            .reset_all()
            .update(&self.db.connection())
            .await
            .expect("the evaluation row is written");
    }

    pub async fn open_build_row(
        &self,
        evaluation: EvaluationId,
        path: &str,
        worker: &str,
        dispatched_at: NaiveDateTime,
    ) -> DispatchedJobId {
        let row = dispatched_job::Model {
            worker_id: worker.into(),
            ..build_assignment(
                evaluation,
                self.db.project_of(evaluation).await,
                self.db.task_of(evaluation).await,
                self.sb(path).await.id,
                dispatched_at,
            )
        };
        let id = row.id;
        row.into_active_model()
            .insert(&self.db.connection())
            .await
            .expect("the dispatched_job row is written");
        id
    }

    pub async fn graph_writer_down(&self) {
        self.settled().await;
        let writer = lock(&self.writer).clone();
        writer
            .stop_and_wait(None, None)
            .await
            .expect("the graph writer stops");
    }

    pub async fn graph_writer_up(&self) {
        let writer = self
            .graph
            .spawn(self.ctx.clone(), None, None)
            .await
            .expect("the graph writer starts again");
        *lock(&self.writer) = writer;
    }
}
