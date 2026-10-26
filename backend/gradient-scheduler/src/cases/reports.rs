/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{CacheId, DispatchedJobId, EvaluationId};
use gradient_entity::pending_delivery::PendingDeliveryKind;
use gradient_fixtures::shape::shape;
use gradient_pool::session_port::SessionSignal;
use gradient_types::{EvalCapability, WaitingReason};
use gradient_wire::types::{
    BuildFailureKind, BuildMetrics, BuildSpecKind, BumpedInputWire, EvalMessageLevel,
    EvalStatsReport, FlakeSource, FlakeStep, Job, JobKind, JobPhase, JobPhaseSpan,
};

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, CaseShapes, SchedulerCase, ago, attr_of, build_output, drv,
    drv_nar, eval_key, failure, is_recent, job_ids, output_nar, span, w_all, w_eval,
    w_network_eval, with_attr, with_output,
};
use crate::jobs::PendingJob;

const W1: &str = "w1";
const W2: &str = "w2";
const SOURCE: &str = "/nix/store/src1-source";

#[tokio::test]
async fn r01_the_building_report_moves_queued_to_building_and_opens_the_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    claim(&case, &[W1], W1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(started(&case, A).await, vec![], "after 1: att(A) == []");
    case.signals(W1);
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::Building, 0),
        "after 2: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        started(&case, A).await,
        vec![(AttemptOutcome::Running, true)],
        "after 2: att(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 1],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    let before = (case.sb(A).await, case.attempts(A).await);
    case.report_building(W1, &a).await;
    assert_eq!(
        (case.sb(A).await, case.attempts(A).await),
        before,
        "after 3: sb(A), att(A) unchanged"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r02_a_completion_without_a_building_report_leaves_no_second_running_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1, W2], W1).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert_eq!(started(&case, A).await, vec![], "after 2: att(A) == []");
    case.check("step 2").await;

    case.resync_startable_set().await;
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.offers_delta(W2).await;
    case.score_all(W2).await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert!(taken.is_none(), "after 4: result == nothing");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r03_a_failure_after_a_completion_and_a_second_completion_change_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, vec![span(JobPhase::Build, 0, 5)], 5)
        .await;
    let closed = vec![(a1, Some(DispatchedJobOutcome::Completed), Some(5))];
    assert_eq!(elapsed(&case, &a).await, closed, "after 2: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 2: evaluation(E1).status"
    );
    let phases = case.phases(a1).await.len();
    case.check("step 2").await;

    let late = failure(BuildFailureKind::Permanent)
        .error("late")
        .spans(vec![span(JobPhase::Build, 0, 6)])
        .elapsed_ms(6);
    case.fail(W1, &a, a1, late).await;
    assert_eq!(elapsed(&case, &a).await, closed, "after 3: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(AttemptOutcome::Built, None)],
        "after 3: att(A)"
    );
    case.check("step 3").await;

    case.complete(W1, &a, a1, vec![span(JobPhase::Build, 0, 7)], 7)
        .await;
    assert_eq!(elapsed(&case, &a).await, closed, "after 4: dj(build:A)");
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(AttemptOutcome::Built, None)],
        "after 4: att(A)"
    );
    assert_eq!(
        case.phases(a1).await.len(),
        phases,
        "after 4: dispatched_job_phase(a1) rows as after 2"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r04_each_build_output_writes_a_metric_row() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;
    let metrics = BuildMetrics {
        peak_ram_mb: Some(100),
        cpu_time_ms: Some(2000),
        build_time_ms: Some(3000),
        concurrent_builds: Some(1),
        ..Default::default()
    };

    claim(&case, &[W1], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    let outputs = vec![build_output(A, A_OUT)];
    case.report_build_output(W1, &a, outputs.clone(), false, Some(metrics.clone()))
        .await;
    let pname = case.db.derivation(A).await.history_name().to_owned();
    let rows: Vec<_> = case
        .derivation_metrics(A)
        .await
        .into_iter()
        .map(|m| {
            (
                m.peak_ram_mb,
                m.cpu_time_ms,
                m.build_time_ms,
                m.worker_id,
                m.architecture,
                m.pname,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![(
            Some(100),
            Some(2000),
            Some(3000),
            W1.to_owned(),
            super::X86.to_owned(),
            Some(pname),
        )],
        "after 2: derivation_metric(A)"
    );
    case.check("step 2").await;

    case.report_build_output(W1, &a, outputs, false, Some(metrics))
        .await;
    assert_eq!(
        case.derivation_metrics(A).await.len(),
        2,
        "after 3: derivation_metric(A) rows"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r05_a_report_for_a_reaped_job_leaves_the_closed_row_and_the_graph_alone() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    case.reap_overdue_aborts(Duration::ZERO).await;
    let reaped = vec![(a1, Some(DispatchedJobOutcome::Abandoned))];
    assert_eq!(
        case.assignment_outcomes(&a).await,
        reaped,
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    case.signals(W1);
    case.check("step 2").await;

    let rows = case.assignments(&a).await;
    let attempts = case.attempts(A).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(case.assignments(&a).await, rows, "after 3: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 3: att(A)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E1).status"
    );
    case.check("step 3").await;

    case.report_building(W1, &a).await;
    assert_eq!(case.assignments(&a).await, rows, "after 4: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 4: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 4: att(A)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r06_a_report_with_the_id_of_a_rejected_claim_is_dropped_once_another_worker_runs_it() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1, W2], W1).await;
    case.reject(W1, &a).await;
    case.check("step 1").await;

    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:A")
        .assignment_id();
    case.report_building(W2, &a).await;
    case.check("step 2").await;

    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 3: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W2),
        "after 3: active(build:A)"
    );
    case.check("step 3").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W2, &a, a2, Vec::new(), 9).await;
    assert_eq!(
        elapsed(&case, &a).await.get(1).cloned(),
        Some((a2, Some(DispatchedJobOutcome::Completed), Some(9))),
        "after 4: dj(build:A)[1]"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 4: sb(A).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r07_a_failure_with_an_old_id_of_a_lost_worker_does_not_fail_the_build_of_another() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1, W2], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.disconnect(W1).await;
    case.check("step 2").await;

    case.offers_delta(W2).await;
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: W2 gets build:A")
        .assignment_id();
    case.report_building(W2, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 3: sb(A).status"
    );
    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.reason, t.dispatched_job))
        .collect();
    assert!(
        matches!(
            attempts.as_slice(),
            [
                (AttemptOutcome::Aborted, Some(AttemptFailureReason::WorkerLost), _),
                (AttemptOutcome::Running, None, Some(running)),
            ] if *running == a2
        ),
        "after 3: att(A) == [{{Aborted, WorkerLost}}, {{Running, a2}}], got {attempts:?}"
    );
    case.check("step 3").await;

    let attempts = case.attempts(A).await;
    case.connect(W1, w_all(), &[]).await;
    let old = failure(BuildFailureKind::Permanent)
        .error("old")
        .elapsed_ms(1);
    case.fail(W1, &a, a1, old).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 4: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 4: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 4: att(A)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 4: evaluation(E1).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r08_a_transient_failure_retries_with_backoff_then_fails_permanently() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with(shape().one(e1, A, A_OUT), |c| {
        c.build.retry_backoff_secs = 1;
    })
    .await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    let a1 = claim(&case, &[W1], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.fail(W1, &a, a1, transient("flaky")).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Failed],
        "after 2: att(A)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 1),
        "after 2: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    case.check("step 2").await;

    retry_after(&case, 1.2).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    let a2 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.fail(W1, &a, a2, transient("")).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 2),
        "after 4: sb(A).status, sb(A).attempt"
    );
    case.check("step 4").await;

    retry_after(&case, 2.5).await;
    case.check("step 5").await;

    let a3 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.fail(W1, &a, a3, transient("")).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 6: sb(A).status"
    );
    let failed: Vec<_> = case
        .attempt_outcomes_with_reasons(A)
        .await
        .into_iter()
        .map(|(outcome, _)| outcome)
        .collect();
    assert_eq!(
        failed,
        vec![AttemptOutcome::Failed; 3],
        "after 6: att(A) has 3 Failed rows"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 6: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 1, 0, 0],
        "after 6: counters(E1)"
    );
    assert!(!case.pending(&a).await, "after 6: pending(build:A)");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn r09_a_permanent_failure_fails_the_dependents_in_both_evaluations() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
        .chain(e1)
        .evaluation(e2)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(C)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(C))
        .job()
        .output(C_OUT)
        .depends_on(B)
        .derivation(B)
        .job();
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let b = case.key(B).await;

    let a1 = claim(&case, &[W1], W1).await;
    assert_eq!(
        case.open_assignment(&b).await.map(|row| row.id),
        Some(a1),
        "after 1: W1 runs build:B"
    );
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    let exit = failure(BuildFailureKind::Permanent)
        .error("exit 1")
        .elapsed_ms(8);
    case.fail(W1, &b, a1, exit).await;
    assert_eq!(
        elapsed(&case, &b).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed), Some(8))],
        "after 2: dj(build:B)"
    );
    let attempts = case.attempts(B).await;
    let summary: Vec<_> = attempts
        .iter()
        .map(|t| (t.outcome, t.reason, t.failure_message.as_deref()))
        .collect();
    assert_eq!(
        summary,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::BuilderNonzero),
            Some("exit 1"),
        )],
        "after 2: att(B)"
    );
    let statuses = (
        case.sb(B).await.status,
        case.sb(A).await.status,
        case.sb(C).await.status,
    );
    assert_eq!(
        statuses,
        (
            BuildStatus::FailedPermanent,
            BuildStatus::DependencyFailed,
            BuildStatus::DependencyFailed,
        ),
        "after 2: sb(B), sb(A), sb(C)"
    );
    for (name, e) in [("E1", e1), ("E2", e2)] {
        assert_eq!(
            case.evaluation(e).await.status,
            EvaluationStatus::Failed,
            "after 2: evaluation({name}).status"
        );
        assert_eq!(
            case.counters(e).await,
            [2, 0, 2, 0, 0],
            "after 2: counters({name})"
        );
    }
    let finalize = attempts[0].id.to_string();
    assert!(
        case.deliveries()
            .await
            .iter()
            .any(|d| d.kind == PendingDeliveryKind::LogFinalize && d.key == finalize),
        "after 2: pending_delivery has LogFinalize for att(B)[0]"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r10_a_timeout_failure_is_failed_timeout_and_fails_the_dependent() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    case.connect(W1, w_all(), &[]).await;
    let b = case.key(B).await;

    let a1 = claim(&case, &[W1], W1).await;
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    let timeout = failure(BuildFailureKind::Timeout).elapsed_ms(14_400_000);
    case.fail(W1, &b, a1, timeout).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTimeout,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(B).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::WallClockTimeout)
        )],
        "after 2: att(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r11_a_second_substitute_miss_turns_the_passthrough_into_a_real_build() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .passthrough(e1)
        .depends_on(B)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    let attempt = case.sb(A).await.attempt;

    offered(&case, &[W1]).await;
    let a1 = substitute(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.fail(W1, &a, a1, failure(BuildFailureKind::SubstituteUnavailable))
        .await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.cache_available, shared.attempt),
        (BuildStatus::Queued, true, attempt),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::SubstituteUnavailable)
        )],
        "after 2: att(A)"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(
        case.pending(&a).await,
        "after 3: pending(build:A) after the admission"
    );
    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let a2 = substitute(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.check("step 3").await;

    case.fail(W1, &a, a2, failure(BuildFailureKind::SubstituteUnavailable))
        .await;
    let shared = case.sb(A).await;
    assert_eq!(
        (
            shared.status,
            shared.cache_available,
            shared.attempt,
            shared.blocking_deps
        ),
        (BuildStatus::Created, false, 0, 1),
        "after 4: sb(A)"
    );
    assert_eq!(
        case.output_of(A, "out").await.external_url,
        None,
        "after 4: output(A, out).external_url"
    );
    let dependency = case.sb(B).await;
    assert_eq!(
        (dependency.status, dependency.wanted),
        (BuildStatus::Queued, true),
        "after 4: sb(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 4: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&a).await, "after 5: pending(build:A)");
    assert!(case.pending(&b).await, "after 5: pending(build:B)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn r12_inputs_unavailable_removes_the_missing_input_and_retries_after_it_is_back() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .depends_on(B)
        .build_status(BuildStatus::Queued)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(B);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    let a1 = claim(&case, &[W1], W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    let missing = failure(BuildFailureKind::InputsUnavailable)
        .error("404")
        .missing(&[super::store_path(B_OUT, "b")])
        .elapsed_ms(2);
    case.fail(W1, &a, a1, missing).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::InputsUnavailable)
        )],
        "after 2: att(A)"
    );
    assert!(
        case.cached_path(B_OUT).await.is_none(),
        "after 2: cached_path(B_OUT) == none"
    );
    let dependency = case.sb(B).await;
    assert_eq!(
        (dependency.status, dependency.fetchable),
        (BuildStatus::Queued, false),
        "after 2: sb(B)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.blocking_deps),
        (BuildStatus::FailedTransient, 1),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r13_a_canceled_build_closes_the_row_abandoned_and_requeues_without_an_attempt() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;
    let attempt = case.sb(A).await.attempt;

    let a1 = claim(&case, &[W1, W2], W1).await;
    case.report_building(W1, &a).await;
    case.signals(W2);
    case.check("step 1").await;

    let canceled = failure(BuildFailureKind::Canceled)
        .error("worker draining")
        .elapsed_ms(3);
    case.fail(W1, &a, a1, canceled).await;
    assert_eq!(
        elapsed(&case, &a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned), Some(3))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Aborted],
        "after 2: att(A)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::Queued, attempt),
        "after 2: sb(A)"
    );
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W2), vec![], "after 2: signals(W2)");
    case.check("step 2").await;

    case.admit_startable_moves().await;
    case.offers_delta(W2).await;
    case.score_all(W2).await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 3: result.job_id"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Aborted],
        "after 3: att(A)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r14_a_canceled_eval_job_puts_the_evaluation_back_to_queued() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingFlake)
        .await;
    case.check("step 0").await;

    case.fail(
        W1,
        &k,
        a1,
        failure(BuildFailureKind::Canceled).elapsed_ms(3),
    )
    .await;
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 1: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        evaluation.status,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert!(
        evaluation.eval_flake_started_at.is_some(),
        "after 1: eval_flake_started_at kept"
    );
    assert_eq!(case.active(&k).await, None, "after 1: active(eval:E1)");
    case.signals(W1);
    case.check("step 1").await;

    case.assign_queued_evals().await;
    assert!(case.pending(&k).await, "after 2: pending(eval:E1)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Offers(_)]),
        "after 2: signals(W1) == [Offers(_)], got {signals:?}"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn a_corruption_within_the_heal_window_fails_instead_of_healing_again() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .into_cache(CacheId::now_v7())
            .eval_cache("F"),
    )
    .await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    case.fail(W1, &k, a1, corrupt()).await;
    let row = case.eval_cache("F").await.expect("after 1: row F kept");
    assert!(row.purged_at.is_some(), "after 1: row F purged");
    assert_eq!(row.size_bytes, 0, "after 1: row F emptied");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 1: dj(eval:E1)"
    );
    case.check("step 1").await;

    set_purged_at(&case, "F", Duration::from_secs(2 * 3600)).await;
    let a2 = evaluate_again(&case, &k).await;
    case.fail(W1, &k, a2, corrupt()).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 2: a purge 2 h ago heals again"
    );
    case.check("step 2").await;

    set_purged_at(&case, "F", Duration::from_secs(60)).await;
    let a3 = evaluate_again(&case, &k).await;
    case.fail(W1, &k, a3, corrupt()).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 3: a purge 1 min ago fails the evaluation"
    );
    assert!(
        has_error(&case, e1).await,
        "after 3: evaluation_message(E1) has an Error"
    );
    assert_eq!(
        case.assignment_outcomes(&k).await.get(2).cloned(),
        Some((a3, Some(DispatchedJobOutcome::Failed))),
        "after 3: dj(eval:E1)[2]"
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn evaluate_again(case: &SchedulerCase, key: &str) -> DispatchedJobId {
    case.assign_queued_evals().await;
    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("W1 gets the evaluation again")
        .assignment_id();
    case.advance_evaluation(W1, key, EvaluationStatus::EvaluatingDerivation)
        .await;
    assignment
}

async fn set_purged_at(case: &SchedulerCase, fingerprint: &str, ago: Duration) {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let purged_at = gradient_types::now() - chrono::Duration::from_std(ago).unwrap();
    gradient_entity::eval_cache_store::Entity::update_many()
        .col_expr(
            gradient_entity::eval_cache_store::Column::PurgedAt,
            Expr::value(purged_at),
        )
        .filter(gradient_entity::eval_cache_store::Column::Fingerprint.eq(fingerprint))
        .exec(&case.db.connection())
        .await
        .expect("the purge time writes");
}

#[tokio::test]
async fn r16_status_reports_move_the_evaluation_forward_and_skip_a_backwards_move() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evalq(e1)
        .evaluation_status(EvaluationStatus::Waiting)
        .waiting_reason(WaitingReason::eval_workers(EvalCapability::Eval, 0).to_json());
    let case = SchedulerCase::start(seed).await;
    let k = eval_key(e1);
    case.connect(W1, w_all(), &[]).await;
    case.refresh_waiting_state().await;
    case.assign_queued_evals().await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Flake)
        .await
        .expect("after 0: W1 gets eval:E1");
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (EvaluationStatus::Queued, None),
        "after 0: evaluation(E1)"
    );
    case.check("step 0").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.fetch_started_at.is_some()),
        (EvaluationStatus::Fetching, true),
        "after 1: {{Fetching, fetch_started_at set}}"
    );
    case.check("step 1").await;

    case.report_status(W1, &k, EvaluationStatus::EvaluatingFlake)
        .await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (
            evaluation.status,
            evaluation.eval_flake_started_at.is_some()
        ),
        (EvaluationStatus::EvaluatingFlake, true),
        "after 2: {{EvaluatingFlake, eval_flake_started_at set}}"
    );
    case.check("step 2").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    assert_eq!(
        case.evaluation(e1).await,
        evaluation,
        "after 3: evaluation(E1) unchanged"
    );
    case.check("step 3").await;

    case.report_status(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.eval_drv_started_at.is_some()),
        (EvaluationStatus::EvaluatingDerivation, true),
        "after 4: {{EvaluatingDerivation, eval_drv_started_at set}}"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r17_a_completed_fetch_job_enqueues_the_cached_follow_up_for_an_eval_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    case.connect(W1, w_network_eval(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;

    case.assign_queued_evals().await;
    let fetch_offer = case.offers_full(W1).await;
    let eval_offer = case.offers_full(W2).await;
    assert!(case.pending(&k).await, "after 1: pending(eval:E1)");
    assert_eq!(
        case.pending_flake_job(&k).await.map(|j| j.steps),
        Some(vec![FlakeStep::FetchFlake]),
        "after 1: steps of the pending job"
    );
    assert_eq!(
        job_ids(&fetch_offer),
        vec![k.clone()],
        "after 1: offers(W1)"
    );
    assert_eq!(
        job_ids(&eval_offer),
        Vec::<String>::new(),
        "after 1: offers(W2)"
    );
    case.check("step 1").await;

    case.score_all(W1).await;
    let nothing = case.request_job(W2, JobKind::Flake).await;
    assert!(nothing.is_none(), "after 2: W2 gets nothing");
    let fetch = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 2: W1 gets eval:E1");
    let a1 = fetch.assignment_id();
    let Job::Flake(job) = &fetch.job else {
        panic!("after 2: a flake job, got {:?}", fetch.job);
    };
    assert_eq!(
        job.steps,
        vec![FlakeStep::FetchFlake],
        "after 2: result.job.steps"
    );
    assert!(
        matches!(job.source, FlakeSource::Repository { .. }),
        "after 2: result.job.source is Repository, got {:?}",
        job.source
    );
    case.check("step 2").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    case.report_flake_source(W1, &k, SOURCE).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.flake_source.as_deref()),
        (EvaluationStatus::Fetching, Some(SOURCE)),
        "after 3: evaluation(E1)"
    );
    case.signals(W2);
    case.check("step 3").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 900)], 900)
        .await;
    assert_eq!(
        elapsed(&case, &k).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed), Some(900))],
        "after 4: dj(eval:E1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Fetching,
        "after 4: evaluation(E1).status"
    );
    let follow_up = case.pending_job(&k).await;
    let Some(PendingJob::Eval(follow_up)) = follow_up else {
        panic!("after 4: pending(eval:E1) is an eval job, got {follow_up:?}");
    };
    assert_eq!(
        follow_up.job.steps,
        vec![FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations],
        "after 4: steps of the follow-up"
    );
    assert_eq!(
        follow_up.job.source,
        FlakeSource::Cached {
            store_path: SOURCE.to_owned()
        },
        "after 4: source of the follow-up"
    );
    let required: Vec<_> = follow_up
        .required_paths
        .iter()
        .map(|p| p.path.as_str())
        .collect();
    assert_eq!(
        required,
        vec![SOURCE],
        "after 4: required_paths of the follow-up"
    );
    assert_eq!(
        case.evaluation_metric(e1).await.map(|m| m.fetch_ms),
        Some(900),
        "after 4: evaluation_metric(E1).fetch_ms"
    );
    let signals = case.signals(W2);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Offers(_)]),
        "after 4: signals(W2) == [Offers(_)], got {signals:?}"
    );
    case.check("step 4").await;

    let offer = case.offers_delta(W2).await;
    assert_eq!(
        job_ids(&offer),
        vec![k.clone()],
        "after 5: offers result.candidates"
    );
    case.score_all(W2).await;
    let a2 = case
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 5: W2 gets eval:E1")
        .assignment_id();
    let rows: Vec<_> = case
        .assignments(&k)
        .await
        .into_iter()
        .map(|row| (row.id, row.outcome, row.worker_id))
        .collect();
    assert_eq!(
        rows,
        vec![
            (a1, Some(DispatchedJobOutcome::Completed), W1.to_owned()),
            (a2, None, W2.to_owned()),
        ],
        "after 5: dj(eval:E1)"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn r18_a_completed_fetch_job_without_a_flake_source_fails_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    case.connect(W1, w_network_eval(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;

    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 1: W1 gets eval:E1")
        .assignment_id();
    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    case.check("step 1").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 100)], 100)
        .await;
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.finished_at.is_some()),
        (EvaluationStatus::Failed, true),
        "after 2: evaluation(E1)"
    );
    assert!(
        has_error(&case, e1).await,
        "after 2: evaluation_message(E1) has an Error"
    );
    assert!(!case.pending(&k).await, "after 2: pending(eval:E1)");
    case.signals(W1);
    case.signals(W2);
    case.check("step 2").await;

    case.assign_queued_evals().await;
    assert!(!case.pending(&k).await, "after 3: pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 3: signals(W2)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r19_an_eval_completion_moves_to_building_or_straight_to_the_verdict() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let (k1, k2) = (eval_key(e1), eval_key(e2));
    let a1 = case.evalrun(W1, e1).await;
    let a2 = case.evalrun(W2, e2).await;
    case.advance_evaluation(W1, &k1, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.advance_evaluation(W2, &k2, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let found = with_attr(with_output(drv(A, &[]), A_OUT), &attr_of(A));
    case.report_eval_result(W1, &k1, vec![found]).await;
    case.commit_nar(drv_nar(A)).await;
    case.upstream_probed(&[A]).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 1: evaluation(E1).status"
    );
    case.check("step 1").await;

    case.report_eval_stats(W1, &k1, EvalStatsReport::default())
        .await;
    let spans = vec![
        span(JobPhase::Fetch, 0, 1000),
        span(JobPhase::EvalFlake, 1000, 2000),
        span(JobPhase::EvalDerivations, 2000, 5000),
    ];
    case.complete(W1, &k1, a1, spans, 5000).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.building_started_at.is_some()),
        (EvaluationStatus::Building, true),
        "after 2: evaluation(E1)"
    );
    assert_eq!(
        case.evaluation_metric(e1)
            .await
            .map(|m| (m.fetch_ms, m.eval_flake_ms, m.eval_drv_ms)),
        Some((1000, 1000, 3000)),
        "after 2: evaluation_metric(E1) phase totals"
    );
    assert_eq!(
        case.assignment_outcomes(&k1).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(eval:E1)"
    );
    case.check("step 2").await;

    case.complete(W2, &k2, a2, Vec::new(), 100).await;
    let evaluation = case.evaluation(e2).await;
    assert_eq!(
        (evaluation.status, evaluation.finished_at.is_some()),
        (EvaluationStatus::Completed, true),
        "after 3: evaluation(E2)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r20_eval_messages_are_stored_for_an_active_job_and_an_error_fails_the_verdict() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    case.report_eval_message(W1, &k, EvalMessageLevel::Warning, "nix-eval", "deprecated")
        .await;
    let stored: Vec<_> = case
        .messages(e1)
        .await
        .into_iter()
        .map(|m| (m.level, m.source, m.message))
        .collect();
    assert_eq!(
        stored,
        vec![(
            MessageLevel::Warning,
            Some("nix-eval".to_owned()),
            "deprecated".to_owned()
        )],
        "after 1: evaluation_message(E1)"
    );
    case.check("step 1").await;

    let unknown = eval_key(EvaluationId::now_v7());
    case.report_eval_message(W1, &unknown, EvalMessageLevel::Error, "nix-eval", "nothing")
        .await;
    assert_eq!(
        case.messages(e1).await.len(),
        1,
        "after 2: evaluation_message(E1) rows unchanged"
    );
    case.check("step 2").await;

    case.report_eval_message(
        W1,
        &k,
        EvalMessageLevel::Error,
        "nix-eval:pkg.b",
        "attribute failed",
    )
    .await;
    assert_eq!(
        case.messages(e1).await.len(),
        2,
        "after 3: evaluation_message(E1) rows"
    );
    case.check("step 3").await;

    case.complete(W1, &k, a1, Vec::new(), 10).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 4: evaluation(E1).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r21_eval_stats_write_the_metric_and_only_the_closing_completion_adds_span_totals() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let stats = EvalStatsReport {
        peak_rss_mb: 512,
        total_thunks: 1000,
        total_eval_ms: 900,
        ..Default::default()
    };
    case.report_eval_stats(W1, &k, stats).await;
    let metric = case
        .evaluation_metric(e1)
        .await
        .expect("after 1: evaluation_metric(E1)");
    assert_eq!(
        (
            metric.peak_rss_mb,
            metric.fetch_ms,
            metric.worker_id.as_str()
        ),
        (512, 0, W1),
        "after 1: evaluation_metric(E1)"
    );
    case.check("step 1").await;

    let spans = vec![
        span(JobPhase::Fetch, 0, 100),
        span(JobPhase::EvalFlake, 100, 200),
        span(JobPhase::EvalDerivations, 200, 900),
    ];
    case.complete(W1, &k, a1, spans, 900).await;
    let metric = case
        .evaluation_metric(e1)
        .await
        .expect("after 2: evaluation_metric(E1)");
    assert_eq!(
        (
            metric.peak_rss_mb,
            metric.fetch_ms,
            metric.eval_flake_ms,
            metric.eval_drv_ms,
            metric.total_eval_ms,
        ),
        (512, 100, 100, 700, 900),
        "after 2: evaluation_metric(E1)"
    );
    let phases: Vec<_> = case
        .phases(a1)
        .await
        .into_iter()
        .map(|p| (p.seq, p.phase, p.start_ms, p.end_ms))
        .collect();
    assert_eq!(
        phases,
        vec![
            (0, JobPhase::Fetch.as_i16(), 0, 100),
            (1, JobPhase::EvalFlake.as_i16(), 100, 200),
            (2, JobPhase::EvalDerivations.as_i16(), 200, 900),
        ],
        "after 2: dispatched_job_phase(a1)"
    );
    case.check("step 2").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 999)], 999)
        .await;
    assert_eq!(
        case.evaluation_metric(e1).await,
        Some(metric),
        "after 3: evaluation_metric(E1) unchanged"
    );
    assert_eq!(
        case.assignments(&k)
            .await
            .first()
            .and_then(|row| row.worker_elapsed_ms),
        Some(900),
        "after 3: dj(eval:E1)[0].worker_elapsed_ms"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r22_input_update_reports_fill_the_row_and_an_expansion_creates_evaluations() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evalq(e1)
        .input_update("c0", "nix flake update", &["nixpkgs*"], true)
        .evaluation_kind(EvaluationKind::InputUpdate);
    let case = SchedulerCase::start(seed).await;
    let k = eval_key(e1);
    case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let bumped = vec![BumpedInputWire {
        name: "nixpkgs".into(),
        old_rev: Some("a".into()),
        new_rev: "b".into(),
    }];
    case.report_input_update_result(W1, &k, "{...}", bumped)
        .await;
    let update = case
        .input_update(e1)
        .await
        .expect("after 1: evaluation_input_update(E1)");
    assert_eq!(
        (update.candidate_lock.as_deref(), update.bumped_inputs),
        (
            Some("{...}"),
            Some(serde_json::json!([{ "name": "nixpkgs", "old_rev": "a", "new_rev": "b" }]))
        ),
        "after 1: evaluation_input_update(E1)"
    );
    case.check("step 1").await;

    case.report_input_update_expansion(W1, &k, &["nixpkgs", "nixpkgs-unstable"])
        .await;
    let task = case.evaluation(e1).await.task;
    let mut created = Vec::new();
    for evaluation in case.evaluations().await {
        if evaluation.id == e1 || evaluation.task != task {
            continue;
        }
        let update = case
            .input_update(evaluation.id)
            .await
            .expect("after 2: an evaluation_input_update row per new evaluation");
        created.push((
            evaluation.kind,
            evaluation.status,
            evaluation.concurrent,
            update.generator,
            update.target_inputs,
        ));
    }
    created.sort_by_key(|row| row.4.to_string());
    let expected = ["nixpkgs", "nixpkgs-unstable"].map(|input| {
        (
            EvaluationKind::InputUpdate,
            EvaluationStatus::Queued,
            true,
            "flake_lock".to_owned(),
            serde_json::json!([input]),
        )
    });
    assert_eq!(
        created,
        expected.to_vec(),
        "after 2: new evaluations of the task of E1"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r23_the_completion_watchdog_drives_an_evaluation_whose_eval_row_closed_long_ago() {
    let [e1, e2, e3, e4] = [(); 4].map(|_| EvaluationId::now_v7());
    let seed = shape()
        .evalq(e1)
        .evaluation_status(EvaluationStatus::EvaluatingDerivation)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Completed, ago(901))
        .startable(A, A_OUT)
        .evalq(e2)
        .evaluation_status(EvaluationStatus::Fetching)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Completed, ago(901))
        .evalq(e3)
        .evaluation_status(EvaluationStatus::EvaluatingFlake)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901))
        .evalq(e4)
        .evaluation_status(EvaluationStatus::EvaluatingDerivation)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Failed, ago(901));
    let case = SchedulerCase::start(seed).await;
    for e in [e1, e2, e3, e4] {
        case.backdate_evaluation(e, ago(901)).await;
    }
    let expected = [
        EvaluationStatus::Building,
        EvaluationStatus::Fetching,
        EvaluationStatus::EvaluatingFlake,
        EvaluationStatus::Failed,
    ];

    for step in 1..=2 {
        case.eval_completion_watchdog().await;
        let mut statuses = Vec::new();
        for e in [e1, e2, e3, e4] {
            statuses.push(case.evaluation(e).await.status);
        }
        assert_eq!(
            statuses,
            expected.to_vec(),
            "after {step}: evaluation(E1..E4).status"
        );
        case.check(&format!("step {step}")).await;
    }

    case.finish().await;
}

#[tokio::test]
async fn r24_the_stranded_build_sweep_requeues_an_abandoned_building_build_and_skips_a_tracked_one()
{
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
        .one(e1, A, A_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901))
        .one(e2, B, B_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901));
    let case = SchedulerCase::start(seed).await;
    case.backdate_shared_build(A, ago(901)).await;
    case.backdate_shared_build(B, ago(901)).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    case.connect(W1, w_all(), &[]).await;
    case.respawn_core().await;
    let row = case
        .assignments(&b)
        .await
        .first()
        .map(|row| row.id)
        .expect("the seeded row of build:B");
    case.reattach(W1, &[(b.as_str(), row)]).await;
    case.check("step 0").await;

    case.stranded_build_sweep().await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 1: att(A)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Building,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes(B).await,
        vec![AttemptOutcome::Running],
        "after 1: att(B)"
    );
    case.check("step 1").await;

    case.resync_startable_set().await;
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert!(!case.pending(&b).await, "after 2: pending(build:B)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r25_the_abandoned_dispatch_sweep_closes_only_old_untracked_rows() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .by_worker("gone")
        .dispatched_at(ago(1801))
        .one(EvaluationId::now_v7(), B, B_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .by_worker("gone2")
        .dispatched_at(ago(1700))
        .one(EvaluationId::now_v7(), C, C_OUT);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);

    let a3 = claim(&case, &[W1], W1).await;
    assert_eq!(
        case.open_assignment(&c).await.map(|row| row.id),
        Some(a3),
        "after 1: W1 runs build:C"
    );
    case.report_building(W1, &c).await;
    case.check("step 1").await;

    case.abandoned_dispatch_sweep().await;
    let closed = case.assignments(&a).await;
    assert!(
        matches!(
            closed.as_slice(),
            [row] if row.outcome == Some(DispatchedJobOutcome::Abandoned)
                && row.finished_at.is_some_and(is_recent)
        ),
        "after 2: dj(build:A) == [{{Abandoned, finished_at ~now}}], got {closed:?}"
    );
    let open: Vec<_> = case
        .assignments(&b)
        .await
        .into_iter()
        .map(|row| row.finished_at)
        .collect();
    assert_eq!(open, vec![None], "after 2: dj(build:B) == [{{open}}]");
    assert_eq!(
        case.assignment_outcomes(&c).await,
        vec![(a3, None)],
        "after 2: dj(build:C)"
    );
    assert_eq!(
        (case.sb(A).await.status, case.sb(B).await.status),
        (BuildStatus::Building, BuildStatus::Building),
        "after 2: sb(A).status, sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Running],
        "after 2: att(A)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r26_phase_spans_are_stored_with_the_parent_on_completion_and_failure() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let mut taken = Vec::new();
    for _ in 0..2 {
        let assignment = case
            .request_job(W1, JobKind::Build)
            .await
            .expect("after 1: W1 gets a build");
        taken.push((assignment.job_id().to_owned(), assignment.assignment_id()));
    }
    let id_of = |key: &str| {
        taken
            .iter()
            .find(|(job, _)| job == key)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("after 1: W1 runs {key}, got {taken:?}"))
    };
    let (a1, a2) = (id_of(&a), id_of(&b));
    case.report_building(W1, &a).await;
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    let spans = vec![
        JobPhaseSpan {
            paths: 3,
            bytes: 1000,
            ..span(JobPhase::Prefetch, 0, 50)
        },
        span(JobPhase::Build, 50, 500),
        JobPhaseSpan {
            parent: Some(1),
            ..span(JobPhase::Compress, 400, 480)
        },
        span(JobPhase::NarPush, 480, 520),
    ];
    case.complete(W1, &a, a1, spans, 520).await;
    let stored: Vec<_> = case
        .phases(a1)
        .await
        .into_iter()
        .map(|p| (p.seq, p.parent_seq, p.phase, p.paths, p.bytes))
        .collect();
    assert_eq!(
        stored,
        vec![
            (0, None, 8, 3, 1000),
            (1, None, 10, 0, 0),
            (2, Some(1), 11, 0, 0),
            (3, None, 12, 0, 0),
        ],
        "after 2: dispatched_job_phase(a1)"
    );
    case.check("step 2").await;

    let failed = failure(BuildFailureKind::Permanent)
        .spans(vec![
            span(JobPhase::Prefetch, 0, 10),
            span(JobPhase::Build, 10, 30),
        ])
        .elapsed_ms(30);
    case.fail(W1, &b, a2, failed).await;
    assert_eq!(
        case.phases(a2).await.len(),
        2,
        "after 3: dispatched_job_phase(a2) rows"
    );
    assert_eq!(
        elapsed(&case, &b).await.first().cloned(),
        Some((a2, Some(DispatchedJobOutcome::Failed), Some(30))),
        "after 3: dj(build:B)[0]"
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn offered(case: &SchedulerCase, workers: &[&str]) {
    case.admit_startable_moves().await;
    for worker in workers {
        case.offers_full(worker).await;
    }
    for worker in workers {
        case.score_all(worker).await;
    }
}

async fn claim(case: &SchedulerCase, workers: &[&str], winner: &str) -> DispatchedJobId {
    offered(case, workers).await;
    case.request_job(winner, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("after 1: {winner} gets a build"))
        .assignment_id()
}

async fn take(case: &SchedulerCase, worker: &str, key: &str) -> DispatchedJobId {
    let assignment = case
        .request_job(worker, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("{worker} gets {key} again"));
    assert_eq!(assignment.job_id(), key, "{worker} gets {key} again");
    assignment.assignment_id()
}

async fn substitute(case: &SchedulerCase, worker: &str, key: &str) -> DispatchedJobId {
    let assignment = case
        .request_job(worker, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("{worker} gets {key}"));
    let Job::Build(job) = &assignment.job else {
        panic!("a build job, got {:?}", assignment.job);
    };
    assert_eq!(
        job.build.kind,
        BuildSpecKind::Substitute,
        "result.job.kind of {key}"
    );
    assignment.assignment_id()
}

async fn retry_after(case: &SchedulerCase, secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    case.requeue_transient_retries().await;
    case.admit_startable_moves().await;
    case.offers_delta(W1).await;
    case.score_all(W1).await;
}

fn transient(error: &str) -> super::Failure {
    failure(BuildFailureKind::Transient)
        .error(error)
        .elapsed_ms(4)
}

fn corrupt() -> super::Failure {
    failure(BuildFailureKind::CorruptEvalCache)
        .error("schema")
        .missing(&["F".into()])
        .elapsed_ms(3)
}

async fn has_error(case: &SchedulerCase, evaluation: EvaluationId) -> bool {
    case.messages(evaluation)
        .await
        .iter()
        .any(|m| m.level == MessageLevel::Error)
}

async fn elapsed(
    case: &SchedulerCase,
    key: &str,
) -> Vec<(DispatchedJobId, Option<DispatchedJobOutcome>, Option<i64>)> {
    case.assignments(key)
        .await
        .into_iter()
        .map(|row| (row.id, row.outcome, row.worker_elapsed_ms))
        .collect()
}

async fn started(case: &SchedulerCase, path: &str) -> Vec<(AttemptOutcome, bool)> {
    case.attempts(path)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.build_started_at.is_some()))
        .collect()
}
