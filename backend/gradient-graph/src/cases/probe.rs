/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::derivation_dependency::{self, EdgeKind};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DerivationId, EvaluationId};
use gradient_fixtures::shape::{Shape, shape};

use super::{
    A, A_OUT, A_OUT2, B, B_OUT, Case, F_OUT, F1, F2, R, R_OUT, UNKNOWN_OUT, as_set, drv, narinfo,
    with_output,
};
use crate::messages::UpstreamHit;

async fn hit_on_a(case: &Case) -> BTreeSet<DerivationId> {
    case.upstream_hits(&[(A_OUT, narinfo(&[(A_OUT, "a")]))])
        .await
}

fn entry_point_without_a_probe_answer(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .output(A_OUT)
        .depends_on(B)
        .runtime_depends_on(R)
        .derivation(B)
        .walked()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped)
        .derivation(R)
        .walked()
        .job()
        .output(R_OUT)
}

#[tokio::test]
async fn p01_a_hit_stores_the_narinfo_adds_runtime_edges_and_queues_the_passthrough() {
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

    let gv = case.gv(e1).await;
    let hit = UpstreamHit {
        deriver: Some(A.to_owned()),
        ..narinfo(&[(A_OUT, "a"), (B_OUT, "b")])
    };

    let gained = case.upstream_hits(&[(A_OUT, hit)]).await;

    let out = case.output_of(A, "out").await;
    let stored = case.output_stored(A, "out").await;
    assert_eq!(
        (
            out.external_url.as_deref(),
            out.nar_hash.as_deref(),
            out.file_hash.as_deref(),
            out.file_size,
            out.nar_size,
            stored
        ),
        (Some("u"), Some("nh"), Some("fh"), Some(1), Some(2), false),
        "after 1: output(A,out) url, nar_hash, file_hash, file_size, nar_size, stored"
    );

    let references = format!("{A_OUT}-a {B_OUT}-b");
    assert_eq!(out.references, Some(references), "after 1: references_list");
    let kind = case.edge(A, B).await.map(|edge| edge.kind);
    assert_eq!(kind, Some(EdgeKind::Both), "after 1: edge(A,B).kind");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.probed, a.wanted),
        (BuildStatus::Queued, true, false, true),
        "after 1: sb(A) status, cache_available, probed, wanted"
    );

    assert_eq!(
        (a.blocking_deps, a.missing_runtime_deps, a.fetchable),
        (1, 1, false),
        "after 1: sb(A) blocking_deps, missing_runtime_deps, fetchable"
    );

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted, b.probed),
        (BuildStatus::Created, 0, true, false),
        "after 1: sb(B) status, attempt, wanted, probed"
    );

    let wanted = case.ids(&[B]).await;
    assert!(gained.is_superset(&wanted), "after 1: result: {gained:?}");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1) +1");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn p02_cache_available_needs_every_output_and_the_second_hit_supplies_it() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .named_output(A_OUT2, "dev"),
    )
    .await;

    hit_on_a(&case).await;

    let out = case.output_of(A, "out").await.external_url;
    assert!(out.is_some(), "after 1: output(A,out).external_url");
    let dev = case.output_of(A, "dev").await.external_url;
    assert_eq!(dev, None, "after 1: output(A,dev).external_url");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.wanted),
        (BuildStatus::Created, false, true),
        "after 1: sb(A) status, cache_available, wanted"
    );

    case.zero_drift("step 1").await;

    let second = narinfo(&[(A_OUT2, "a-dev"), (A_OUT, "a")]);
    case.upstream_hits(&[(A_OUT2, second)]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.missing_runtime_deps),
        (BuildStatus::Created, true, 0),
        "after 2: sb(A) status, cache_available, missing_runtime_deps (answers Q5)"
    );

    case.zero_drift("step 2").await;

    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.probed),
        (BuildStatus::Queued, true),
        "after 3: sb(A) status, probed (answers Q5)"
    );

    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn p03_a_hit_on_a_terminal_success_build_changes_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(A),
    )
    .await;

    let gv = case.gv(e1).await;

    let gained = hit_on_a(&case).await;

    let url = case.output_of(A, "out").await.external_url;
    assert_eq!(url, None, "after 1: output(A,out).external_url");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.fetchable),
        (BuildStatus::Completed, false, true),
        "after 1: sb(A) status, cache_available, fetchable"
    );

    assert!(gained.is_empty(), "after 1: result == []: {gained:?}");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1) unchanged");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn p04_a_hit_turns_a_failed_build_into_a_passthrough_that_queues_after_its_retry() {
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
            .output(A_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            )
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    hit_on_a(&case).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.wanted),
        (BuildStatus::FailedPermanent, true, false),
        "after 1: sb(A) status, cache_available, wanted"
    );

    let b = case.sb(B).await.status;
    assert_eq!(b, BuildStatus::Skipped, "after 1: sb(B).status");
    case.zero_drift("step 1").await;

    case.retry_build(e1, A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.attempt),
        (BuildStatus::Queued, true, 0),
        "after 2: sb(A) status, cache_available, attempt"
    );

    let job = case.job(e1, A).await.expect("after 2: build_job(E1,A)");
    assert!(!job.aborted, "after 2: build_job(E1,A).aborted: {job:?}");
    let evaluation = case.evaluation(e1).await.status;
    assert_eq!(
        evaluation,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );

    let b = case.sb(B).await.status;
    assert_eq!(b, BuildStatus::Skipped, "after 2: sb(B).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn p05_a_miss_makes_the_build_a_builder_and_thaws_its_build_inputs() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(entry_point_without_a_probe_answer(e1)).await;
    let (gv, r) = (case.gv(e1).await, case.sb(R).await);

    let gained = case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.probed, a.status, a.blocking_deps),
        (true, BuildStatus::Created, 1),
        "after 1: sb(A) probed, status, blocking_deps (answers X1)"
    );

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted, b.probed),
        (BuildStatus::Created, 0, true, false),
        "after 1: sb(B) status, attempt, wanted, probed"
    );

    assert_eq!(case.sb(R).await, r, "after 1: sb(R) unchanged");
    let wanted = case.ids(&[B]).await;
    assert!(gained.is_superset(&wanted), "after 1: result: {gained:?}");
    assert_eq!(case.gv(e1).await, gv + 1, "after 1: gv(E1) +1");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn p06_answering_an_already_probed_build_changes_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(entry_point_without_a_probe_answer(e1)).await;

    case.upstream_probed(&[A]).await;

    case.zero_drift("step 1").await;
    let (gv, rows) = (case.gv(e1).await, case.rows().await);

    let gained = case.upstream_probed(&[A]).await;

    assert!(gained.is_empty(), "after 2: result == []: {gained:?}");
    assert_eq!(case.gv(e1).await, gv, "after 2: gv(E1) as after 1");
    assert_eq!(case.rows().await, rows, "after 2: all rows as after 1");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn p07_hits_and_probed_end_in_the_same_state_in_either_order() {
    for hit_first in [true, false] {
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
                .probed()
                .drv_cached()
                .output(B_OUT)
                .build_status(BuildStatus::Skipped),
        )
        .await;

        if hit_first {
            hit_on_a(&case).await;
            case.zero_drift("variant a step 1").await;
            case.upstream_probed(&[A]).await;
        } else {
            case.upstream_probed(&[A]).await;

            let (a, b) = (case.sb(A).await, case.sb(B).await);
            assert_eq!(
                (b.status, b.wanted),
                (BuildStatus::Queued, true),
                "variant b after 1: sb(B) status, wanted"
            );

            assert_eq!(
                (a.status, a.probed),
                (BuildStatus::Created, true),
                "variant b after 1: sb(A) status, probed"
            );

            case.zero_drift("variant b step 1").await;
            hit_on_a(&case).await;
        }

        let (a, b) = (case.sb(A).await, case.sb(B).await);
        assert_eq!(
            (a.status, a.cache_available, a.probed, a.blocking_deps),
            (BuildStatus::Queued, true, true, 1),
            "after 2, hit first {hit_first}: sb(A) status, cache_available, probed, blocking_deps"
        );

        assert_eq!(
            (b.status, b.wanted, b.attempt),
            (BuildStatus::Skipped, false, 0),
            "after 2, hit first {hit_first}: sb(B) status, wanted, attempt"
        );

        let counters = case.counters(e1).await;
        assert_eq!(
            counters,
            [2, 1, 0, 1, 0],
            "after 2, hit first {hit_first}: counters(E1)"
        );

        case.zero_drift("step 2").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn p08_a_probe_answer_leaves_a_stub_unprobed_until_its_walk_is_answered() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .derivation(B)
            .job()
            .drv_cached(),
    )
    .await;

    let outputs = case.rows().await["derivation_output"].clone();

    let gained = case
        .upstream_hits(&[(B_OUT, narinfo(&[(B_OUT, "b")]))])
        .await;

    assert!(gained.is_empty(), "after 1: result == []: {gained:?}");
    assert_eq!(
        case.rows().await["derivation_output"],
        outputs,
        "after 1: no output row changed"
    );

    case.zero_drift("step 1").await;

    let gained = case.upstream_probed(&[B]).await;

    let probed = case.sb(B).await.probed;
    assert!(
        !probed,
        "after 2: sb(B).probed, a stub has no outputs to ask for"
    );
    assert!(gained.is_empty(), "after 2: result == []: {gained:?}");
    case.zero_drift("step 2").await;

    let walked = vec![with_output(drv(B, &[]), B_OUT)];
    let report = case.walk(case.tasked(e1, walked).await).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.probed, b.wanted),
        (BuildStatus::Created, false, true),
        "after 3: sb(B) status, probed, wanted"
    );
    assert_eq!(
        as_set(&report.to_probe),
        case.ids(&[B]).await,
        "after 3: report.to_probe"
    );
    case.zero_drift("step 3").await;

    case.upstream_probed(&[B]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.probed),
        (BuildStatus::Queued, true),
        "after 4: sb(B) status, probed"
    );
    case.zero_drift("step 4").await;
    case.finish().await;
}

#[tokio::test]
async fn p09_a_hit_reference_adds_an_edge_to_each_known_producer_and_none_for_an_unknown_path() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .derivation(F1)
            .walked()
            .job()
            .output(F_OUT)
            .build_status(BuildStatus::Skipped)
            .derivation(F2)
            .walked()
            .job()
            .output(F_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    let references = [(A_OUT, "a"), (F_OUT, "f"), (UNKNOWN_OUT, "unknown")];

    let gained = case.upstream_hits(&[(A_OUT, narinfo(&references))]).await;

    for producer in [F1, F2] {
        let kind = case.edge(A, producer).await.map(|edge| edge.kind);
        assert_eq!(
            kind,
            Some(EdgeKind::Runtime),
            "after 1: edge(A,{producer}).kind"
        );
    }

    let edges = case.db.rows::<derivation_dependency::Entity>().await.len();
    assert_eq!(edges, 2, "after 1: no edge for UNKNOWN_OUT");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.missing_runtime_deps),
        (BuildStatus::Created, true, 2),
        "after 1: sb(A) status, cache_available, missing_runtime_deps (answers Q7)"
    );

    for producer in [F1, F2] {
        let build = case.sb(producer).await;
        assert_eq!(
            (build.status, build.wanted, build.attempt),
            (BuildStatus::Created, true, 0),
            "after 1: sb({producer}) status, wanted, attempt"
        );
    }

    let wanted = case.ids(&[F1, F2]).await;
    assert!(gained.is_superset(&wanted), "after 1: result: {gained:?}");
    case.zero_drift("step 1").await;

    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await.status;
    assert_eq!(a, BuildStatus::Queued, "after 2: sb(A).status (answers Q7)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn p10_a_hit_on_an_output_in_our_cache_writes_nothing_and_makes_no_passthrough() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .cached_output(A),
    )
    .await;

    let gained = hit_on_a(&case).await;

    let url = case.output_of(A, "out").await.external_url;
    assert_eq!(url, None, "after 1: output(A,out).external_url");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.probed),
        (BuildStatus::Created, false, false),
        "after 1: sb(A) status, cache_available, probed"
    );

    assert!(gained.is_empty(), "after 1: result == []: {gained:?}");
    case.zero_drift("step 1").await;
    case.finish().await;
}
