/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_wire::types::DiscoveredDerivation;

use super::{
    A, A_OUT, B, B_OUT, Case, N, chain, chain_done, drv, nar, narinfo, with_attr, with_output,
    with_references,
};
use crate::messages::Transition;

fn walk_of_a(dependencies: &[&str]) -> Vec<DiscoveredDerivation> {
    vec![with_attr(with_output(drv(A, dependencies), A_OUT), "pkg.a")]
}

#[tokio::test]
async fn x01_two_projects_share_the_rows_of_a_graph_and_finish_through_the_same_builds() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .drv_cached()
            .output(B_OUT)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .entry_point("pkg.a")
            .job()
            .derivation(B)
            .job(),
    )
    .await;

    case.upstream_probed(&[B]).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(
        case.counters(e2).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E2)"
    );
    case.zero_drift("step 1").await;

    let assignment = case.assigned(e1, B, false).await;
    assert!(assignment.is_some(), "step 2: a worker takes B (X9)");
    case.build_started(B).await;
    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;
    case.build_completed(B).await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    let attempts = case.attempts(B).await.len();
    assert_eq!(attempts, 1, "after 2: a single attempt of B");
    case.zero_drift("step 2").await;

    case.build_started(A).await;
    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;
    case.build_completed(A).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(
        case.evaluation_status(e2).await,
        EvaluationStatus::Completed,
        "after 3: evaluation(E2).status"
    );
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn x02_a_second_walk_with_an_extra_dependency_makes_the_first_evaluation_wait() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(chain_done(e1).evaluation(e2)).await;
    let version = case.gv(e1).await;

    case.walk(case.tasked(e2, walk_of_a(&[B, N])).await).await;

    assert_eq!(
        case.edge(A, N).await.map(|edge| edge.kind),
        Some(EdgeKind::Buildtime),
        "after 1: edge(A,N)"
    );
    assert!(
        !case.derivation(N).await.walked,
        "after 1: derivation(N) is a stub"
    );
    assert!(
        case.job(e2, N).await.is_some(),
        "after 1: build_job(E2,N) exists"
    );
    assert!(
        case.job(e1, N).await.is_some(),
        "after 1: build_job(E1,N) exists, E1 reaches N through A (I12)"
    );
    let (top, added) = (case.sb(A).await, case.sb(N).await);
    assert_eq!(top.status, BuildStatus::Created, "after 1: sb(A).status");
    assert_eq!(top.blocking_deps, 1, "after 1: sb(A).blocking_deps");
    assert_eq!(
        case.derivation(A).await.unwalked_inputs,
        1,
        "after 1: derivation(A).unwalked_inputs"
    );
    assert_eq!(added.status, BuildStatus::Created, "after 1: sb(N).status");
    assert!(added.wanted, "after 1: sb(N).wanted");
    assert!(case.gv(e1).await > version, "after 1: gv(E1) rose");
    assert_eq!(
        case.counters(e1).await,
        [3, 2, 0, 0, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(
        case.counters(e2).await,
        [3, 2, 0, 0, 0],
        "after 1: counters(E2), B is Completed (weak spot 5)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn x03_a_hit_of_a_project_makes_the_build_a_passthrough_for_the_other_project_too() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        chain(e1)
            .evaluation(e2)
            .derivation(A)
            .entry_point("pkg.a")
            .job()
            .derivation(B)
            .job(),
    )
    .await;

    case.upstream_hits(&[(B_OUT, narinfo(&[(B_OUT, "b")]))])
        .await;

    let hit = case.sb(B).await;
    assert_eq!(hit.status, BuildStatus::Queued, "after 1: sb(B).status");
    assert!(hit.cache_available, "after 1: sb(B).cache_available");
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(
        case.counters(e2).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E2)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn x04_a_later_evaluation_over_a_finished_graph_completes_at_stream_end_without_a_build() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        chain_done(e1)
            .derivation(A)
            .build_status(BuildStatus::Completed)
            .cached_output(A)
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation(e2),
    )
    .await;

    let report = case.walk(case.tasked(e2, walk_of_a(&[B])).await).await;

    assert_eq!(
        case.counters(e2).await,
        [2, 0, 0, 0, 0],
        "after 1: counters(E2)"
    );
    assert!(!case.sb(A).await.wanted, "after 1: sb(A).wanted");
    assert!(
        report.to_probe.is_empty(),
        "after 1: report.to_probe == []: {:?}",
        report.to_probe
    );
    case.zero_drift("step 1").await;

    case.transition(Transition::EvalStreamCompleted { evaluation: e2 })
        .await;

    assert_eq!(
        case.evaluation_status(e2).await,
        EvaluationStatus::Completed,
        "after 2: evaluation(E2).status"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn x05_a_later_evaluation_asks_the_probe_again_about_an_earlier_miss() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Queued)
            .evaluation(e2),
    )
    .await;

    let report = case.walk(case.tasked(e2, walk_of_a(&[B])).await).await;

    let missed = case.ids(&[B]).await;
    assert!(
        missed.iter().all(|b| report.to_probe.contains(b)),
        "after 1: report.to_probe holds B: {:?}",
        report.to_probe
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}
