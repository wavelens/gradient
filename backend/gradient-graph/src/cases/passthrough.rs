/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::BuildFailureKind;

use super::{
    A, A_OUT, B, B_OUT, BD, BD_OUT, Case, R, R_OUT, X, X_OUT, drv_nar, nar, pass, with_references,
};

async fn substitute_miss(case: &Case, evaluation: EvaluationId) {
    case.assigned(evaluation, A, true)
        .await
        .expect("a worker claims the passthrough A");

    case.build_started(A).await;
    case.build_failed(A, BuildFailureKind::SubstituteUnavailable, &[])
        .await;

    case.close_assignment(A).await;
}

#[tokio::test]
async fn u01_a_passthrough_completes_as_substituted_and_is_fetchable_after_its_runtime_dependency()
{
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
            .derivation(A)
            .walked()
            .job()
            .probed()
            .output(A_OUT)
            .cache_available()
            .runtime_depends_on(R)
            .depends_on(BD)
            .build_status(BuildStatus::Queued)
            .derivation(R)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(R_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(BD)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(BD_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    case.assigned(e1, A, true)
        .await
        .expect("step 1: a worker claims the passthrough A");

    case.build_started(A).await;

    case.zero_drift("step 1").await;

    case.commit_nar(with_references(
        nar(A_OUT, "a"),
        &[(A_OUT, "a"), (R_OUT, "r")],
    ))
    .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.missing_runtime_deps),
        (BuildStatus::Building, 1),
        "after 2: sb(A) status, missing_runtime_deps"
    );

    case.zero_drift("step 2").await;

    case.build_output(A, true).await;
    case.build_completed(A).await;

    let (a, x) = (case.sb(A).await, case.sb(X).await);
    assert_eq!(
        (a.status, a.fetchable, a.wanted),
        (BuildStatus::Substituted, false, true),
        "after 3: sb(A) status, fetchable, wanted"
    );

    assert_eq!(
        (x.status, x.blocking_deps),
        (BuildStatus::Created, 1),
        "after 3: sb(X) status, blocking_deps"
    );

    let bd = case.sb(BD).await.status;
    assert_eq!(bd, BuildStatus::Skipped, "after 3: sb(Bd).status");
    case.zero_drift("step 3").await;

    case.build_started(R).await;
    case.commit_nar(with_references(nar(R_OUT, "r"), &[(R_OUT, "r")]))
        .await;

    case.build_completed(R).await;

    let (a, x) = (case.sb(A).await, case.sb(X).await);
    assert!(case.sb(R).await.fetchable, "after 4: sb(R) fetchable");
    assert!(a.fetchable, "after 4: sb(A) fetchable: {a:?}");
    assert_eq!(
        (x.status, x.blocking_deps),
        (BuildStatus::Queued, 0),
        "after 4: sb(X) status, blocking_deps"
    );

    let counters = case.counters(e1).await;
    assert_eq!(counters, [4, 1, 0, 1, 0], "after 4: counters(E1)");
    case.zero_drift("step 4").await;
    case.finish().await;
}

#[tokio::test]
async fn u02_two_substitute_misses_in_the_same_evaluation_give_the_passthrough_up() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(pass(e1).evaluation_status(EvaluationStatus::Building)).await;

    substitute_miss(&case, e1).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 1: sb(A) status, cache_available"
    );

    let attempts = case.attempts(A).await;
    assert_eq!(attempts.len(), 1, "after 1: attempts of A: {attempts:?}");
    let b = case.sb(B).await.status;
    assert_eq!(b, BuildStatus::Skipped, "after 1: sb(B).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    substitute_miss(&case, e1).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.attempt),
        (BuildStatus::Created, false, 0),
        "after 2: sb(A) status, cache_available, attempt"
    );

    assert_eq!(
        (a.probed, a.wanted, a.blocking_deps),
        (true, true, 1),
        "after 2: sb(A) probed, wanted, blocking_deps"
    );

    let out = case.output_of(A, "out").await;
    assert_eq!(
        (
            out.external_url,
            out.nar_hash,
            out.file_hash,
            out.file_size,
            out.references
        ),
        (None, None, None, None, None),
        "after 2: output(A,out) url, nar_hash, file_hash, file_size, references_list"
    );

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted),
        (BuildStatus::Queued, 0, true),
        "after 2: sb(B) status, attempt, wanted"
    );

    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn u03_substitute_misses_in_two_evaluations_do_not_add_up() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        pass(e1)
            .evaluation_status(EvaluationStatus::Building)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .entry_point("pkg.a")
            .job(),
    )
    .await;

    substitute_miss(&case, e1).await;

    case.zero_drift("step 1").await;

    substitute_miss(&case, e2).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 2: sb(A) status, cache_available"
    );

    let b = case.sb(B).await.status;
    assert_eq!(b, BuildStatus::Skipped, "after 2: sb(B).status");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn u04_a_given_up_passthrough_stays_a_builder_and_queues_after_its_drv_commit() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(pass(e1).evaluation_status(EvaluationStatus::Building)).await;
    substitute_miss(&case, e1).await;
    substitute_miss(&case, e1).await;
    let (a, b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (a.status, a.cache_available, a.probed, b.status),
        (BuildStatus::Created, false, true, BuildStatus::Queued),
        "given: sb(A) status, cache_available, probed and sb(B).status after U02 step 2"
    );

    case.zero_drift("the given state").await;

    let gained = case.upstream_probed(&[A]).await;

    assert!(gained.is_empty(), "after 1: result == []: {gained:?}");
    let a = case.sb(A).await.status;
    assert_eq!(a, BuildStatus::Created, "after 1: sb(A).status");
    case.zero_drift("step 1").await;

    case.build_started(B).await;
    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;

    case.build_completed(B).await;

    assert!(case.sb(B).await.fetchable, "after 2: sb(B) fetchable");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 0),
        "after 2: sb(A) status, blocking_deps"
    );

    case.zero_drift("step 2").await;

    case.commit_nar(drv_nar(A)).await;

    let a = case.sb(A).await.status;
    assert_eq!(a, BuildStatus::Queued, "after 3: sb(A).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 3: counters(E1)");
    case.zero_drift("step 3").await;
    case.finish().await;
}
