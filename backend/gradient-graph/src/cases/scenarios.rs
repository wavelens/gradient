/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_entity::{derivation, evaluation};
use gradient_fixtures::shape::shape;
use gradient_wire::types::BuildFailureKind;

use super::{
    A, A_OUT, A1, A1_OUT, A2, A2_OUT, B, B_OUT, Case, X, X_OUT, drv, drv_nar, nar, narinfo,
    with_attr, with_output, with_references,
};
use crate::messages::{Demotion, GcRequest, NarCommit, Transition};

async fn statuses(case: &Case, paths: &[&str]) -> Vec<BuildStatus> {
    let mut statuses = Vec::new();
    for path in paths {
        statuses.push(case.sb(path).await.status);
    }

    statuses
}

async fn is_signed(case: &Case, hash: &str, cache: CacheId) -> bool {
    case.signature(hash, cache)
        .await
        .is_some_and(|row| row.signature.is_some())
}

#[tokio::test]
async fn l01_full_life_of_a_small_project_a_second_evaluation_a_shared_failure_a_retry_and_a_gc() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (c1, c2) = (CacheId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation(e2)
            .into_cache(c2)
            .keyed(Case::key()),
    )
    .await;
    let (caches_of_e1, caches_of_e2) =
        (case.project_caches(e1).await, case.project_caches(e2).await);
    let x_above_a = || with_attr(with_output(drv(X, &[A]), X_OUT), "pkg.x");

    let chain_below_x = vec![
        x_above_a(),
        with_output(drv(A, &[B]), A_OUT),
        with_output(drv(B, &[]), B_OUT),
    ];
    case.walk(case.tasked(e1, chain_below_x).await).await;

    assert_eq!(
        statuses(&case, &[X, A, B]).await,
        [
            BuildStatus::Created,
            BuildStatus::Skipped,
            BuildStatus::Skipped
        ],
        "after 1: X, A, B"
    );
    assert!(case.sb(X).await.wanted, "after 1: X wanted");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 1, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    for path in [X, A, B] {
        case.upstream_probed(&[path]).await;
    }

    assert_eq!(
        statuses(&case, &[X, A, B]).await,
        [BuildStatus::Created; 3],
        "after 2: nothing Queued"
    );
    let wanted = [case.sb(A).await.wanted, case.sb(B).await.wanted];
    assert_eq!(wanted, [true, true], "after 2: A, B wanted");
    case.zero_drift("step 2").await;

    for path in [X, A, B] {
        case.commit_nar(NarCommit {
            targets: caches_of_e1,
            ..drv_nar(path)
        })
        .await;
    }

    assert_eq!(
        statuses(&case, &[X, A, B]).await,
        [
            BuildStatus::Created,
            BuildStatus::Created,
            BuildStatus::Queued
        ],
        "after 3: X, A, B"
    );
    let blocking = [
        case.sb(X).await.blocking_deps,
        case.sb(A).await.blocking_deps,
    ];
    assert_eq!(blocking, [1, 1], "after 3: blocking_deps of X, A");
    let drv_of_b = &B[..32];
    assert!(
        is_signed(&case, drv_of_b, c1).await,
        "after 3: signature(B.drv, C1)"
    );
    case.zero_drift("step 3").await;

    case.transition(Transition::EvalStreamCompleted { evaluation: e1 })
        .await;

    let status = case.evaluation_status(e1).await;
    assert_eq!(
        status,
        EvaluationStatus::Building,
        "after 4: evaluation(E1)"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 1, 0], "after 4: counters(E1)");
    case.zero_drift("step 4").await;

    case.build_started(B).await;

    case.zero_drift("step 5").await;

    case.walk(case.tasked(e2, vec![x_above_a()]).await).await;
    case.transition(Transition::EvalStreamCompleted { evaluation: e2 })
        .await;

    for path in [X, A, B] {
        let job = case.job(e2, path).await;
        assert!(job.is_some(), "after 6: build_job(E2, {path})");
    }
    let status = case.evaluation_status(e2).await;
    assert_eq!(
        status,
        EvaluationStatus::Building,
        "after 6: evaluation(E2)"
    );
    let counters = case.counters(e2).await;
    assert_eq!(counters, [3, 3, 0, 0, 1], "after 6: counters(E2)");
    case.zero_drift("step 6").await;

    case.commit_nar(NarCommit {
        targets: caches_of_e1,
        ..with_references(nar(B_OUT, "b"), &[(B_OUT, "b")])
    })
    .await;
    case.build_completed(B).await;

    assert!(case.sb(B).await.fetchable, "after 7: sb(B) fetchable");
    assert!(
        is_signed(&case, B_OUT, c1).await,
        "after 7: signature(B_OUT, C1)"
    );
    assert!(
        is_signed(&case, B_OUT, c2).await,
        "after 7: signature(B_OUT, C2)"
    );
    let status = case.sb(A).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 7: sb(A)");
    let counters = [case.counters(e1).await, case.counters(e2).await];
    assert_eq!(
        counters,
        [[3, 2, 0, 1, 0]; 2],
        "after 7: counters(E1), counters(E2)"
    );
    case.zero_drift("step 7").await;

    case.build_started(A).await;
    case.build_failed(A, BuildFailureKind::Permanent, &[]).await;

    assert_eq!(
        statuses(&case, &[A, X]).await,
        [BuildStatus::FailedPermanent, BuildStatus::DependencyFailed],
        "after 8: A, X"
    );
    let verdicts = [
        case.evaluation_status(e1).await,
        case.evaluation_status(e2).await,
    ];
    assert_eq!(
        verdicts,
        [EvaluationStatus::Failed; 2],
        "after 8: evaluation(E1), evaluation(E2)"
    );
    case.zero_drift("step 8").await;

    let retry = case.retry_build(e2, A).await;
    case.upstream_probed(&[A]).await;
    case.upstream_probed(&[X]).await;

    assert_eq!(retry.refusal, None, "step 9: the retry is taken");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.attempt, a.probed),
        (BuildStatus::Queued, 0, true),
        "after 9: sb(A)"
    );
    let status = case.sb(X).await.status;
    assert_eq!(status, BuildStatus::Created, "after 9: sb(X)");
    let statuses_of_evaluations = [
        case.evaluation_status(e1).await,
        case.evaluation_status(e2).await,
    ];
    assert_eq!(
        statuses_of_evaluations,
        [EvaluationStatus::Failed, EvaluationStatus::Building],
        "after 9: evaluation(E1), evaluation(E2)"
    );
    case.zero_drift("step 9").await;

    case.build_started(A).await;
    case.commit_nar(NarCommit {
        targets: caches_of_e2,
        ..with_references(nar(A_OUT, "a"), &[(A_OUT, "a"), (B_OUT, "b")])
    })
    .await;
    case.build_completed(A).await;

    assert!(case.sb(A).await.fetchable, "after 10: sb(A) fetchable");
    let status = case.sb(X).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 10: sb(X)");
    case.zero_drift("step 10").await;

    case.build_started(X).await;
    case.commit_nar(NarCommit {
        targets: caches_of_e2,
        ..with_references(nar(X_OUT, "x"), &[(X_OUT, "x"), (A_OUT, "a")])
    })
    .await;
    case.build_completed(X).await;

    assert!(case.sb(X).await.fetchable, "after 11: sb(X) fetchable");
    let verdicts = [
        case.evaluation_status(e1).await,
        case.evaluation_status(e2).await,
    ];
    assert_eq!(
        verdicts,
        [EvaluationStatus::Failed, EvaluationStatus::Completed],
        "after 11: evaluation(E1), evaluation(E2)"
    );
    case.zero_drift("step 11").await;

    case.gc(GcRequest::Evaluations { ids: vec![e1] }).await;
    let collected = case
        .gc(GcRequest::Derivations {
            candidates: Vec::new(),
        })
        .await;

    let gone = case.db.find::<evaluation::Entity>(e1).await;
    assert_eq!(gone, None, "after 12: no evaluation(E1)");
    let named = case.jobs(e2).await.len();
    assert_eq!(named, 3, "after 12: E2 adopts nothing and still names B");
    assert_eq!(
        collected.deleted_derivations,
        [],
        "after 12: an empty list deletes nothing"
    );
    let derivations = case.db.rows::<derivation::Entity>().await.len();
    assert_eq!(derivations, 3, "after 12: every derivation is still there");
    let b = case.sb(B).await;
    assert!(b.fetchable, "after 12: B keeps its rows: {b:?}");
    let stored = case.output_stored(B, "out").await;
    assert!(stored, "after 12: B keeps its cached path");
    case.zero_drift("step 12").await;
    case.finish().await;
}

#[tokio::test]
async fn l02_passthrough_life_hit_miss_miss_real_build_verdict_retire_rebuild_as_a_builder() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .output(B_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    case.upstream_hits(&[(A_OUT, narinfo(&[(A_OUT, "a")]))])
        .await;
    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.probed),
        (BuildStatus::Queued, true, true),
        "after 1: sb(A)"
    );
    let status = case.sb(B).await.status;
    assert_eq!(status, BuildStatus::Skipped, "after 1: sb(B)");
    case.zero_drift("step 1").await;

    case.transition(Transition::EvalStreamCompleted { evaluation: e1 })
        .await;

    let status = case.evaluation_status(e1).await;
    assert_eq!(
        status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1)"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;

    for miss in 1..=2 {
        let assignment = case.assigned(e1, A, true).await;
        assert!(assignment.is_some(), "step 3: miss {miss} is assigned");
        case.build_started(A).await;
        case.build_failed(A, BuildFailureKind::SubstituteUnavailable, &[])
            .await;
        case.close_assignment(A).await;
    }

    let (a, b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (a.status, a.cache_available, a.attempt),
        (BuildStatus::Created, false, 0),
        "after 3: sb(A)"
    );
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Created, true),
        "after 3: sb(B)"
    );
    case.zero_drift("step 3").await;

    case.upstream_probed(&[B]).await;
    case.commit_nar(drv_nar(B)).await;
    case.commit_nar(drv_nar(A)).await;

    let status = case.sb(B).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 4: sb(B)");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 4: sb(A)"
    );
    case.zero_drift("step 4").await;

    case.build_started(B).await;
    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;
    case.build_completed(B).await;

    assert!(case.sb(B).await.fetchable, "after 5: sb(B) fetchable");
    let status = case.sb(A).await.status;
    assert_eq!(status, BuildStatus::Queued, "after 5: sb(A)");
    case.zero_drift("step 5").await;

    case.build_started(A).await;
    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;
    case.build_completed(A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.fetchable),
        (BuildStatus::Completed, true),
        "after 6: sb(A)"
    );
    let verdict = case.evaluation_status(e1).await;
    assert_eq!(
        verdict,
        EvaluationStatus::Completed,
        "after 6: evaluation(E1)"
    );
    let frozen = case.counters(e1).await;
    case.zero_drift("step 6").await;

    case.demote(Demotion::MissingNar { hash: A_OUT.into() })
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.wanted),
        (BuildStatus::Queued, false, true),
        "after 7: sb(A)"
    );
    let verdict = case.evaluation_status(e1).await;
    assert_eq!(
        verdict,
        EvaluationStatus::Completed,
        "after 7: evaluation(E1)"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, frozen, "after 7: counters(E1) as at the verdict");
    case.zero_drift("step 7").await;
    case.finish().await;
}

#[tokio::test]
async fn l03_skip_and_thaw_over_three_evaluations_and_a_gc() {
    let (e1, e2, e3) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
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
            .build_status(BuildStatus::Queued)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A2)
            .walked()
            .entry_point("pkg.a2")
            .job()
            .probed()
            .output(A2_OUT)
            .cache_available()
            .depends_on(B)
            .build_status(BuildStatus::Queued)
            .derivation(B)
            .job()
            .evaluation(e3),
    )
    .await;

    case.set_evaluation_status(e1, EvaluationStatus::Aborted)
        .await;
    case.transition(Transition::AbortEvaluationSharedBuilds {
        evaluation: e1,
        hard: true,
    })
    .await;

    let status = case.sb(A1).await.status;
    assert_eq!(status, BuildStatus::Aborted, "after 1: sb(A1)");
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt),
        (BuildStatus::Skipped, 0),
        "after 1: sb(B)"
    );
    let status = case.evaluation_status(e1).await;
    assert_eq!(status, EvaluationStatus::Aborted, "after 1: evaluation(E1)");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;

    let b_as_entry_point = vec![with_attr(with_output(drv(B, &[]), B_OUT), "pkg.b")];
    case.walk(case.tasked(e3, b_as_entry_point).await).await;
    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Queued, true),
        "after 2: sb(B)"
    );
    let counters = [case.counters(e2).await, case.counters(e3).await];
    assert_eq!(
        counters,
        [[2, 2, 0, 2, 0], [1, 1, 0, 1, 0]],
        "after 2: counters(E2), counters(E3)"
    );
    case.zero_drift("step 2").await;

    case.gc(GcRequest::Evaluations { ids: vec![e3] }).await;

    let status = case.sb(B).await.status;
    assert_eq!(status, BuildStatus::Skipped, "after 3: sb(B)");
    let job = case.job(e2, B).await;
    assert!(job.is_some(), "after 3: build_job(E2,B) still exists");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 3: counters(E2)");
    case.zero_drift("step 3").await;

    case.commit_nar(with_references(nar(A2_OUT, "a2"), &[(A2_OUT, "a2")]))
        .await;
    case.build_started(A2).await;
    case.build_completed(A2).await;

    let a2 = case.sb(A2).await;
    assert_eq!(
        (a2.status, a2.fetchable),
        (BuildStatus::Completed, true),
        "after 4: sb(A2)"
    );
    let verdict = case.evaluation_status(e2).await;
    assert_eq!(
        verdict,
        EvaluationStatus::Completed,
        "after 4: evaluation(E2)"
    );
    case.zero_drift("step 4").await;
    case.finish().await;
}
