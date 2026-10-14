/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::status::BuildRefusal;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::BuildFailureKind;

use super::{
    A, A_OUT, A1, A1_OUT, A2, A2_OUT, B, B_OUT, BUILDER_EXIT, C, C_OUT, Case, D, D_OUT, G, G_OUT,
    H, H_OUT, P, P_OUT, Q, Q_OUT, R, R_OUT, S, S_OUT, X, X_OUT, Y, Y_OUT, chain, diamond, nar,
    with_references,
};

#[tokio::test]
async fn f01_a_permanent_failure_below_a_diamond_fails_both_middles_and_the_top_once_each() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        diamond(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(D)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;
    let version = case.gv(e1).await;

    case.build_failed(D, BuildFailureKind::Permanent, &[]).await;

    assert_eq!(
        case.sb(D).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(D)"
    );
    for path in [B, C, A] {
        assert_eq!(
            case.sb(path).await.status,
            BuildStatus::DependencyFailed,
            "after 1: sb({path})"
        );
    }
    for path in [A, B, C, D] {
        assert!(!case.sb(path).await.wanted, "after 1: sb({path}).wanted");
    }
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [4, 0, 4, 0, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(case.gv(e1).await, version + 1, "after 1: gv(E1), once");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn f02_a_build_edge_into_a_passthrough_does_not_spread_the_failure() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(A)
            .depends_on(B)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .output(A_OUT)
            .cache_available()
            .depends_on(B)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(X)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 1, 2, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;
    let assignment = case.assigned(e1, A, true).await;
    case.build_started(A).await;
    case.build_output(A, true).await;
    case.build_completed(A).await;

    assert!(assignment.is_some(), "step 2: Assigned(E1, A) is accepted");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.fetchable),
        (BuildStatus::Substituted, true),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 0, 2, 0, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn f03_a_runtime_edge_spreads_the_failure_into_a_passthrough() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .output(A_OUT)
            .cache_available()
            .runtime_depends_on(B)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::DependencyFailed, true),
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
async fn f04_a_retry_thaws_the_shared_build_and_only_the_dependents_of_its_evaluation() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A1)
            .walked()
            .entry_point("pkg.a1")
            .job()
            .probed()
            .drv_cached()
            .output(A1_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A2)
            .walked()
            .entry_point("pkg.a2")
            .job()
            .probed()
            .drv_cached()
            .output(A2_OUT)
            .depends_on(B)
            .derivation(B)
            .job(),
    )
    .await;

    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(B)"
    );
    for path in [A1, A2] {
        assert_eq!(
            case.sb(path).await.status,
            BuildStatus::DependencyFailed,
            "after 1: sb({path})"
        );
    }
    for evaluation in [e1, e2] {
        assert_eq!(
            case.evaluation(evaluation).await.status,
            EvaluationStatus::Failed,
            "after 1: evaluation({evaluation})"
        );
    }
    case.zero_drift("step 1").await;

    case.retry_build(e2, B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.probed, b.wanted),
        (BuildStatus::Created, 0, false, false),
        "after 2: sb(B)"
    );
    let a2 = case.sb(A2).await;
    assert_eq!(
        (a2.status, a2.attempt, a2.probed, a2.wanted),
        (BuildStatus::Created, 0, false, true),
        "after 2: sb(A2)"
    );
    assert_eq!(
        case.sb(A1).await.status,
        BuildStatus::DependencyFailed,
        "after 2: sb(A1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1)"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E2)"
    );
    assert_eq!(
        case.counters(e2).await,
        [2, 1, 0, 0, 0],
        "after 2: counters(E2)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn f05_abort_build_is_refused_on_a_terminal_build_a_finished_evaluation_and_a_shared_build() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(A)
            .depends_on(B)
            .depends_on(C)
            .depends_on(S)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(C)
            .derivation(S)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(S_OUT)
            .build_status(BuildStatus::Queued)
            .evaluation(e2)
            .derivation(S)
            .job()
            .evaluation(e3)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(Y)
            .walked()
            .entry_point("pkg.y")
            .job()
            .probed()
            .drv_cached()
            .output(Y_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(Y),
    )
    .await;
    let c = case.sb(C).await;

    let report = case.abort_build(e1, C).await;

    assert_eq!(
        report.refusal,
        Some(BuildRefusal::WrongStatus(BuildStatus::Completed)),
        "after 1: AbortBuild(E1, C)"
    );
    assert_eq!(case.sb(C).await, c, "after 1: sb(C)");
    case.zero_drift("step 1").await;

    let report = case.abort_build(e1, S).await;

    assert_eq!(
        report.refusal,
        Some(BuildRefusal::NeededByOtherEvaluation),
        "after 2: AbortBuild(E1, S)"
    );
    assert_eq!(
        case.sb(S).await.status,
        BuildStatus::Queued,
        "after 2: sb(S)"
    );
    case.zero_drift("step 2").await;

    let report = case.abort_build(e3, Y).await;

    assert_eq!(
        report.refusal,
        Some(BuildRefusal::EvaluationFinished),
        "after 3: AbortBuild(E3, Y)"
    );
    case.zero_drift("step 3").await;

    case.abort_build(e1, A).await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 4: sb(A)"
    );
    assert!(
        case.job(e1, A).await.is_some_and(|job| job.aborted),
        "after 4: build_job(E1, A).aborted"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::DependencyFailed,
        "after 4: sb(X)"
    );
    case.zero_drift("step 4").await;

    case.abort_build(e1, B).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 5: sb(B)"
    );
    assert_eq!(
        case.latest_attempt(B).await.outcome,
        AttemptOutcome::Aborted,
        "after 5: attempt(B, latest)"
    );
    assert!(
        case.job(e1, B).await.is_some_and(|job| job.aborted),
        "after 5: build_job(E1, B).aborted"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 5: evaluation(E1), S is still Queued"
    );
    assert_eq!(
        case.counters(e1).await,
        [5, 1, 3, 1, 0],
        "after 5: counters(E1)"
    );
    case.zero_drift("step 5").await;
    case.finish().await;
}

#[tokio::test]
async fn f06_aborting_a_dependency_fails_the_entry_point_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1).evaluation_status(EvaluationStatus::Building)).await;

    case.abort_build(e1, B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Aborted, false),
        "after 1: sb(B)"
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
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn f07_retrying_a_dependency_failed_build_retries_the_failed_dependencies_below_it() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .depends_on(D)
            .depends_on(G)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .depends_on(C)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(AttemptOutcome::Failed, BUILDER_EXIT)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::FailedTimeout)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::WallClockTimeout),
            )
            .derivation(G)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(G_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(G)
            .depends_on(H)
            .derivation(H)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(H_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(AttemptOutcome::Failed, BUILDER_EXIT),
    )
    .await;
    let g = case.sb(G).await;

    let report = case.retry_build(e1, A).await;

    assert_eq!(report.refusal, None, "after 1: RetryBuild(E1, A)");
    for path in [B, C, D] {
        let build = case.sb(path).await;
        assert_eq!(
            (build.status, build.attempt, build.probed, build.wanted),
            (BuildStatus::Created, 0, false, false),
            "after 1: sb({path})"
        );
    }
    assert_eq!(
        case.sb(H).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(H), behind the fetchable G"
    );
    assert_eq!(case.sb(G).await, g, "after 1: sb(G)");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.attempt, a.probed, a.wanted),
        (BuildStatus::Created, 0, false, true),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [6, 1, 1, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn f08_retry_build_is_refused_outside_the_four_failed_statuses() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        diamond(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(C)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(B)
            .build_status(BuildStatus::Completed)
            .cached_output(B),
    )
    .await;
    let (rows, version) = (case.rows().await, case.gv(e1).await);

    for (step, path, status) in [
        (1, A, BuildStatus::Created),
        (2, D, BuildStatus::Queued),
        (3, C, BuildStatus::Building),
        (4, B, BuildStatus::Completed),
    ] {
        let report = case.retry_build(e1, path).await;

        assert_eq!(
            report.refusal,
            Some(BuildRefusal::WrongStatus(status)),
            "after {step}: RetryBuild(E1, {path})"
        );
        assert_eq!(case.rows().await, rows, "after {step}: nothing changes");
        assert_eq!(case.gv(e1).await, version, "after {step}: gv(E1)");
        case.zero_drift(&format!("step {step}")).await;
    }
    case.finish().await;
}

#[tokio::test]
async fn f09_a_retry_is_refused_while_a_newer_evaluation_of_the_task_exists() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(AttemptOutcome::Failed, BUILDER_EXIT)
            .evaluation(e2)
            .same_task_as(e1)
            .derivation(A)
            .entry_point("pkg.a")
            .job(),
    )
    .await;
    let attempts = case.attempts(A).await;

    let report = case.retry_build(e1, A).await;

    assert_eq!(
        report.refusal,
        Some(BuildRefusal::NewerEvaluation),
        "after 1: RetryBuild(E1, A)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::FailedPermanent, false),
        "after 1: sb(A)"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 1: attempts of A");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 1: evaluation(E2)"
    );
    assert_eq!(
        case.counters(e2).await,
        [1, 0, 1, 0, 0],
        "after 1: counters(E2)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn f10_the_failure_spread_marks_only_created_queued_and_failed_transient_dependents() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(P)
            .depends_on(Q)
            .depends_on(R)
            .derivation(P)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(P_OUT)
            .depends_on(D)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(Q)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(Q_OUT)
            .depends_on(D)
            .build_status(BuildStatus::Completed)
            .cached_output(Q)
            .derivation(R)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(R_OUT)
            .depends_on(D)
            .build_status(BuildStatus::FailedTransient)
            .attempt(AttemptOutcome::Failed, None)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.build_failed(D, BuildFailureKind::Permanent, &[]).await;

    assert_eq!(
        case.sb(D).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(D)"
    );
    assert_eq!(
        case.sb(R).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(R)"
    );
    assert_eq!(
        case.sb(P).await.status,
        BuildStatus::Building,
        "after 1: sb(P)"
    );
    assert_eq!(
        case.sb(Q).await.status,
        BuildStatus::Completed,
        "after 1: sb(Q)"
    );
    assert_eq!(
        case.sb(X).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(X)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1), P is still Building"
    );
    assert_eq!(
        case.counters(e1).await,
        [5, 1, 3, 0, 1],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}
