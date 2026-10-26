/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{self, DispatchedJobKind};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_entity::runner_job::{self, RunnerJobStatus};
use gradient_entity::runner_job_attempt;
use gradient_fixtures::shape::{Shape, shape};
use gradient_types::WaitingReason;
use gradient_types::runner::{RunnerInventory, RunnerSpec};
use gradient_wire::types::{Job, JobKind};

use super::{A, A_OUT, SchedulerCase, w_runner};
use crate::test_jobs::allow_all;

const W1: &str = "w1";
const ATTR: &str = "apps.x86_64-linux.a";

fn runner_shape(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(ATTR)
        .job()
        .output(A_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(A)
}

fn spec() -> RunnerSpec {
    RunnerSpec {
        program: format!("/nix/store/{A_OUT}-a/bin/a"),
        nodes: 1,
        ..RunnerSpec::default()
    }
}

fn inventory() -> RunnerInventory {
    RunnerInventory {
        labels: vec![W1.into()],
        allow: allow_all(),
        ..RunnerInventory::default()
    }
}

async fn seed_queued_job(case: &SchedulerCase, spec: &RunnerSpec) {
    let spec = serde_json::to_string(spec).expect("the spec encodes");
    case.db
        .execute(&format!(
            "UPDATE entry_point SET runner = '{spec}'::jsonb WHERE eval = '{ATTR}'"
        ))
        .await;
    case.db
        .execute(
            "INSERT INTO runner_job (id, evaluation, entry_point, program_derivation, requirement, \
             spec, retries_left, status, created_at) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, '{}'::jsonb, e.runner, 0, \
             0, now() AT TIME ZONE 'UTC' FROM entry_point e WHERE e.runner IS NOT NULL",
        )
        .await;
}

async fn the_job(case: &SchedulerCase) -> runner_job::Model {
    let mut jobs = case.db.rows::<runner_job::Entity>().await;
    assert_eq!(jobs.len(), 1, "one runner job");
    jobs.remove(0)
}

#[tokio::test]
async fn a_queued_runner_job_reaches_a_matching_runner_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(runner_shape(e1)).await;
    seed_queued_job(&case, &spec()).await;
    case.connect(W1, w_runner(inventory()), &[]).await;

    case.assign_runner_jobs().await;
    let assignment = case
        .request_job(W1, JobKind::Runner)
        .await
        .expect("after 1: a runner job");

    assert!(
        matches!(assignment.job, Job::Runner(_)),
        "after 1: the job kind"
    );
    let job = the_job(&case).await;
    assert_eq!(job.status, RunnerJobStatus::Starting, "after 1: the job");
    let attempts = case.db.rows::<runner_job_attempt::Entity>().await;
    assert_eq!(
        attempts
            .iter()
            .map(|a| (a.number, a.worker.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, W1)],
        "after 1: the attempts"
    );
    let rows = case.db.rows::<dispatched_job::Entity>().await;
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![DispatchedJobKind::Runner],
        "after 1: the dispatched rows"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_unplaceable_job_stores_its_wait_reason() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(runner_shape(e1)).await;
    let printer = RunnerSpec {
        labels: vec!["3d-printer".into()],
        ..spec()
    };
    seed_queued_job(&case, &printer).await;
    case.connect(W1, w_runner(inventory()), &[]).await;

    case.assign_runner_jobs().await;

    let reason = the_job(&case)
        .await
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);
    assert!(
        matches!(
            reason,
            Some(WaitingReason::RunnerWorkers {
                matching_workers: 0,
                ..
            })
        ),
        "after 1: the wait reason: {reason:?}"
    );
    case.finish().await;
}
