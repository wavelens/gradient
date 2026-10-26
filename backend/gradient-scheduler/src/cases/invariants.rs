/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;

use chrono::NaiveDateTime;
use gradient_db::deliveries::trigger_fire::TriggerFire;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, TaskTriggerId};
use gradient_entity::pending_delivery::PendingDeliveryKind;
use gradient_entity::{
    build_attempt, cluster_attempt, cluster_job, cluster_member, dispatched_job, evaluation,
    pending_delivery, task_trigger, worker_connection,
};
use gradient_fixtures::database::TestDatabase;
use gradient_pool::WorkerInfo;
use sea_orm::Iterable;

use crate::Scheduler;
use crate::jobs::{TrackedKeys, build_job_key, eval_job_key};

pub(super) fn s11_a_single_job_list_per_key(tracked: &TrackedKeys, after: &str) {
    let lists = [
        ("pending", &tracked.pending),
        ("active", &tracked.active),
        ("cluster book", &tracked.cluster_book),
        ("awaiting reclaim", &tracked.awaiting_reclaim),
    ];
    assert_unique(
        "S11",
        after,
        "job lists of key",
        lists
            .into_iter()
            .flat_map(|(list, keys)| keys.iter().map(move |key| (key.as_str(), list))),
    );
}

pub(super) struct Rows {
    assignments: Vec<dispatched_job::Model>,
    attempts: Vec<build_attempt::Model>,
    evaluations: Vec<evaluation::Model>,
    connections: Vec<worker_connection::Model>,
    clusters: Vec<cluster_job::Model>,
    members: Vec<cluster_member::Model>,
    cluster_attempts: Vec<cluster_attempt::Model>,
    deliveries: Vec<pending_delivery::Model>,
    triggers: Vec<task_trigger::Model>,
}

impl Rows {
    pub(super) async fn read(db: &TestDatabase) -> Self {
        Self {
            assignments: db.rows::<dispatched_job::Entity>().await,
            attempts: db.rows::<build_attempt::Entity>().await,
            evaluations: db.rows::<evaluation::Entity>().await,
            connections: db.rows::<worker_connection::Entity>().await,
            clusters: db.rows::<cluster_job::Entity>().await,
            members: db.rows::<cluster_member::Entity>().await,
            cluster_attempts: db.rows::<cluster_attempt::Entity>().await,
            deliveries: db.rows::<pending_delivery::Entity>().await,
            triggers: db.rows::<task_trigger::Entity>().await,
        }
    }

    pub(super) fn assert_invariants(&self, after: &str, held_for_reclaim: &HashSet<String>) {
        self.s1_a_single_open_row_per_job(after);
        self.s2_open_rows_have_no_outcome(after);
        self.s6_disconnected_workers_have_no_open_row(after, held_for_reclaim);
        self.s14_a_single_running_attempt_per_shared_build(after);
        self.s19_waiting_reason_only_while_waiting(after);
        self.s20_a_single_active_evaluation_per_branch(after);
        self.s21_a_single_open_attempt_per_cluster(after);
        self.s22_closed_attempts_have_no_open_row(after);
        self.s23_running_clusters_have_a_started_attempt(after);
        self.s23_started_attempts_have_a_row_per_member(after);
        self.s24_workers_of_an_attempt(after);
        self.s27_a_single_undelivered_firing_per_key(after);
    }

    pub(super) fn s7_no_open_row_before_start(
        &self,
        connected: &HashSet<String>,
        started_at: NaiveDateTime,
        after: &str,
    ) {
        let older: Vec<_> = self
            .open_assignments()
            .filter(|row| connected.contains(&row.worker_id) && row.dispatched_at < started_at)
            .map(|row| (&row.worker_id, row.id, row.dispatched_at))
            .collect();
        assert!(
            older.is_empty(),
            "S7 after {after}: open rows of connected workers dispatched before the start {started_at}: {older:?}"
        );
    }

    pub(super) fn s8_open_builds_within_slots(&self, workers: &[WorkerInfo], after: &str) {
        for worker in workers {
            let builds: Vec<_> = self
                .open_assignments()
                .filter(|row| row.worker_id == worker.id && row.kind == DispatchedJobKind::Build)
                .map(|row| row.id)
                .collect();
            assert!(
                builds.len() <= worker.max_concurrent_builds as usize,
                "S8 after {after}: worker {} with {} build slots: open build rows {builds:?}",
                worker.id,
                worker.max_concurrent_builds
            );
        }
    }

    pub(super) async fn s12_no_open_job_pending(
        &self,
        scheduler: &Scheduler,
        workers_of_other_instances: &HashSet<String>,
        after: &str,
    ) {
        for key in self
            .open_assignments()
            .filter(|row| !workers_of_other_instances.contains(&row.worker_id))
            .filter_map(|row| row.job_id.as_deref())
        {
            assert!(
                scheduler.pending_job(key).await.is_none(),
                "S12 after {after}: job {key} pending while its row is open"
            );
        }
    }

    fn open_assignments(&self) -> impl Iterator<Item = &dispatched_job::Model> {
        self.assignments
            .iter()
            .filter(|row| row.finished_at.is_none())
    }

    fn s1_a_single_open_row_per_job(&self, after: &str) {
        assert_unique(
            "S1",
            after,
            "open rows of job",
            self.open_assignments()
                .filter_map(|row| Some((row.job_id.as_deref()?, row.id))),
        );
    }

    fn s2_open_rows_have_no_outcome(&self, after: &str) {
        for row in &self.assignments {
            assert_eq!(
                row.finished_at.is_none(),
                row.outcome.is_none(),
                "S2 after {after}: row {} with finished_at {:?} and outcome {:?}",
                row.id,
                row.finished_at,
                row.outcome
            );
        }
    }

    fn s6_disconnected_workers_have_no_open_row(
        &self,
        after: &str,
        held_for_reclaim: &HashSet<String>,
    ) {
        let connected: HashSet<&str> = self
            .connections
            .iter()
            .filter(|c| c.disconnected_at.is_none())
            .map(|c| c.worker_id.as_str())
            .collect();
        let gone: HashSet<&str> = self
            .connections
            .iter()
            .map(|c| c.worker_id.as_str())
            .filter(|worker| !connected.contains(worker))
            .collect();
        let open: Vec<_> = self
            .open_assignments()
            .filter(|row| gone.contains(row.worker_id.as_str()))
            .filter(|row| {
                !row.job_id
                    .as_ref()
                    .is_some_and(|key| held_for_reclaim.contains(key))
            })
            .map(|row| (&row.worker_id, row.id))
            .collect();
        assert!(
            open.is_empty(),
            "S6 after {after}: open rows of disconnected workers: {open:?}"
        );
    }

    fn s14_a_single_running_attempt_per_shared_build(&self, after: &str) {
        assert_unique(
            "S14",
            after,
            "Running attempts of shared build",
            self.attempts
                .iter()
                .filter(|a| a.outcome == AttemptOutcome::Running)
                .map(|a| (a.derivation_build, a.id)),
        );
    }

    fn s19_waiting_reason_only_while_waiting(&self, after: &str) {
        let outside: Vec<_> = self
            .evaluations
            .iter()
            .filter(|e| e.waiting_reason.is_some() && e.status != EvaluationStatus::Waiting)
            .map(|e| (e.id, e.status))
            .collect();
        assert!(
            outside.is_empty(),
            "S19 after {after}: waiting_reason outside Waiting: {outside:?}"
        );
    }

    fn s20_a_single_active_evaluation_per_branch(&self, after: &str) {
        assert_unique(
            "S20",
            after,
            "active evaluations of task and branch",
            self.evaluations
                .iter()
                .filter(|e| e.status.is_active() && !e.concurrent)
                .filter_map(|e| Some(((e.task?, e.branch.clone()), e.id))),
        );
    }

    fn s21_a_single_open_attempt_per_cluster(&self, after: &str) {
        assert_unique(
            "S21",
            after,
            "open attempts of cluster",
            self.cluster_attempts
                .iter()
                .filter(|a| a.finished_at.is_none())
                .map(|a| (a.cluster_job, a.id)),
        );
    }

    fn s22_closed_attempts_have_no_open_row(&self, after: &str) {
        let closed: HashSet<_> = self
            .cluster_attempts
            .iter()
            .filter(|a| a.finished_at.is_some())
            .map(|a| a.id)
            .collect();
        let open: Vec<_> = self
            .open_assignments()
            .filter(|row| row.cluster_attempt.is_some_and(|a| closed.contains(&a)))
            .map(|row| (row.id, row.cluster_attempt))
            .collect();
        assert!(
            open.is_empty(),
            "S22 after {after}: open rows of closed attempts: {open:?}"
        );
    }

    fn s23_running_clusters_have_a_started_attempt(&self, after: &str) {
        for cluster in self
            .clusters
            .iter()
            .filter(|c| c.status == ClusterJobStatus::Running)
        {
            let started: Vec<_> = self
                .cluster_attempts
                .iter()
                .filter(|a| a.cluster_job == cluster.id)
                .filter(|a| a.finished_at.is_none() && a.started_at.is_some())
                .map(|a| a.id)
                .collect();
            assert_eq!(
                started.len(),
                1,
                "S23 after {after}: open started attempts of Running cluster {}: {started:?}",
                cluster.id
            );
        }
    }

    fn s23_started_attempts_have_a_row_per_member(&self, after: &str) {
        for attempt in self
            .cluster_attempts
            .iter()
            .filter(|a| a.started_at.is_some())
        {
            let with_rows: HashSet<&str> = self
                .assignments
                .iter()
                .filter(|row| row.cluster_attempt == Some(attempt.id))
                .filter_map(|row| row.job_id.as_deref())
                .collect();
            let missing: Vec<_> = self
                .members
                .iter()
                .filter(|m| m.cluster_job == attempt.cluster_job)
                .filter_map(member_key)
                .filter(|key| !with_rows.contains(key.as_str()))
                .collect();
            assert!(
                missing.is_empty(),
                "S23 after {after}: started attempt {} without rows of members {missing:?}",
                attempt.id
            );
        }
    }

    fn s24_workers_of_an_attempt(&self, after: &str) {
        let open_member_rows = || {
            self.open_assignments()
                .filter_map(|row| Some((row.cluster_attempt?, row)))
        };
        assert_unique(
            "S24",
            after,
            "open rows of attempt and worker",
            open_member_rows().map(|(attempt, row)| ((attempt, row.worker_id.as_str()), row.id)),
        );

        let pins: HashMap<String, &str> = self
            .members
            .iter()
            .filter_map(|m| Some((member_key(m)?, m.pin.as_deref()?)))
            .collect();
        let off_pin: Vec<_> = open_member_rows()
            .filter_map(|(_, row)| {
                let pin = pins.get(row.job_id.as_deref()?)?;
                (row.worker_id != *pin).then_some((row.id, &row.worker_id, *pin))
            })
            .collect();
        assert!(
            off_pin.is_empty(),
            "S24 after {after}: pinned members on other workers: {off_pin:?}"
        );
    }

    fn s27_a_single_undelivered_firing_per_key(&self, after: &str) {
        assert_unique(
            "S27",
            after,
            "undelivered firings of key",
            self.deliveries
                .iter()
                .filter(|d| d.kind == PendingDeliveryKind::TriggerFire)
                .filter(|d| d.delivered_at.is_none() && d.failed_at.is_none())
                .map(|d| (d.key.as_str(), d.id)),
        );
    }

    fn delivered_firing(&self, key: &str) -> bool {
        self.deliveries.iter().any(|d| {
            d.kind == PendingDeliveryKind::TriggerFire && d.key == key && d.delivered_at.is_some()
        })
    }
}

type Closing = (Option<NaiveDateTime>, Option<DispatchedJobOutcome>);

#[derive(Default)]
pub(super) struct Observed {
    closed_rows: HashMap<DispatchedJobId, Closing>,
    evaluation_statuses: HashMap<EvaluationId, EvaluationStatus>,
    fired_at: HashMap<TaskTriggerId, Option<NaiveDateTime>>,
}

impl Observed {
    pub(super) fn of(rows: &Rows) -> Self {
        Self {
            closed_rows: rows
                .assignments
                .iter()
                .filter(|row| row.finished_at.is_some())
                .map(|row| (row.id, (row.finished_at, row.outcome)))
                .collect(),
            evaluation_statuses: rows.evaluations.iter().map(|e| (e.id, e.status)).collect(),
            fired_at: rows
                .triggers
                .iter()
                .map(|t| (t.id, t.last_fired_at))
                .collect(),
        }
    }

    pub(super) fn advance(&mut self, rows: &Rows, after: &str) {
        self.s2_closed_rows_stay_closed(rows, after);
        self.s18_evaluation_moves(rows, after);
        self.s28_firings_move_forward(rows, after);
        *self = Self::of(rows);
    }

    fn s2_closed_rows_stay_closed(&self, rows: &Rows, after: &str) {
        for row in &rows.assignments {
            if let Some(closing) = self.closed_rows.get(&row.id) {
                assert_eq!(
                    (row.finished_at, row.outcome),
                    *closing,
                    "S2 after {after}: closed row {} changed",
                    row.id
                );
            }
        }
    }

    fn s18_evaluation_moves(&self, rows: &Rows, after: &str) {
        for evaluation in &rows.evaluations {
            if let Some(&from) = self.evaluation_statuses.get(&evaluation.id) {
                assert!(
                    reachable(from, evaluation.status),
                    "S18 after {after}: evaluation {} moved {from:?} to {:?}",
                    evaluation.id,
                    evaluation.status
                );
            }
        }
    }

    fn s28_firings_move_forward(&self, rows: &Rows, after: &str) {
        for trigger in &rows.triggers {
            let Some(&before) = self.fired_at.get(&trigger.id) else {
                continue;
            };
            if trigger.last_fired_at == before {
                continue;
            }
            assert!(
                trigger.last_fired_at > before,
                "S28 after {after}: last_fired_at of trigger {} moved back from {before:?} to {:?}",
                trigger.id,
                trigger.last_fired_at
            );
            let firing = TriggerFire {
                trigger: trigger.id,
                last_fired_at: before,
            }
            .key();
            assert!(
                rows.delivered_firing(&firing),
                "S28 after {after}: last_fired_at of trigger {} moved without the delivered firing {firing}",
                trigger.id
            );
        }
    }
}

fn reachable(from: EvaluationStatus, to: EvaluationStatus) -> bool {
    let mut seen = vec![from];
    let mut next = vec![from];
    while let Some(status) = next.pop() {
        for target in EvaluationStatus::iter() {
            if allowed_move(status, target) && !seen.contains(&target) {
                seen.push(target);
                next.push(target);
            }
        }
    }
    seen.contains(&to)
}

fn allowed_move(from: EvaluationStatus, to: EvaluationStatus) -> bool {
    to.sources().contains(&from)
        || (to == EvaluationStatus::Building && EvaluationStatus::REOPENABLE.contains(&from))
}

fn member_key(member: &cluster_member::Model) -> Option<String> {
    member.derivation_build.map(build_job_key).or_else(|| {
        let index = member.eval_index.and_then(|i| u16::try_from(i).ok());
        member
            .evaluation
            .map(|e| eval_job_key(e, index.unwrap_or(0)))
    })
}

fn assert_unique<K: Hash + Eq + Debug, V: Debug>(
    invariant: &str,
    after: &str,
    what: &str,
    pairs: impl Iterator<Item = (K, V)>,
) {
    let mut groups: HashMap<K, Vec<V>> = HashMap::new();
    for (key, value) in pairs {
        groups.entry(key).or_default().push(value);
    }
    for (key, values) in groups {
        assert!(
            values.len() <= 1,
            "{invariant} after {after}: {what} {key:?}: {values:?}"
        );
    }
}
