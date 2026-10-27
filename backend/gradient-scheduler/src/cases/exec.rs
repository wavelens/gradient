/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::dispatched_job::{self, DispatchedJobKind};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::{self, ExecJobStatus};
use gradient_entity::exec_job_attempt;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ProjectId};
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::session_port::SessionSignal;
use gradient_types::WaitingReason;
use gradient_types::exec::{ExecInventory, ExecSpec};
use gradient_wire::types::BuildFailureKind;
use gradient_wire::types::{DiscoveredDerivation, Job, JobKind};

use super::{
    A, A_OUT, CaseShapes, SchedulerCase, drv, eval_key, failure, w_all, w_exec, with_attr,
    with_output,
};
use crate::test_jobs::allow_all;

const W1: &str = "w1";
const W2: &str = "w2";
const ATTR: &str = "apps.x86_64-linux.a";

fn exec_shape(evaluation: EvaluationId) -> Shape {
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

fn spec() -> ExecSpec {
    ExecSpec {
        program: format!("/nix/store/{A_OUT}-a/bin/a"),
        nodes: 1,
        ..ExecSpec::default()
    }
}

fn inventory() -> ExecInventory {
    ExecInventory {
        labels: vec![W1.into()],
        allow: allow_all(),
        ..ExecInventory::default()
    }
}

async fn seed_queued_job(case: &SchedulerCase, spec: &ExecSpec) {
    let retries = spec.retries;
    let spec = serde_json::to_string(spec).expect("the spec encodes");
    case.db
        .execute("UPDATE project SET exec_enabled = true")
        .await;
    case.db
        .execute(&format!(
            "UPDATE entry_point SET exec = '{spec}'::jsonb WHERE eval = '{ATTR}'"
        ))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, requirement, \
             spec, retries_left, status, created_at) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, '{{}}'::jsonb, e.exec, \
             {retries}, 0, now() AT TIME ZONE 'UTC' FROM entry_point e WHERE e.exec IS NOT NULL"
        ))
        .await;
}

async fn dispatched(case: &SchedulerCase, spec: &ExecSpec) -> (String, DispatchedJobId) {
    seed_queued_job(case, spec).await;
    case.connect(W1, w_exec(inventory()), &[]).await;
    case.assign_exec_jobs().await;
    let assignment = case
        .request_job(W1, JobKind::Exec)
        .await
        .expect("an exec job");
    (assignment.job_id().to_owned(), assignment.assignment_id())
}

async fn the_attempt(case: &SchedulerCase) -> exec_job_attempt::Model {
    let mut attempts = case.db.rows::<exec_job_attempt::Entity>().await;
    assert_eq!(attempts.len(), 1, "one attempt");
    attempts.remove(0)
}

async fn the_job(case: &SchedulerCase) -> exec_job::Model {
    let mut jobs = case.db.rows::<exec_job::Entity>().await;
    assert_eq!(jobs.len(), 1, "one exec job");
    jobs.remove(0)
}

#[tokio::test]
async fn a_queued_exec_job_reaches_a_matching_exec_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    seed_queued_job(&case, &spec()).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;
    let assignment = case
        .request_job(W1, JobKind::Exec)
        .await
        .expect("after 1: an exec job");

    assert!(
        matches!(assignment.job, Job::Exec(_)),
        "after 1: the job kind"
    );
    let job = the_job(&case).await;
    assert_eq!(job.status, ExecJobStatus::Starting, "after 1: the job");
    let attempts = case.db.rows::<exec_job_attempt::Entity>().await;
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
        vec![DispatchedJobKind::Exec],
        "after 1: the dispatched rows"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_unplaceable_job_stores_its_wait_reason() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    let printer = ExecSpec {
        labels: vec!["3d-printer".into()],
        ..spec()
    };
    seed_queued_job(&case, &printer).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.assign_exec_jobs().await;

    let reason = the_job(&case)
        .await
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json);
    assert!(
        matches!(
            reason,
            Some(WaitingReason::ExecWorkers {
                matching_workers: 0,
                ..
            })
        ),
        "after 1: the wait reason: {reason:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_lost_worker_requeues_its_exec_job_without_a_retry() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    dispatched(
        &case,
        &ExecSpec {
            retries: 2,
            ..spec()
        },
    )
    .await;

    case.disconnect(W1).await;

    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.retries_left),
        (ExecJobStatus::Queued, 2),
        "after 1: re-queued with its retries"
    );
    assert_eq!(
        the_attempt(&case).await.end_reason,
        Some(AttemptEndReason::Lost),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_non_zero_exit_reported_by_the_worker_restarts_the_job() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    let (key, assignment) = dispatched(
        &case,
        &ExecSpec {
            retries: 1,
            ..spec()
        },
    )
    .await;

    case.fail(W1, &key, assignment, failure(BuildFailureKind::Permanent))
        .await;

    let job = the_job(&case).await;
    assert_eq!(
        (job.status, job.retries_left),
        (ExecJobStatus::Queued, 0),
        "after 1: re-queued with the retry spent"
    );
    assert_eq!(
        the_attempt(&case).await.end_reason,
        Some(AttemptEndReason::Failed),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_abort_sends_abort_job_and_ends_aborted() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    let (key, _) = dispatched(&case, &spec()).await;
    case.signals(W1);

    let aborted = case
        .scheduler
        .abort_exec_job(the_job(&case).await.id)
        .await
        .expect("the abort runs");

    assert!(aborted, "after 1: the job was open");
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Aborted,
        "after 1: the job"
    );
    assert!(
        case.signals(W1)
            .iter()
            .any(|s| matches!(s, SessionSignal::Abort { job_id, .. } if *job_id == key)),
        "after 1: the worker was told to abort {key}"
    );
    case.finish().await;
}

const EVALUATOR: &str = "we";
const SECOND_ATTR: &str = "apps.x86_64-linux.b";

fn pre_warms(signals: &[SessionSignal]) -> Vec<u32> {
    signals
        .iter()
        .filter_map(|signal| match signal {
            SessionSignal::PreWarmVms { count } => Some(*count),
            _ => None,
        })
        .collect()
}

fn exec_root() -> DiscoveredDerivation {
    DiscoveredDerivation {
        exec: Some(spec()),
        ..with_attr(with_output(drv(A, &[]), A_OUT), ATTR)
    }
}

fn queued_evaluation_with_a_cached_program(evaluation: EvaluationId) -> Shape {
    shape()
        .evalq(evaluation)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(A)
}

async fn exec_recorded_mid_stream(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> (String, DispatchedJobId) {
    case.db
        .execute("UPDATE project SET exec_enabled = true")
        .await;
    case.connect(W1, w_exec(inventory()), &[]).await;
    case.connect(EVALUATOR, w_all(), &[]).await;
    let assignment = case.evalrun(EVALUATOR, evaluation).await;
    let key = eval_key(evaluation);
    case.advance_evaluation(EVALUATOR, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.signals(W1);
    case.report_eval_result(EVALUATOR, &key, vec![exec_root()])
        .await;
    (key, assignment)
}

#[tokio::test]
async fn an_exec_entry_point_recorded_mid_stream_sends_pre_warm_vms() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;

    exec_recorded_mid_stream(&case, e1).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_job_waiting_for_its_build_counts_and_dispatch_drops_it() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;
    let (key, assignment) = exec_recorded_mid_stream(&case, e1).await;
    case.signals(W1);

    case.complete(EVALUATOR, &key, assignment, Vec::new(), 0)
        .await;
    case.settled().await;
    assert_eq!(
        the_job(&case).await.status,
        ExecJobStatus::Created,
        "after 1: the job waits for its build"
    );
    case.scheduler.refresh_pre_warm().await;
    assert!(
        pre_warms(&case.signals(W1)).is_empty(),
        "after 1: the count stays 1"
    );

    case.assign_exec_jobs().await;
    case.request_job(W1, JobKind::Exec)
        .await
        .expect("after 2: W1 gets the exec job");
    case.assign_exec_jobs().await;

    assert_eq!(
        pre_warms(&case.signals(W1)).last(),
        Some(&0),
        "after 2: the dispatched job counts nowhere"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_aborted_evaluation_drops_its_exec_jobs() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(queued_evaluation_with_a_cached_program(e1)).await;
    exec_recorded_mid_stream(&case, e1).await;
    case.signals(W1);

    case.abort_evaluation(e1).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![0], "after 1: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_custom_image_or_finished_job_counts_nowhere() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1).entry_point(SECOND_ATTR)).await;
    let spec = serde_json::to_string(&spec()).expect("the spec encodes");
    case.db
        .execute(&format!("UPDATE entry_point SET exec = '{spec}'::jsonb"))
        .await;
    case.db
        .execute(&format!(
            "INSERT INTO exec_job (id, evaluation, entry_point, program_derivation, \
             image_derivation, requirement, spec, retries_left, status, created_at) \
             SELECT gen_random_uuid(), e.evaluation, e.id, e.derivation, \
             CASE WHEN e.eval = '{ATTR}' THEN e.derivation END, '{{}}'::jsonb, e.exec, 0, \
             CASE WHEN e.eval = '{ATTR}' THEN 0 ELSE 3 END, now() AT TIME ZONE 'UTC' \
             FROM entry_point e"
        ))
        .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    case.scheduler.refresh_pre_warm().await;

    assert!(
        pre_warms(&case.signals(W1)).is_empty(),
        "after 1: no count for W1"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_reconnected_worker_gets_its_count_again() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    let spec = serde_json::to_string(&spec()).expect("the spec encodes");
    case.db
        .execute(&format!("UPDATE entry_point SET exec = '{spec}'::jsonb"))
        .await;
    case.connect(W1, w_exec(inventory()), &[]).await;
    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");

    case.disconnect(W1).await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 2: W1");
    case.finish().await;
}

#[tokio::test]
async fn a_worker_of_another_project_gets_no_count() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(exec_shape(e1)).await;
    let spec = serde_json::to_string(&spec()).expect("the spec encodes");
    case.db
        .execute(&format!("UPDATE entry_point SET exec = '{spec}'::jsonb"))
        .await;

    case.connect(W2, w_exec(inventory()), &[ProjectId::now_v7()])
        .await;
    case.connect(W1, w_exec(inventory()), &[]).await;

    assert_eq!(pre_warms(&case.signals(W1)), vec![1], "after 1: W1");
    assert!(
        pre_warms(&case.signals(W2)).is_empty(),
        "after 1: no count for W2"
    );
    case.finish().await;
}
