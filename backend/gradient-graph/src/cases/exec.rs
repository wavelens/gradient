/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::exec_jobs::continuations;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::exec_job_continuation::ContinuationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId, ExecJobAttemptId, ExecJobId};
use gradient_entity::{
    cluster_job, cluster_member, evaluation_message, exec_job, exec_job_attempt,
    exec_job_continuation, exec_job_phase, exec_job_token,
};
use gradient_fixtures::shape::shape;
use gradient_types::exec::{ExecSpec, exec_image_attr};
use gradient_wire::types::{BuildFailureKind, DiscoveredDerivation};

use super::{A, A_OUT, B, B_OUT, Case, drv, nar, with_attr, with_output, with_references};
use crate::messages::{
    ClusterNext, ContinuationOutcome, ExecNodeEnd, GcRequest, Transition, TransitionReport,
};

const ATTR: &str = "apps.x86_64-linux.a";
const MISSING_SECRET: &str =
    "secret \"hf-token\" not found in project secrets, repository sops files or worker sops files";

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
    case.sql("UPDATE task SET exec_enabled = true").await;
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
async fn a_task_without_exec_jobs_fails_the_attribute_with_its_message() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(shape().evaluation(e1)).await;
    case.sql("UPDATE project SET exec_enabled = true").await;

    case.walk(case.tasked(e1, vec![exec_root()]).await).await;

    assert!(
        case.entry_points(e1).await.is_empty(),
        "after 1: no entry point"
    );
    assert_eq!(
        messages(&case, e1).await,
        vec![(
            "exec jobs are disabled for this task".to_owned(),
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
            token_hash: format!("h{number}"),
            token_expires_at: gradient_types::now() + chrono::Duration::hours(1),
            master_port: None,
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
async fn a_job_whose_task_switched_exec_jobs_off_fails_at_settle() {
    let (case, e1) = exec_case(spec()).await;
    case.sql("UPDATE task SET exec_enabled = false").await;

    case.transition(Transition::SettleExecJobs).await;

    let job = job(&case, e1).await;
    assert_eq!(
        (job.status, job.message.as_deref()),
        (
            ExecJobStatus::Failed,
            Some("exec jobs are disabled for this task")
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
            token_hash: "stale".into(),
            token_expires_at: gradient_types::now(),
            master_port: None,
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

#[tokio::test]
async fn a_job_failed_before_dispatch_ends_failed_without_a_retry() {
    let (case, e1) = queued_case(with_retries(2)).await;

    case.transition(Transition::FailExecJob {
        exec_job: job(&case, e1).await.id,
        message: MISSING_SECRET.into(),
    })
    .await;

    let failed = job(&case, e1).await;
    assert_eq!(
        (
            failed.status,
            failed.message.as_deref(),
            failed.retries_left
        ),
        (ExecJobStatus::Failed, Some(MISSING_SECRET), 2),
        "after 1: the job"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Failed),
        1,
        "after 1: one failed event: {events:?}"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_job_failed_at_hand_out_closes_its_attempt() {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;

    case.transition(Transition::FailExecJob {
        exec_job: job(&case, e1).await.id,
        message: MISSING_SECRET.into(),
    })
    .await;

    assert_eq!(
        job(&case, e1).await.status,
        ExecJobStatus::Failed,
        "after 1: the job"
    );
    assert_eq!(
        attempt_row(&case, attempt).await.end_reason,
        Some(AttemptEndReason::Failed),
        "after 1: the attempt"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_finished_job_ignores_a_late_failure() {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;
    case.transition(Transition::ExecProgramStarted { attempt })
        .await;
    end_attempt(&case, attempt, AttemptEndReason::Completed).await;
    let events = exec_events(&case).await;

    case.transition(Transition::FailExecJob {
        exec_job: job(&case, e1).await.id,
        message: MISSING_SECRET.into(),
    })
    .await;

    let finished = job(&case, e1).await;
    assert_eq!(
        (finished.status, finished.message),
        (ExecJobStatus::Completed, None),
        "after 1: the job"
    );
    assert_eq!(exec_events(&case).await, events, "after 1: no new event");
    case.finish().await;
}

const B_ATTR: &str = "packages.x86_64-linux.b";

async fn tokens(case: &Case) -> Vec<(String, bool)> {
    let mut rows: Vec<(String, bool)> = case
        .db
        .rows::<exec_job_token::Entity>()
        .await
        .into_iter()
        .map(|token| (token.token_hash, token.revoked))
        .collect();
    rows.sort();
    rows
}

async fn continuation_rows(case: &Case) -> Vec<exec_job_continuation::Model> {
    case.db.rows::<exec_job_continuation::Entity>().await
}

async fn record_continue_file(case: &Case, evaluation: EvaluationId, attributes: &[&str]) {
    let attributes: Vec<String> = attributes.iter().map(|a| (*a).to_owned()).collect();
    let recorded = continuations::record(
        &case.db.connection(),
        job(case, evaluation).await.id,
        evaluation,
        &attributes,
    )
    .await
    .expect("the continue file is recorded");
    assert!(recorded, "the open job takes the continue file");
}

async fn continued_case() -> (Case, EvaluationId, ExecJobId) {
    let (case, e1) = queued_case(spec()).await;
    let attempt = assign(&case, e1, 1, 5).await;
    record_continue_file(&case, e1, &[ATTR, B_ATTR]).await;
    end_attempt(&case, attempt, AttemptEndReason::Completed).await;
    let exec_job = job(&case, e1).await.id;
    (case, e1, exec_job)
}

#[tokio::test]
async fn a_dispatched_attempt_gets_one_token_and_its_end_revokes_it() {
    let (case, e1) = queued_case(with_retries(1)).await;

    let first = assign(&case, e1, 1, 5).await;
    assert_eq!(
        tokens(&case).await,
        vec![("h1".to_owned(), false)],
        "after 1: one live token"
    );

    end_attempt(&case, first, AttemptEndReason::Lost).await;
    let second = assign(&case, e1, 2, 5).await;
    assert_eq!(
        tokens(&case).await,
        vec![("h1".to_owned(), true), ("h2".to_owned(), false)],
        "after 2: the lost attempt's token is revoked, the new one is live"
    );

    end_attempt(&case, second, AttemptEndReason::Completed).await;
    assert_eq!(
        tokens(&case).await,
        vec![("h1".to_owned(), true), ("h2".to_owned(), true)],
        "after 3: every token is revoked"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_continue_file_adds_entry_points_once_and_keeps_the_evaluation_building() {
    let (case, e1, exec_job) = continued_case().await;
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: the queued continuation keeps the evaluation building"
    );

    let report = case
        .walk(crate::messages::RecordBatch {
            continuation: Some(exec_job),
            ..case
                .tasked(e1, vec![exec_root(), entry_point(B, B_OUT, B_ATTR)])
                .await
        })
        .await;
    assert!(!report.skipped, "after 2: the continuation batch is taken");
    let names: Vec<String> = case
        .entry_points(e1)
        .await
        .into_iter()
        .map(|point| point.eval)
        .collect();
    assert_eq!(
        names.iter().filter(|name| *name == ATTR).count(),
        1,
        "after 2: {names:?}"
    );
    assert!(names.contains(&B_ATTR.to_owned()), "after 2: {names:?}");

    case.transition(Transition::ContinuationEnded {
        exec_job,
        outcome: ContinuationOutcome::Done,
    })
    .await;
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 3: build b is still open"
    );

    case.build_started(B).await;
    case.commit_nar(nar(B_OUT, "b")).await;
    case.build_completed(B).await;
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 4: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_continue_file_from_a_failed_attempt_is_forgotten_on_restart() {
    let (case, e1) = queued_case(with_retries(1)).await;
    let first = assign(&case, e1, 1, 5).await;
    record_continue_file(&case, e1, &[B_ATTR]).await;
    end_attempt(&case, first, AttemptEndReason::Failed).await;

    let second = assign(&case, e1, 2, 5).await;
    end_attempt(&case, second, AttemptEndReason::Completed).await;

    assert!(
        continuation_rows(&case).await.is_empty(),
        "after 2: no continuation"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 2: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_batch_for_a_building_evaluation_without_a_continuation_is_dropped() {
    let (case, e1) = exec_case(spec()).await;

    let report = case
        .walk(case.tasked(e1, vec![entry_point(B, B_OUT, B_ATTR)]).await)
        .await;

    assert!(report.skipped, "after 1: the batch is dropped");
    assert!(
        !case
            .entry_points(e1)
            .await
            .iter()
            .any(|point| point.eval == B_ATTR),
        "after 1: no new entry point"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_failed_continuation_fails_the_evaluation_with_its_message() {
    let (case, e1, exec_job) = continued_case().await;

    case.transition(Transition::ContinuationEnded {
        exec_job,
        outcome: ContinuationOutcome::Failed {
            error: "attribute 'b' missing".into(),
        },
    })
    .await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: the evaluation"
    );
    assert!(
        messages(&case, e1).await.contains(&(
            "continue.json: attribute 'b' missing".to_owned(),
            Some("nix-eval".to_owned())
        )),
        "after 1: the message"
    );
    case.finish().await;
}

#[tokio::test]
async fn deleting_an_evaluation_reports_its_exec_jobs() {
    let (case, e1) = queued_case(spec()).await;
    let exec_job = job(&case, e1).await.id;

    let report = case.gc(GcRequest::Evaluations { ids: vec![e1] }).await;

    assert_eq!(
        report.deleted_exec_jobs,
        vec![exec_job],
        "after 1: the report"
    );
    case.finish().await;
}

const MASTER_PORT: u16 = 29500;

async fn exec_cluster_case(nodes: u32, keep_running: bool, retries: u32) -> (Case, EvaluationId) {
    queued_case(ExecSpec {
        nodes,
        keep_running,
        retries,
        ..spec()
    })
    .await
}

async fn node_rows(case: &Case, evaluation: EvaluationId) -> Vec<exec_job::Model> {
    let mut rows: Vec<exec_job::Model> = case
        .db
        .rows::<exec_job::Entity>()
        .await
        .into_iter()
        .filter(|job| job.evaluation == evaluation)
        .collect();
    rows.sort_by_key(|job| job.node_index);
    rows
}

async fn assign_node(case: &Case, node: &exec_job::Model, number: u32) -> ExecJobAttemptId {
    let attempt = ExecJobAttemptId::now_v7();
    let report = case
        .transition(Transition::ExecJobAssigned {
            exec_job: node.id,
            attempt,
            number,
            assignment: DispatchedJobId::now_v7(),
            worker: format!("w{}", node.node_index),
            retries_cap: 5,
            token_hash: format!("h{}-{number}", node.node_index),
            token_expires_at: gradient_types::now() + chrono::Duration::hours(1),
            master_port: Some(MASTER_PORT),
        })
        .await;
    assert!(
        !report.already_aborted,
        "node {} attempt {number} opens",
        node.node_index
    );
    attempt
}

async fn running_cluster(case: &Case, evaluation: EvaluationId) -> Vec<ExecJobAttemptId> {
    let mut attempts = Vec::new();
    for node in node_rows(case, evaluation).await {
        let attempt = assign_node(case, &node, 1).await;
        case.transition(Transition::ExecProgramStarted { attempt })
            .await;
        attempts.push(attempt);
    }
    attempts
}

fn node_end(attempt: ExecJobAttemptId, reason: AttemptEndReason) -> ExecNodeEnd {
    ExecNodeEnd {
        attempt,
        reason,
        message: (reason != AttemptEndReason::Completed).then(|| "node ended".to_owned()),
    }
}

async fn settle_cluster(
    case: &Case,
    attempts: &[ExecJobAttemptId],
    reasons: [AttemptEndReason; 2],
    next: ClusterNext,
) {
    case.transition(Transition::ExecClusterSettled {
        nodes: attempts
            .iter()
            .zip(reasons)
            .map(|(attempt, reason)| node_end(*attempt, reason))
            .collect(),
        next,
    })
    .await;
}

fn events_of(events: &[String], job: ExecJobId) -> usize {
    let prefix = format!("exec_job:{job}:");
    events.iter().filter(|key| key.starts_with(&prefix)).count()
}

#[tokio::test]
async fn a_stream_completion_creates_one_row_per_node_and_their_cluster() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;

    let rows = node_rows(&case, e1).await;
    assert_eq!(
        rows.iter().map(|job| job.node_index).collect::<Vec<_>>(),
        vec![0, 1],
        "after 1: one row per node"
    );
    assert_eq!(
        case.db.rows::<cluster_job::Entity>().await.len(),
        1,
        "after 1: one cluster job"
    );
    let members = case.db.rows::<cluster_member::Entity>().await;
    let mut targets: Vec<_> = members.iter().map(|m| m.exec_job).collect();
    targets.sort();
    let mut expected: Vec<_> = rows.iter().map(|job| Some(job.id)).collect();
    expected.sort();
    assert_eq!(targets, expected, "after 1: one member per node");
    let events = exec_events(&case).await;
    assert_eq!(
        (
            count_status(&events, ExecJobStatus::Queued),
            events_of(&events, rows[0].id)
        ),
        (1, 1),
        "after 1: one queued event under node 0: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn the_master_port_is_written_on_every_node_attempt() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;

    let attempts = running_cluster(&case, e1).await;

    for attempt in attempts {
        assert_eq!(
            attempt_row(&case, attempt).await.master_port,
            Some(i32::from(MASTER_PORT)),
            "after 1: the attempt {attempt}"
        );
    }
    case.finish().await;
}

#[tokio::test]
async fn a_cluster_restart_requeues_every_node_and_consumes_one_retry() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    let attempts = running_cluster(&case, e1).await;
    let before = exec_events(&case).await;

    settle_cluster(
        &case,
        &attempts,
        [AttemptEndReason::Failed, AttemptEndReason::Aborted],
        ClusterNext::Restart {
            consume_retry: true,
        },
    )
    .await;

    let rows = node_rows(&case, e1).await;
    assert!(
        rows.iter()
            .all(|row| (row.status, row.retries_left) == (ExecJobStatus::Queued, 0)),
        "after 1: every node queued with the retry spent: {rows:?}"
    );
    assert_eq!(
        attempt_row(&case, attempts[0]).await.end_reason,
        Some(AttemptEndReason::Failed),
        "after 1: node 0's attempt"
    );
    assert_eq!(
        attempt_row(&case, attempts[1]).await.end_reason,
        Some(AttemptEndReason::Aborted),
        "after 1: node 1's attempt"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Queued) - count_status(&before, ExecJobStatus::Queued),
        1,
        "after 1: one queued event: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_lost_node_restart_keeps_the_retries() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    let attempts = running_cluster(&case, e1).await;

    settle_cluster(
        &case,
        &attempts,
        [AttemptEndReason::Aborted, AttemptEndReason::Lost],
        ClusterNext::Restart {
            consume_retry: false,
        },
    )
    .await;

    let rows = node_rows(&case, e1).await;
    assert!(
        rows.iter()
            .all(|row| (row.status, row.retries_left) == (ExecJobStatus::Queued, 1)),
        "after 1: every node queued with its retry: {rows:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_kept_failure_ends_the_job_failed_once() {
    let (case, e1) = exec_cluster_case(2, true, 1).await;
    let attempts = running_cluster(&case, e1).await;

    settle_cluster(
        &case,
        &attempts,
        [AttemptEndReason::Completed, AttemptEndReason::Failed],
        ClusterNext::End,
    )
    .await;

    let rows = node_rows(&case, e1).await;
    assert_eq!(
        rows.iter().map(|row| row.status).collect::<Vec<_>>(),
        vec![ExecJobStatus::Completed, ExecJobStatus::Failed],
        "after 1: the nodes"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Failed),
        1,
        "after 1: one failed event: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|key| key.starts_with(&format!("exec_job:{}:", rows[0].id))
                && key.ends_with(&format!(":{}", i16::from(ExecJobStatus::Failed)))),
        "after 1: the failed event is node 0's: {events:?}"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: the evaluation"
    );
    case.finish().await;
}

#[tokio::test]
async fn a_survivor_report_after_the_restart_changes_nothing() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    let attempts = running_cluster(&case, e1).await;
    settle_cluster(
        &case,
        &attempts,
        [AttemptEndReason::Failed, AttemptEndReason::Aborted],
        ClusterNext::Restart {
            consume_retry: true,
        },
    )
    .await;
    let events = exec_events(&case).await;

    end_attempt(&case, attempts[1], AttemptEndReason::Aborted).await;

    let rows = node_rows(&case, e1).await;
    assert!(
        rows.iter().all(|row| row.status == ExecJobStatus::Queued),
        "after 2: both nodes stay queued: {rows:?}"
    );
    assert_eq!(exec_events(&case).await, events, "after 2: no new event");
    case.finish().await;
}

#[tokio::test]
async fn aborting_the_job_aborts_every_node() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    running_cluster(&case, e1).await;
    let rows = node_rows(&case, e1).await;

    let report = case
        .transition(Transition::AbortExecJob {
            exec_job: rows[0].id,
        })
        .await;

    assert_eq!(
        report.aborted_exec_jobs,
        vec![rows[0].id, rows[1].id],
        "after 1: the report"
    );
    assert!(
        node_rows(&case, e1)
            .await
            .iter()
            .all(|row| row.status == ExecJobStatus::Aborted),
        "after 1: every node is aborted"
    );
    let events = exec_events(&case).await;
    assert_eq!(
        count_status(&events, ExecJobStatus::Aborted),
        1,
        "after 1: one aborted event: {events:?}"
    );
    case.finish().await;
}

#[tokio::test]
async fn phases_of_every_node_are_announced_with_their_node() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    let attempts = running_cluster(&case, e1).await;

    case.transition(Transition::ExecPhase {
        attempt: attempts[1],
        name: "epoch-1".into(),
        payload: Vec::new(),
    })
    .await;

    let nodes = case
        .db
        .texts(
            "SELECT jsonb_path_query_first(payload, 'lax $.**.node')::text \
             FROM pending_delivery WHERE key LIKE 'exec_job.phase:%'",
        )
        .await;
    assert_eq!(nodes, vec!["1".to_owned()], "after 1: one phase of node 1");
    case.finish().await;
}

#[tokio::test]
async fn the_continue_file_of_node_0_is_activated_when_every_node_completes() {
    let (case, e1) = exec_cluster_case(2, false, 1).await;
    let attempts = running_cluster(&case, e1).await;
    let rows = node_rows(&case, e1).await;
    for row in &rows {
        let recorded =
            continuations::record(&case.db.connection(), row.id, e1, &[B_ATTR.to_owned()])
                .await
                .expect("the continue file is recorded");
        assert!(recorded, "node {} takes the continue file", row.node_index);
    }

    settle_cluster(
        &case,
        &attempts,
        [AttemptEndReason::Completed, AttemptEndReason::Completed],
        ClusterNext::End,
    )
    .await;

    let status_of = |job: ExecJobId, rows: &[exec_job_continuation::Model]| {
        rows.iter()
            .find(|row| row.exec_job == job)
            .map(|row| row.status)
    };
    let continued = continuation_rows(&case).await;
    assert_eq!(
        (
            status_of(rows[0].id, &continued),
            status_of(rows[1].id, &continued)
        ),
        (
            Some(ContinuationStatus::Queued),
            Some(ContinuationStatus::Recorded)
        ),
        "after 1: only node 0's continue file is queued"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: the evaluation"
    );
    case.finish().await;
}
