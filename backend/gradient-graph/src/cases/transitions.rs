/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};
use gradient_wire::types::BuildFailureKind;

use super::{
    A, B, B_OUT, C, Case, D, I, I_OUT, P, P_OUT, Q, Q_OUT, R, R_OUT, chain, chain_done, diamond,
    nar, store_path, with_references,
};
use crate::messages::{Demotion, OrphanCause, Transition};

fn building(shape: Shape, path: &str) -> Shape {
    shape
        .derivation(path)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
}

#[tokio::test]
async fn t01_build_started_moves_queued_to_building_with_a_running_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    let assignment = case.assigned(e1, B, false).await;

    assert!(assignment.is_some(), "step 1: Assigned(E1, B) is accepted");
    case.zero_drift("step 1").await;
    let version = case.gv(e1).await;

    case.build_started(B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Building, true),
        "after 2: sb(B)"
    );
    assert_eq!(
        case.latest_attempt(B).await.outcome,
        AttemptOutcome::Running,
        "after 2: attempt(B, latest)"
    );
    assert_eq!(case.gv(e1).await, version + 1, "after 2: gv(E1)");
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 1],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t02_a_completed_build_is_fetchable_only_after_the_commit_and_the_parent_waits_for_it() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain(e1), B)).await;

    case.build_completed(B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Completed, false, true),
        "after 1: sb(B)"
    );
    assert!(
        !case.output_stored(B, "out").await,
        "after 1: cached_path(output(B,out).hash) stored"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 0, 0],
        "after 1: counters(E1)"
    );
    let report = case.drift().await;
    assert_eq!(report.unbacked_trusted_outputs, 0, "after 1: {report:?}");
    assert_eq!(report.total(), 0, "drift after step 1: {report:?}");

    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;

    let b = case.sb(B).await;
    assert_eq!((b.fetchable, b.wanted), (true, false), "after 2: sb(B)");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Queued, 0),
        "after 2: sb(A)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t03_a_transient_failure_stays_a_wanted_builder_and_requeues_only_after_the_backoff() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain(e1), B)).await;

    case.build_failed(B, BuildFailureKind::Transient, &[]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::FailedTransient, true),
        "after 1: sb(B)"
    );
    assert_eq!(
        case.latest_attempt(B).await.outcome,
        AttemptOutcome::Failed,
        "after 1: attempt(B, latest)"
    );
    assert!(case.sb(A).await.wanted, "after 1: sb(A).wanted");
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    let requeued = case.requeue().await;

    assert_eq!(requeued, 0, "after 2: result inside the backoff of B");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTransient,
        "after 2: sb(B)"
    );
    case.zero_drift("step 2").await;

    case.advance_clock(B).await;
    let requeued = case.requeue().await;

    assert_eq!(requeued, 1, "after 3: result past the backoff of B");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 3: sb(B)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 3: counters(E1)"
    );
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn t04_a_permanent_failure_fails_the_dependents_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain(e1).evaluation_status(EvaluationStatus::Building),
        B,
    ))
    .await;
    let version = case.gv(e1).await;

    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::FailedPermanent, false),
        "after 1: sb(B)"
    );
    let attempt = case.latest_attempt(B).await;
    assert_eq!(
        (attempt.outcome, attempt.failure_message.is_some()),
        (AttemptOutcome::Failed, true),
        "after 1: attempt(B, latest): {attempt:?}"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::DependencyFailed, false),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(case.gv(e1).await, version + 1, "after 1: gv(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t05_a_timeout_is_a_terminal_failure_and_spreads_like_a_permanent_failure() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain(e1).evaluation_status(EvaluationStatus::Building),
        B,
    ))
    .await;

    case.build_failed(B, BuildFailureKind::Timeout, &[]).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTimeout,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t06_inputs_unavailable_with_the_object_in_storage_deletes_nothing_and_retries() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    case.store_object(B_OUT).await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert!(
        case.find_cached_path(B_OUT).await.is_some(),
        "after 1: cached_path(B_OUT) present"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Completed, true),
        "after 1: sb(B)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps, a.wanted),
        (BuildStatus::FailedTransient, 0, true),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t07_inputs_unavailable_with_the_object_gone_drops_the_path_and_resets_the_producer() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert!(
        case.find_cached_path(B_OUT).await.is_none(),
        "after 1: cached_path(B_OUT) gone"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Queued, false, true),
        "after 1: sb(B)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::FailedTransient, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    case.advance_clock(A).await;
    case.requeue().await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 2: sb(A)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t08_the_fourth_inputs_unavailable_fails_the_build_permanently_without_a_self_heal() {
    let e1 = EvaluationId::now_v7();
    let unavailable = Some(AttemptFailureReason::InputsUnavailable);
    let case = Case::start(
        chain_done(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(A)"
    );
    assert!(
        case.find_cached_path(B_OUT).await.is_some(),
        "after 1: cached_path(B_OUT) present, no self-heal"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 1, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t09_a_lost_worker_closes_only_the_running_attempt_and_requeues_only_building_rows() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        diamond(e1)
            .derivation(D)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Failed, None)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.builds_lost(&[D, C], OrphanCause::WorkerLost).await;

    assert_eq!(
        case.attempt(D, 1).await.outcome,
        AttemptOutcome::Failed,
        "after 1: attempt(D, 1)"
    );
    let closed = case.attempt(D, 2).await;
    assert_eq!(
        (closed.outcome, closed.reason),
        (
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        ),
        "after 1: attempt(D, 2)"
    );
    assert_eq!(
        case.sb(D).await.status,
        BuildStatus::Queued,
        "after 1: sb(D)"
    );
    assert_eq!(
        case.sb(C).await.status,
        BuildStatus::Created,
        "after 1: sb(C)"
    );
    assert_eq!(
        case.counters(e1).await,
        [4, 4, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    let (rows, version) = (case.rows().await, case.gv(e1).await);

    case.builds_lost(&[D], OrphanCause::WorkerLost).await;

    assert_eq!(case.rows().await, rows, "after 2: nothing changes");
    assert_eq!(case.gv(e1).await, version, "after 2: gv(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t10_a_build_lost_with_its_worker_goes_to_created_when_its_conditions_regressed() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain_done(e1), A)).await;

    case.demote(Demotion::MissingNar { hash: B_OUT.into() })
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Building, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 1: sb(B)"
    );
    case.zero_drift("step 1").await;

    case.builds_lost(&[A], OrphanCause::WorkerLost).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 2: sb(A)"
    );
    let closed = case.latest_attempt(A).await;
    assert_eq!(
        (closed.outcome, closed.reason),
        (
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        ),
        "after 2: attempt(A, latest)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t11_not_enough_resources_fails_a_queued_build_its_dependents_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1).evaluation_status(EvaluationStatus::Building)).await;

    case.not_enough_resources(e1, B, 4096).await;

    let attempt = case.latest_attempt(B).await;
    assert_eq!(
        (attempt.outcome, attempt.reason),
        (
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::NotEnoughResources)
        ),
        "after 1: attempt(B, latest)"
    );
    assert!(
        attempt
            .failure_message
            .as_deref()
            .is_some_and(|message| message.contains("4096")),
        "after 1: attempt(B, latest).failure_message: {attempt:?}"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::FailedPermanent, false),
        "after 1: sb(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t12_not_enough_resources_leaves_a_build_alone_that_a_worker_took() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1).derivation(B).open_assignment()).await;
    let version = case.gv(e1).await;

    case.not_enough_resources(e1, B, 4096).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 1: sb(B)"
    );
    let attempts = case.attempts(B).await;
    assert!(attempts.is_empty(), "after 1: no attempt: {attempts:?}");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 1: sb(A)"
    );
    assert_eq!(case.gv(e1).await, version, "after 1: gv(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t13_prioritize_sets_the_flag_on_the_build_or_on_every_named_build_of_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    case.prioritize_build(B).await;

    assert!(case.sb(B).await.prioritized, "after 1: sb(B).prioritized");
    assert!(!case.sb(A).await.prioritized, "after 1: sb(A).prioritized");
    case.zero_drift("step 1").await;

    case.transition(Transition::PrioritizeEvaluation { evaluation: e1 })
        .await;

    assert!(case.sb(A).await.prioritized, "after 2: sb(A).prioritized");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t14_requeue_imports_thaws_the_failed_closure_except_reproducible_builder_failures() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(I)
            .walked()
            .entry_point("pkg.i")
            .job()
            .probed()
            .drv_cached()
            .output(I_OUT)
            .ifd()
            .depends_on(P)
            .depends_on(Q)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(P)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(P_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::NotEnoughResources),
            )
            .derivation(Q)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(Q_OUT)
            .build_status(BuildStatus::DependencyFailed)
            .depends_on(R)
            .derivation(R)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(R_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            ),
    )
    .await;

    case.requeue_imports(e1, &[I]).await;

    let p = case.sb(P).await;
    assert_eq!(
        (p.status, p.attempt, p.probed, p.wanted),
        (BuildStatus::Skipped, 0, true, false),
        "after 1: sb(P)"
    );
    assert_eq!(
        case.sb(I).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(I)"
    );
    assert_eq!(
        case.sb(Q).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(Q)"
    );
    assert_eq!(
        case.sb(R).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(R)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}
