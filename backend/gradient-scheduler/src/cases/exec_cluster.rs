/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::exec_job::{self, ExecJobStatus};
use gradient_entity::ids::{ClusterJobId, EvaluationId};
use gradient_entity::{dispatched_job, exec_job_attempt, exec_job_token};
use gradient_pool::session_port::SessionSignal;
use gradient_types::WaitingReason;
use gradient_types::exec::{ExecInventory, ExecSpec, hash_job_token};
use gradient_wire::types::{Job, JobKind};

use super::exec::{W1, W2, exec_shape, inventory, spec};
use super::{SchedulerCase, w_exec, without_offers};
use crate::jobs::exec_job_key;
use crate::test_jobs::allow_all;

const ZONE: &str = "lab";

fn pair_spec(retries: u32, keep_running: bool) -> ExecSpec {
    ExecSpec {
        nodes: 2,
        network: Some(true),
        retries,
        keep_running,
        ..spec()
    }
}

fn pair_inventory() -> ExecInventory {
    ExecInventory {
        allow: gradient_types::exec::ExecAllow {
            nodes: 2,
            ..allow_all()
        },
        ..inventory()
    }
}

async fn seed_queued_cluster(case: &SchedulerCase, spec: &ExecSpec, cluster: ClusterJobId) {
    let (retries, last) = (spec.retries, spec.nodes - 1);
    let spec = serde_json::to_string(spec).expect("the spec encodes");
    case.db
        .execute("UPDATE project SET exec_enabled = true")
        .await;
    case.db
        .execute(&format!(
            "UPDATE entry_point SET exec = '{spec}'::jsonb WHERE eval = '{}'",
            super::exec::ATTR
        ))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, requirement, \
             spec, retries_left, status, created_at, node_index) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, '{{}}'::jsonb, e.exec, \
             {retries}, 0, now() AT TIME ZONE 'UTC', n \
             FROM entry_point e, generate_series(0, {last}) n WHERE e.exec IS NOT NULL"
        ))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO cluster_job (id, status, same_zone, attempts, retry_budget, created_at, \
             updated_at) VALUES ('{cluster}', 0, true, 0, 3, now() AT TIME ZONE 'UTC', \
             now() AT TIME ZONE 'UTC')"
        ))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO cluster_member (id, cluster_job, role, \"primary\", exec_job) \
             SELECT gen_random_uuid(), '{cluster}', 'node', false, id FROM exec_job"
        ))
        .await;
}

pub(super) async fn with_queued_cluster(spec: ExecSpec) -> (SchedulerCase, ClusterJobId) {
    let (evaluation, cluster) = (EvaluationId::now_v7(), ClusterJobId::now_v7());
    let case = SchedulerCase::start_then(exec_shape(evaluation), async |case| {
        seed_queued_cluster(case, &spec, cluster).await;
    })
    .await;
    (case, cluster)
}

pub(super) async fn node_rows(case: &SchedulerCase) -> Vec<exec_job::Model> {
    let mut rows = case.db.rows::<exec_job::Entity>().await;
    rows.sort_by_key(|row| row.node_index);
    rows
}

pub(super) async fn connect_pair(case: &SchedulerCase) {
    case.connect(W1, w_exec(pair_inventory()).zone(ZONE), &[])
        .await;
    case.connect(W2, w_exec(pair_inventory()).zone(ZONE), &[])
        .await;
}

pub(super) async fn place_pair(case: &SchedulerCase) {
    case.assign_exec_jobs().await;
    case.idle(W1, JobKind::Exec).await;
    case.idle(W2, JobKind::Exec).await;
    case.plan_clusters().await;
}

pub(super) fn assigned_key(case: &SchedulerCase, worker: &str) -> String {
    let assigned: Vec<String> = without_offers(case.signals(worker))
        .into_iter()
        .filter_map(|signal| match signal {
            SessionSignal::ClusterAssign { job_id } => Some(job_id),
            _ => None,
        })
        .collect();
    let [key] = assigned.as_slice() else {
        panic!("{worker} got one cluster member, got {assigned:?}");
    };
    key.clone()
}

#[tokio::test]
async fn a_two_node_job_is_placed_on_two_workers_with_one_master_port() {
    let (case, cluster) = with_queued_cluster(pair_spec(1, false)).await;
    connect_pair(&case).await;

    place_pair(&case).await;

    assert_eq!(
        case.cluster_attempts(cluster).await.len(),
        1,
        "after 1: one cluster attempt"
    );
    let rows = case.db.rows::<dispatched_job::Entity>().await;
    assert!(
        rows.len() == 2 && rows.iter().all(|row| row.cluster_attempt.is_some()),
        "after 1: two dispatched rows of the attempt: {rows:?}"
    );
    let ports: Vec<Option<i32>> = case
        .db
        .rows::<exec_job_attempt::Entity>()
        .await
        .iter()
        .map(|attempt| attempt.master_port)
        .collect();
    assert_eq!(
        ports,
        vec![Some(29500), Some(29500)],
        "after 1: both attempts carry the master port"
    );

    let nodes = node_rows(&case).await;
    let keys = [exec_job_key(nodes[0].id), exec_job_key(nodes[1].id)];
    let (on_w1, on_w2) = (assigned_key(&case, W1), assigned_key(&case, W2));
    let mut assigned = vec![on_w1.clone(), on_w2.clone()];
    assigned.sort();
    let mut expected = keys.to_vec();
    expected.sort();
    assert_eq!(assigned, expected, "after 1: each worker holds one node");

    let mut tokens = Vec::new();
    for (worker, key) in [(W1, &on_w1), (W2, &on_w2)] {
        let (membership, job) = case.take_cluster_job(worker, key).await;
        let Job::Exec(job) = job else {
            panic!("after 1: {key} is an exec job")
        };
        let node = u32::from(key == &keys[1]);
        assert_eq!(membership.index, node, "after 1: member index of {key}");
        assert_eq!(
            job.context.master_port,
            Some(29500),
            "after 1: master port of {key}"
        );
        tokens.push((job.context.attempt.clone(), job.context.token.clone()));
    }
    assert_ne!(tokens[0].1, tokens[1].1, "after 1: one token per node");
    let stored = case.db.rows::<exec_job_token::Entity>().await;
    for (attempt, token) in &tokens {
        let hashes: Vec<&str> = stored
            .iter()
            .filter(|row| row.attempt.to_string() == *attempt)
            .map(|row| row.token_hash.as_str())
            .collect();
        assert_eq!(
            hashes,
            vec![hash_job_token(token).as_str()],
            "after 1: the token of attempt {attempt}"
        );
    }
    case.finish().await;
}

#[tokio::test]
async fn a_two_node_job_never_runs_as_single_jobs() {
    let (case, _) = with_queued_cluster(pair_spec(1, false)).await;
    case.connect(W1, w_exec(pair_inventory()).zone(ZONE), &[])
        .await;

    case.assign_exec_jobs().await;
    case.idle(W1, JobKind::Exec).await;
    case.plan_clusters().await;

    assert!(
        case.db.rows::<dispatched_job::Entity>().await.is_empty(),
        "after 1: no dispatch"
    );
    for row in node_rows(&case).await {
        assert_eq!(row.status, ExecJobStatus::Queued, "after 1: the node");
        let reason = row
            .waiting_reason
            .as_ref()
            .and_then(WaitingReason::from_json);
        assert!(
            matches!(
                &reason,
                Some(WaitingReason::ExecWorkers { unmet, .. }) if unmet.nodes == 2
            ),
            "after 1: node {} waits for two workers: {reason:?}",
            row.node_index
        );
    }
    case.finish().await;
}
