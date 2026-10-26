/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, RunnerJobAttemptId};
use gradient_entity::runner_job::RunnerJobStatus;
use gradient_entity::runner_job_attempt::AttemptEndReason;
use gradient_entity::{evaluation_message, runner_job, runner_job_attempt, runner_job_phase};
use gradient_fixtures::shape::shape;
use gradient_types::runner::{RunnerSpec, runner_image_attr};
use gradient_wire::types::{BuildFailureKind, DiscoveredDerivation};

use super::{A, A_OUT, B, B_OUT, Case, drv, nar, with_attr, with_output, with_references};
use crate::messages::{Transition, TransitionReport};

const ATTR: &str = "apps.x86_64-linux.a";

fn entry_point(path: &str, output: &str, attr: &str) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, &[]), output), attr)
}

fn spec() -> RunnerSpec {
    RunnerSpec {
        program: format!("{A_OUT}/bin/a"),
        nodes: 1,
        ..Default::default()
    }
}

fn runner_root() -> DiscoveredDerivation {
    runner_root_with(spec())
}

fn runner_root_with(spec: RunnerSpec) -> DiscoveredDerivation {
    DiscoveredDerivation {
        runner: Some(spec),
        ..entry_point(A, A_OUT, ATTR)
    }
}

fn image_root() -> DiscoveredDerivation {
    entry_point(B, B_OUT, &runner_image_attr(ATTR))
}

async fn switch_runner_jobs_on(case: &Case) {
    case.sql("UPDATE project SET runner_enabled = true").await;
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
async fn a_runner_entry_point_keeps_its_spec() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_runner_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    let points = case.entry_points(e1).await;
    assert_eq!(points.len(), 1, "after 1: one entry point");
    let stored: RunnerSpec =
        serde_json::from_value(points[0].runner.clone().expect("after 1: a runner column"))
            .expect("after 1: the spec decodes");
    assert_eq!(stored, spec(), "after 1: the stored spec");
    case.finish().await;
}

#[tokio::test]
async fn a_project_without_runner_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "runner jobs are disabled for this project".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_instance_without_runner_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start_without_runner_jobs(shape().evaluation(e1)).await;
    switch_runner_jobs_on(&case).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "runner jobs are disabled on this instance".to_owned(),
            Some(format!("nix-eval:{ATTR}"))
        )],
        "after 1: the refusal"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_refused_runner_inserts_neither_its_program_nor_its_image() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root(), image_root()]).await)
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
async fn a_refused_runner_skips_its_image_in_a_later_batch() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;

    case.walk(case.tasked(e1, vec![runner_root()]).await).await;
    case.walk(case.tasked(e1, vec![image_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 2: no entry point"
    );
    assert!(!case.sb(B).await.wanted, "after 2: the image is not wanted");
    case.finish().await;
}

async fn walked_runner(spec: RunnerSpec) -> (Case, EvaluationId) {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    switch_runner_jobs_on(&case).await;
    case.walk(case.tasked(e1, vec![runner_root_with(spec)]).await)
        .await;
    case.transition(Transition::EvalStreamCompleted { evaluation: e1 })
        .await;
    (case, e1)
}

async fn runner_case(spec: RunnerSpec) -> (Case, EvaluationId) {
    let (case, e1) = walked_runner(spec).await;
    case.build_started(A).await;
    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;
    case.build_completed(A).await;
    (case, e1)
}

async fn queued_case(spec: RunnerSpec) -> (Case, EvaluationId) {
    let (case, e1) = runner_case(spec).await;
    case.transition(Transition::SettleRunnerJobs).await;
    (case, e1)
}

fn with_retries(retries: u32) -> RunnerSpec {
    RunnerSpec { retries, ..spec() }
}

async fn job(case: &Case, evaluation: EvaluationId) -> runner_job::Model {
    let mut jobs: Vec<runner_job::Model> = case
        .db
        .rows::<runner_job::Entity>()
        .await
        .into_iter()
        .filter(|job| job.evaluation == evaluation)
        .collect();
    assert_eq!(jobs.len(), 1, "one runner job of {evaluation}");
    jobs.remove(0)
}

async fn assign(
    case: &Case,
    evaluation: EvaluationId,
    number: u32,
    retries_cap: u32,
) -> RunnerJobAttemptId {
    let attempt = RunnerJobAttemptId::now_v7();
    let report = case
        .transition(Transition::RunnerJobAssigned {
            runner_job: job(case, evaluation).await.id,
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
    attempt: RunnerJobAttemptId,
    reason: AttemptEndReason,
) -> TransitionReport {
    case.transition(Transition::RunnerAttemptEnded {
        attempt,
        reason,
        message: Some("the program exited with 1".into()),
    })
    .await
}

async fn attempt_row(case: &Case, attempt: RunnerJobAttemptId) -> runner_job_attempt::Model {
    case.db
        .find::<runner_job_attempt::Entity>(attempt)
        .await
        .unwrap_or_else(|| panic!("no attempt {attempt}"))
}

async fn runner_events(case: &Case) -> Vec<String> {
    case.db
        .texts("SELECT key FROM pending_delivery WHERE key LIKE 'runner_job%' ORDER BY id")
        .await
}

fn count_status(events: &[String], status: RunnerJobStatus) -> usize {
    let suffix = format!(":{}", i16::from(status));
    events
        .iter()
        .filter(|key| key.starts_with("runner_job:") && key.ends_with(&suffix))
        .count()
}

#[tokio::test]
async fn a_stream_completion_creates_the_job_and_a_cached_program_queues_it() {
    let (case, e1) = runner_case(spec()).await;
    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Created,
        "after 1: the job waits for its build"
    );
    assert!(runner_events(&case).await.is_empty(), "after 1: no event");

    case.transition(Transition::SettleRunnerJobs).await;

    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Queued,
        "after 2: the job is queued"
    );
    let events = runner_events(&case).await;
    assert_eq!(
        (events.len(), count_status(&events, RunnerJobStatus::Queued)),
        (1, 1),
        "after 2: one queued event: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_failed_program_build_fails_the_job() {
    let (case, e1) = walked_runner(spec()).await;
    case.build_failed(A, BuildFailureKind::Permanent, &[]).await;
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: the open job keeps the evaluation building"
    );

    case.transition(Transition::SettleRunnerJobs).await;

    let job = job(&case, e1).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (RunnerJobStatus::Failed, Some("the program build failed")),
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
async fn a_non_zero_exit_restarts_until_no_retry_is_left() {
    let (case, e1) = queued_case(with_retries(1)).await;

    let first = assign(&case, e1, 1, 5).await;
    end_attempt(&case, first, AttemptEndReason::Failed).await;
    let requeued = job(&case, e1).await;
    assert_eq!(
        (requeued.status, requeued.retries_left),
        (RunnerJobStatus::Queued, 0),
        "after 1: re-queued with the retry spent"
    );

    let second = assign(&case, e1, 2, 5).await;
    end_attempt(&case, second, AttemptEndReason::Failed).await;
    let failed = job(&case, e1).await;
    assert_eq!(
        (failed.status, failed.message.as_deref()),
        (RunnerJobStatus::Failed, Some("the program exited with 1")),
        "after 2: the job failed"
    );
    let events = runner_events(&case).await;
    assert_eq!(
        (
            count_status(&events, RunnerJobStatus::Queued),
            count_status(&events, RunnerJobStatus::Failed)
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
        (RunnerJobStatus::Starting, 2),
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
            (RunnerJobStatus::Queued, 3),
            "after loss {number}: re-queued without a retry spent"
        );
    }

    let tenth = assign(&case, e1, 10, 5).await;
    end_attempt(&case, tenth, AttemptEndReason::Lost).await;

    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Failed,
        "after loss 10: the job failed"
    );
    let events = runner_events(&case).await;
    assert_eq!(
        count_status(&events, RunnerJobStatus::Queued),
        10,
        "every re-queue is announced: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn an_abort_never_restarts() {
    let (case, e1) = queued_case(with_retries(3)).await;
    let attempt = assign(&case, e1, 1, 5).await;
    case.transition(Transition::RunnerProgramStarted { attempt })
        .await;
    let runner_job = job(&case, e1).await.id;

    let report = case
        .transition(Transition::AbortRunnerJob { runner_job })
        .await;

    assert_eq!(
        report.aborted_runner_jobs,
        vec![runner_job],
        "after 1: the report"
    );
    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Aborted,
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
    let runner_job = job(&case, e1).await.id;
    case.transition(Transition::AbortRunnerJob { runner_job })
        .await;
    let events = runner_events(&case).await;

    end_attempt(&case, attempt, AttemptEndReason::Aborted).await;
    end_attempt(&case, attempt, AttemptEndReason::Completed).await;

    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Aborted,
        "after 2: the job"
    );
    assert_eq!(runner_events(&case).await, events, "after 2: no new event");
    case.finish().await;
}

#[tokio::test]
async fn a_phase_is_recorded_and_announced() {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;

    case.transition(Transition::RunnerPhase {
        attempt,
        name: "epoch-3".into(),
        payload: vec![1, 2],
    })
    .await;

    let phases = case.db.rows::<runner_job_phase::Entity>().await;
    assert_eq!(
        phases
            .iter()
            .map(|p| (p.name.as_str(), p.payload.clone()))
            .collect::<Vec<_>>(),
        vec![("epoch-3", vec![1, 2])],
        "after 1: the phase row"
    );
    let events = runner_events(&case).await;
    assert_eq!(
        events
            .iter()
            .filter(|key| key.starts_with("runner_job.phase:"))
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

    case.transition(Transition::RunnerProgramStarted { attempt })
        .await;
    case.transition(Transition::RunnerProgramStarted { attempt })
        .await;

    assert_eq!(
        job(&case, e1).await.status,
        RunnerJobStatus::Running,
        "after 2: the job"
    );
    let events = runner_events(&case).await;
    assert_eq!(
        count_status(&events, RunnerJobStatus::Running),
        1,
        "after 2: one started event: {events:?}"
    );
    case.finish().await;
}
