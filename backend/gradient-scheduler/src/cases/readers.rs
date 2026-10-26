/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet, HashSet};

use gradient_db::evaluations::counters::eval_counters;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::ids::{ClusterJobId, DispatchedJobId, EvaluationId, TaskTriggerId};
use gradient_entity::{
    build_attempt, build_job, cached_path, cluster_attempt, cluster_job, derivation_build,
    derivation_metric, derivation_output, dispatched_job, dispatched_job_phase, eval_cache_store,
    evaluation, evaluation_input_update, evaluation_message, evaluation_metric, pending_delivery,
    task_trigger, upstream_metric, worker_connection, worker_sample,
};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use super::SchedulerCase;
use crate::actor::{Offer, SchedulerMsg};
use crate::cluster::{PendingCluster, Reservation, SlotKind};
use crate::jobs::{PendingBuildJob, PendingJob, build_job_key};
use crate::probe::ProbePlan;
use gradient_wire::types::FlakeJob;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    pub workers: usize,
    pub pending: usize,
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UpstreamTally {
    pub url: String,
    pub requests: i32,
    pub hits: i32,
    pub misses: i32,
}

pub(crate) fn planned_targets(plan: &ProbePlan) -> BTreeMap<EvaluationId, BTreeSet<String>> {
    plan.rounds
        .iter()
        .map(|(evaluation, targets)| {
            (
                evaluation.id,
                targets.iter().map(|path| path.hash().to_owned()).collect(),
            )
        })
        .collect()
}

pub(crate) fn job_ids(offer: &Offer) -> Vec<String> {
    offer.candidates.iter().map(|c| c.job_id.clone()).collect()
}

impl SchedulerCase {
    pub async fn key(&self, path: &str) -> String {
        build_job_key(self.sb(path).await.id)
    }

    pub async fn sb(&self, path: &str) -> derivation_build::Model {
        self.db.shared_build(path).await
    }

    pub async fn attempts(&self, path: &str) -> Vec<build_attempt::Model> {
        self.db.attempts(path).await
    }

    pub async fn attempt_outcomes(&self, path: &str) -> Vec<AttemptOutcome> {
        self.attempts(path)
            .await
            .into_iter()
            .map(|t| t.outcome)
            .collect()
    }

    pub async fn attempt_outcomes_with_reasons(
        &self,
        path: &str,
    ) -> Vec<(AttemptOutcome, Option<AttemptFailureReason>)> {
        self.attempts(path)
            .await
            .into_iter()
            .map(|t| (t.outcome, t.reason))
            .collect()
    }

    pub async fn counters(&self, evaluation: EvaluationId) -> [i64; 5] {
        let c = eval_counters(&self.db.connection(), evaluation)
            .await
            .expect("the counters read")
            .unwrap_or_else(|| panic!("no counters of {evaluation}"));
        [c.named, c.active, c.failed, c.queued, c.building]
    }

    pub async fn evaluation(&self, id: EvaluationId) -> evaluation::Model {
        self.db
            .find::<evaluation::Entity>(id)
            .await
            .unwrap_or_else(|| panic!("no evaluation {id}"))
    }

    pub async fn evaluations(&self) -> Vec<evaluation::Model> {
        evaluation::Entity::find()
            .order_by_asc(evaluation::Column::CreatedAt)
            .order_by_asc(evaluation::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the evaluations read")
    }

    pub async fn job(&self, evaluation: EvaluationId, path: &str) -> Option<build_job::Model> {
        self.db.job(evaluation, path).await
    }

    pub async fn output_of(&self, path: &str, name: &str) -> derivation_output::Model {
        self.db.output_of(path, name).await
    }

    pub async fn cached_path(&self, hash: &str) -> Option<cached_path::Model> {
        self.db.cached_path(hash).await
    }

    pub async fn assignments(&self, key: &str) -> Vec<dispatched_job::Model> {
        dispatched_job::Entity::find()
            .filter(dispatched_job::Column::JobId.eq(key))
            .order_by_asc(dispatched_job::Column::CreatedAt)
            .order_by_asc(dispatched_job::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the assignments read")
    }

    pub async fn assignment_outcomes(
        &self,
        key: &str,
    ) -> Vec<(DispatchedJobId, Option<DispatchedJobOutcome>)> {
        self.assignments(key)
            .await
            .into_iter()
            .map(|row| (row.id, row.outcome))
            .collect()
    }

    pub async fn open_assignment(&self, key: &str) -> Option<dispatched_job::Model> {
        self.assignments(key)
            .await
            .into_iter()
            .find(|row| row.finished_at.is_none())
    }

    pub async fn assignments_of(&self, worker: &str) -> Vec<dispatched_job::Model> {
        dispatched_job::Entity::find()
            .filter(dispatched_job::Column::WorkerId.eq(worker))
            .order_by_asc(dispatched_job::Column::CreatedAt)
            .order_by_asc(dispatched_job::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the assignments read")
    }

    pub async fn phases(&self, assignment: DispatchedJobId) -> Vec<dispatched_job_phase::Model> {
        dispatched_job_phase::Entity::find()
            .filter(dispatched_job_phase::Column::DispatchedJob.eq(assignment))
            .order_by_asc(dispatched_job_phase::Column::Seq)
            .all(&self.db.connection())
            .await
            .expect("the phases read")
    }

    pub async fn connections(&self, worker: &str) -> Vec<worker_connection::Model> {
        worker_connection::Entity::find()
            .filter(worker_connection::Column::WorkerId.eq(worker))
            .order_by_asc(worker_connection::Column::ConnectedAt)
            .order_by_asc(worker_connection::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the connections read")
    }

    pub async fn worker_samples(&self) -> Vec<worker_sample::Model> {
        worker_sample::Entity::find()
            .order_by_asc(worker_sample::Column::At)
            .order_by_asc(worker_sample::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the worker samples read")
    }

    pub async fn samples(&self, worker: &str) -> Vec<worker_sample::Model> {
        self.worker_samples()
            .await
            .into_iter()
            .filter(|sample| sample.worker_id == worker)
            .collect()
    }

    pub async fn deliveries(&self) -> Vec<pending_delivery::Model> {
        pending_delivery::Entity::find()
            .order_by_asc(pending_delivery::Column::CreatedAt)
            .order_by_asc(pending_delivery::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the deliveries read")
    }

    pub async fn trigger(&self, id: TaskTriggerId) -> task_trigger::Model {
        self.db
            .find::<task_trigger::Entity>(id)
            .await
            .unwrap_or_else(|| panic!("no trigger {id}"))
    }

    pub async fn cluster(&self, id: ClusterJobId) -> cluster_job::Model {
        self.db
            .find::<cluster_job::Entity>(id)
            .await
            .unwrap_or_else(|| panic!("no cluster job {id}"))
    }

    pub async fn cluster_attempts(&self, cluster: ClusterJobId) -> Vec<cluster_attempt::Model> {
        cluster_attempt::Entity::find()
            .filter(cluster_attempt::Column::ClusterJob.eq(cluster))
            .order_by_asc(cluster_attempt::Column::CreatedAt)
            .order_by_asc(cluster_attempt::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the cluster attempts read")
    }

    pub async fn messages(&self, evaluation: EvaluationId) -> Vec<evaluation_message::Model> {
        evaluation_message::Entity::find()
            .filter(evaluation_message::Column::Evaluation.eq(evaluation))
            .order_by_asc(evaluation_message::Column::CreatedAt)
            .order_by_asc(evaluation_message::Column::Id)
            .all(&self.db.connection())
            .await
            .expect("the messages read")
    }

    pub async fn evaluation_metric(
        &self,
        evaluation: EvaluationId,
    ) -> Option<evaluation_metric::Model> {
        evaluation_metric::Entity::find()
            .filter(evaluation_metric::Column::Evaluation.eq(evaluation))
            .one(&self.db.connection())
            .await
            .expect("the evaluation metric reads")
    }

    pub async fn derivation_metrics(&self, path: &str) -> Vec<derivation_metric::Model> {
        derivation_metric::Entity::find()
            .filter(derivation_metric::Column::Derivation.eq(self.db.id_of(path).await))
            .order_by_asc(derivation_metric::Column::CreatedAt)
            .all(&self.db.connection())
            .await
            .expect("the derivation metrics read")
    }

    pub async fn upstream_metrics(&self) -> Vec<UpstreamTally> {
        let rows = upstream_metric::Entity::find()
            .order_by_asc(upstream_metric::Column::UpstreamUrl)
            .all(&self.db.connection())
            .await
            .expect("the upstream metrics read");
        let mut tallies: BTreeMap<String, UpstreamTally> = BTreeMap::new();
        for row in rows {
            let tally = tallies
                .entry(row.upstream_url.clone())
                .or_insert_with(|| UpstreamTally {
                    url: row.upstream_url.clone(),
                    requests: 0,
                    hits: 0,
                    misses: 0,
                });
            tally.requests += row.request_count;
            tally.hits += row.narinfo_hits;
            tally.misses += row.narinfo_misses;
        }
        tallies.into_values().collect()
    }

    pub async fn input_update(
        &self,
        evaluation: EvaluationId,
    ) -> Option<evaluation_input_update::Model> {
        evaluation_input_update::Entity::find()
            .filter(evaluation_input_update::Column::Evaluation.eq(evaluation))
            .one(&self.db.connection())
            .await
            .expect("the input update reads")
    }

    pub async fn eval_cache(&self, fingerprint: &str) -> Option<eval_cache_store::Model> {
        eval_cache_store::Entity::find()
            .filter(eval_cache_store::Column::Fingerprint.eq(fingerprint))
            .one(&self.db.connection())
            .await
            .expect("the eval cache reads")
    }

    pub async fn pending(&self, key: &str) -> bool {
        self.scheduler.pending_job(key).await.is_some()
    }

    pub async fn pending_job(&self, key: &str) -> Option<PendingJob> {
        self.scheduler.pending_job(key).await
    }

    pub async fn pending_flake_job(&self, key: &str) -> Option<FlakeJob> {
        match self.pending_job(key).await? {
            PendingJob::Eval(job) => Some(job.job),
            PendingJob::Build(_) | PendingJob::Runner(_) => None,
        }
    }

    pub async fn pending_build_job(&self, key: &str) -> Option<PendingBuildJob> {
        match self.pending_job(key).await? {
            PendingJob::Build(job) => Some(job),
            PendingJob::Eval(_) | PendingJob::Runner(_) => None,
        }
    }

    pub async fn active(&self, key: &str) -> Option<String> {
        let job_id = key.to_owned();
        self.scheduler
            .call(|reply| SchedulerMsg::ActiveWorker { job_id, reply })
            .await
            .expect("the scheduler core names the worker of a job")
    }

    pub async fn tracked(&self, key: &str) -> bool {
        self.scheduler
            .untracked(vec![key.to_owned()])
            .await
            .is_empty()
    }

    pub async fn totals(&self) -> Totals {
        let c = self.scheduler.counts().await;
        Totals {
            workers: c.workers,
            pending: c.pending,
            active: c.active,
        }
    }

    pub fn input_waits(&self) -> usize {
        self.scheduler.input_waits.waiting_requests()
    }

    pub fn import_waits(&self) -> usize {
        self.scheduler.import_waits.waiting_requests()
    }

    pub async fn sent_set(&self, worker: &str) -> HashSet<String> {
        let worker = worker.to_owned();
        self.scheduler
            .call(|reply| SchedulerMsg::SentCandidates { worker, reply })
            .await
            .expect("the scheduler core lists the sent candidates")
            .unwrap_or_default()
    }

    pub async fn idle_slots(&self) -> Vec<(String, SlotKind)> {
        self.scheduler
            .cluster_snapshot()
            .await
            .slots
            .into_iter()
            .map(|slot| (slot.worker, slot.kind))
            .collect()
    }

    pub async fn waiting_cluster(&self, cluster: ClusterJobId) -> Option<PendingCluster> {
        self.scheduler
            .call(|reply| SchedulerMsg::WaitingCluster { cluster, reply })
            .await
            .expect("the scheduler core reads the cluster book")
    }

    pub async fn reservation(&self) -> Option<Reservation> {
        self.scheduler.reservation().await
    }
}
