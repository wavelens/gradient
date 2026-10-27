/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ExecJobAttemptId};
use gradient_entity::{evaluation_message, exec_job, exec_job_attempt, exec_job_phase};
use gradient_fixtures::shape::shape;
use gradient_types::exec::{ExecSpec, exec_image_attr};
use gradient_wire::types::{BuildFailureKind, DiscoveredDerivation};

use super::{A, A_OUT, B, B_OUT, Case, drv, nar, with_attr, with_output, with_references};
use crate::messages::{Transition, TransitionReport};

const ATTR: &str = "apps.x86_64-linux.a";

fn entry_point(path: &str, output: &str, attr: &str) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, &[]), output), attr)
}

fn spec() -> ExecSpec {
    ExecSpec {
        program: format!("{A_OUT}/bin/a"),
        nodes: 1,
        ..Default::default()
    }
}

fn exec_root() -> DiscoveredDerivation {
    exec_root_with(spec())
}

fn exec_root_with(spec: ExecSpec) -> DiscoveredDerivation {
    DiscoveredDerivation {
        exec: Some(spec),
        ..entry_point(A, A_OUT, ATTR)
    }
}

fn image_root() -> DiscoveredDerivation {
    entry_point(B, B_OUT, &exec_image_attr(ATTR))
}

async fn switch_exec_jobs_on(case: &Case) {
    case.sql("UPDATE project SET exec_enabled = true").await;
}

async fn messages(case: &Case, evaluation: EvaluationId) -> Vec<(String, Option<String>)> {
    case.db
        .rows::<evaluation_message::Entity>()
        .await
        .into_iter()
        .filter(|m| m.evaluation == evaluation)
        .map(|m| (m.message, m.source))
        .collect()
}

#[tokio::test]
async fn an_exec_entry_point_keeps_its_spec() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;

    let points = case.entry_points(e1).await;
    assert_eq!(points.len(), 1, "after 1: one entry point");
    let stored: ExecSpec =
        serde_json::from_value(points[0].exec.clone().expect("after 1: an exec column"))
            .expect("after 1: the spec decodes");
    assert_eq!(stored, spec(), "after 1: the stored spec");
    case.finish().await;
}

#[tokio::test]
async fn a_project_without_exec_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "exec jobs are disabled for this project".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_instance_without_exec_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_without_exec_jobs(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "exec jobs are disabled on this instance".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_refused_exec_inserts_neither_its_program_nor_its_image() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![exec_root(), image_root()]).await)
        .await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert!(
        !case.sb(A).await.wanted,
        "after 1: the program is not wanted"
    );
    assert!(!case.sb(B).await.wanted, "after 1: the image is not wanted");
    case.finish().await;
}

#[tokio::test]
async fn a_refused_exec_skips_its_image_in_a_later_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;
    case.walk(case.tasked(e1, vec![image_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 2: no entry point"
    );
    assert!(!case.sb(B).await.wanted, "after 2: the image is not wanted");
    case.finish().await;
}

#[tokio::test]
async fn a_multi_node_exec_skips_its_image_in_a_later_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;

    case.walk(
        case.tasked(e1, vec![exec_root_with(ExecSpec { nodes: 2, ..spec() })])
            .await,
    )
    .await;
    case.walk(case.tasked(e1, vec![image_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 2: no entry point"
    );
    assert!(!case.sb(B).await.wanted, "after 2: the image is not wanted");
    case.finish().await;
}

#[tokio::test]
async fn an_accepted_exec_keeps_its_image_from_a_later_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;
    case.walk(case.tasked(e1, vec![image_root()]).await).await;

    let mut attrs: Vec<String> = case
        .entry_points(e1)
        .await
        .into_iter()
        .map(|point| point.eval)
        .collect();
    attrs.sort();
    assert_eq!(
        attrs,
        vec![ATTR.to_owned(), exec_image_attr(ATTR)],
        "after 2: the program and its image"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_repeated_batch_counts_no_new_exec_entry_point() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;

    let first = case.walk(case.tasked(e1, vec![exec_root()]).await).await;
    let again = case.walk(case.tasked(e1, vec![exec_root()]).await).await;

    assert_eq!(
        first.exec_entry_points, 1,
        "after 1: one new exec entry point"
    );
    assert_eq!(again.exec_entry_points, 0, "after 2: nothing new");
    case.finish().await;
}

async fn walked_exec(spec: ExecSpec) -> (Case, EvaluationId) {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_exec_jobs_on(&case).await;
    case.walk(case.tasked(e1, vec![exec_root_with(spec)]).await)
        .await;
    case.transition(Transition::EvalStreamCompleted { evaluation: e1 })
        .await;
    (case, e1)
}

async fn exec_case(spec: ExecSpec) -> (Case, EvaluationId) {
    let (case, e1) = walked_exec(spec).await;
    case.build_started(A).await;
    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;
    case.build_completed(A).await;
    (case, e1)
}

async fn queued_case(spec: ExecSpec) -> (Case, EvaluationId) {
    let (case, e1) = exec_case(spec).await;
    case.transition(Transition::SettleExecJobs).await;
    (case, e1)
}

fn with_retries(retries: u32) -> ExecSpec {
    ExecSpec { retries, ..spec() }
}

async fn job(case: &Case, evaluation: EvaluationId) -> exec_job::Model {
    let mut jobs: Vec<exec_job::Model> = case
        .db
        .rows::<exec_job::Entity>()
        .await
        .into_iter()
        .filter(|job| job.evaluation == evaluation)
        .collect();
    assert_eq!(jobs.len(), 1, "one exec job of {evaluation}");
    jobs.remove(0)
}

async fn assign(
    case: &Case,
    evaluation: EvaluationId,
    number: u32,
    retries_cap: u32,
) -> ExecJobAttemptId {
    let attempt = ExecJobAttemptId::now_v7();
    let report = case
        .transition(Transition::ExecJobAssigned {
            exec_job: job(case, evaluation).await.id,
            attempt,
            number,
            assignment: DispatchedJobId::now_v7(),
            worker: "w1".into(),
            retries_cap,
        })
        .await;
    assert!(!report.already_aborted, "attempt {number} opens");
    attempt
}

async fn end_attempt(
    case: &Case,
    attempt: ExecJobAttemptId,
    reason: AttemptEndReason,
) -> TransitionReport {
    case.transition(Transition::ExecAttemptEnded {
        attempt,
        reason,
        message: Some("the program exited with 1".into()),
    })
    .await
}

async fn attempt_row(case: &Case, attempt: ExecJobAttemptId) -> exec_job_attempt::Model {
    case.db
        .find::<exec_job_attempt::Entity>(attempt)
        .await
        .unwrap_or_else(|| panic!("no attempt {attempt}"))
}

async fn exec_events(case: &Case) -> Vec<String> {
    case.db
        .texts("SELECT key FROM pending_delivery WHERE key LIKE 'exec_job%' ORDER BY id")
        .await
}

fn count_status(events: &[String], status: ExecJobStatus) -> usize {
    let suffix = format!(":{}", i16::from(status));
    events
        .iter()
        .filter(|key| key.starts_with("exec_job:") && key.ends_with(&suffix))
        .count()
}

#[tokio::test]
async fn a_stream_completion_creates_the_job_and_a_cached_program_queues_it() {
    let (case, e1) = exec_case(spec()).await;
    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Created,
        "after 1: the job waits for its build"
    );
    assert!(exec_events(&case).await.is_empty(), "after 1: no event");

    case.transition(Transition::SettleExecJobs).await;

    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Queued,
        "after 2: the job is queued"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        (events.len(), count_status(&events, ExecJobStatus::Queued)),
        (1, 1),
        "after 2: one queued event: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_failed_program_build_fails_the_job() {
    let (case, e1) = walked_exec(spec()).await;
    case.build_failed(A, BuildFailureKind::Permanent, &[]).await;
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: the open job keeps the evaluation building"
    );

    case.transition(Transition::SettleExecJobs).await;

    let job = job(&case, e1).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (ExecJobStatus::Failed, Some("the program build failed")),
        "after 2: the job"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 2: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_job_whose_project_switched_exec_jobs_off_fails_at_settle() {
    let (case, e1) = exec_case(spec()).await;
    case.sql("UPDATE project SET exec_enabled = false").await;

    case.transition(Transition::SettleExecJobs).await;

    let job = job(&case, e1).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (
            ExecJobStatus::Failed,
            Some("exec jobs are disabled for this project")
        ),
        "after 1: the job"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_non_zero_exit_restarts_until_no_retry_is_left() {
    let (case, e1) = queued_case(with_retries(1)).await;

    let first = assign(&case, e1, 1, 5).await;
    end_attempt(&case, first, AttemptEndReason::Failed).await;
    let requeued = job(&case, e1).await;
    assert_eq!(
        (requeued.status, requeued.retries_left),
        (ExecJobStatus::Queued, 0),
        "after 1: re-queued with the retry spent"
    );

    let second = assign(&case, e1, 2, 5).await;
    end_attempt(&case, second, AttemptEndReason::Failed).await;
    let failed = job(&case, e1).await;
    assert_eq!(
        (failed.status, failed.message.as_deref()),
        (ExecJobStatus::Failed, Some("the program exited with 1")),
        "after 2: the job failed"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        (
            count_status(&events, ExecJobStatus::Queued),
            count_status(&events, ExecJobStatus::Failed)
        ),
        (2, 1),
        "after 2: two queued events and one failed event: {events:?}"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 2: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn retries_are_clamped_to_the_worker_cap_at_dispatch() {
    let (case, e1) = queued_case(with_retries(5)).await;

    assign(&case, e1, 1, 2).await;

    let started = job(&case, e1).await;
    assert_eq!(
        (started.status, started.retries_left),
        (ExecJobStatus::Starting, 2),
        "after 1: the job"
    );
    case.finish().await;
}

#[tokio::test]
async fn ten_losses_in_a_row_fail_the_job() {
    let (case, e1) = queued_case(with_retries(3)).await;

    for number in 1..=9 {
        let attempt = assign(&case, e1, number, 5).await;
        end_attempt(&case, attempt, AttemptEndReason::Lost).await;
        let requeued = job(&case, e1).await;
        assert_eq!(
            (requeued.status, requeued.retries_left),
            (ExecJobStatus::Queued, 3),
            "after loss {number}: re-queued without a retry spent"
        );
    }

    let tenth = assign(&case, e1, 10, 5).await;
    end_attempt(&case, tenth, AttemptEndReason::Lost).await;

    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Failed,
        "after loss 10: the job failed"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Queued),
        10,
        "every re-queue is announced: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_stale_attempt_number_leaves_the_job_queued() {
    let (case, e1) = queued_case(with_retries(3)).await;
    let first = assign(&case, e1, 1, 5).await;
    end_attempt(&case, first, AttemptEndReason::Lost).await;

    let report = case
        .transition(Transition::ExecJobAssigned {
            exec_job: job(&case, e1).await.id,
            attempt: ExecJobAttemptId::now_v7(),
            number: 1,
            assignment: DispatchedJobId::now_v7(),
            worker: "w2".into(),
            retries_cap: 5,
        })
        .await;

    assert!(
        report.already_aborted,
        "after 2: the stale number is refused"
    );
    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Queued,
        "after 2: the job"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_abort_never_restarts() {
    let (case, e1) = queued_case(with_retries(3)).await;
    let attempt = assign(&case, e1, 1, 5).await;
    case.transition(Transition::ExecProgramStarted { attempt })
        .await;
    let exec_job = job(&case, e1).await.id;

    let report = case.transition(Transition::AbortExecJob { exec_job }).await;

    assert_eq!(
        report.aborted_exec_jobs,
        vec![exec_job],
        "after 1: the report"
    );
    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Aborted,
        "after 1: the job"
    );
    assert_eq!(
        attempt_row(&case, attempt).await.end_reason,
        Some(AttemptEndReason::Aborted),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_report_for_a_closed_attempt_changes_nothing() {
    let (case, e1) = queued_case(with_retries(3)).await;
    let attempt = assign(&case, e1, 1, 5).await;
    let exec_job = job(&case, e1).await.id;
    case.transition(Transition::AbortExecJob { exec_job }).await;
    let events = exec_events(&case).await;

    end_attempt(&case, attempt, AttemptEndReason::Aborted).await;
    end_attempt(&case, attempt, AttemptEndReason::Completed).await;

    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Aborted,
        "after 2: the job"
    );
    assert_eq!(exec_events(&case).await, events, "after 2: no new event");
    case.finish().await;
}

#[tokio::test]
async fn a_phase_is_recorded_and_announced() {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;

    case.transition(Transition::ExecPhase {
        attempt,
        name: "epoch-3".into(),
        payload: vec![1, 2],
    })
    .await;

    let phases = case.db.rows::<exec_job_phase::Entity>().await;
    assert_eq!(
        phases
            .iter()
            .map(|p| (p.name.as_str(), p.payload.clone()))
            .collect::<Vec<_>>(),
        vec![("epoch-3", vec![1, 2])],
        "after 1: the phase row"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        events
            .iter()
            .filter(|key| key.starts_with("exec_job.phase:"))
            .count(),
        1,
        "after 1: one phase event: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_program_start_moves_the_job_to_running_once() {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;

    case.transition(Transition::ExecProgramStarted { attempt })
        .await;
    case.transition(Transition::ExecProgramStarted { attempt })
        .await;

    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Running,
        "after 2: the job"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Running),
        1,
        "after 2: one started event: {events:?}"
    );
    case.finish().await;
}
