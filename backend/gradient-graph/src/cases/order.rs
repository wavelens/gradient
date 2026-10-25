/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, DerivationId, EvaluationId};
use gradient_entity::{
    build_job, derivation, derivation_build, derivation_dependency, derivation_output, evaluation,
};
use gradient_fixtures::shape::{Shape, shape};
use gradient_fixtures::test_date;

use super::{
    A, A_OUT, B, B_OUT, Case, GraphRows, as_set, chain, chain_done, drv, nar, narinfo, with_attr,
    with_output, with_references,
};
use crate::messages::{Demotion, GcRequest, NarCommit};

type EndState = (Vec<derivation_build::Model>, [[i64; 5]; 2]);

fn without_identity(build: derivation_build::Model) -> derivation_build::Model {
    derivation_build::Model {
        id: Default::default(),
        derivation: Default::default(),
        created_at: test_date(),
        updated_at: test_date(),
        probed_at: None,
        queued_at: None,
        ready_at: None,
        dispatched_at: None,
        ..build
    }
}

fn named(jobs: &[build_job::Model]) -> BTreeSet<DerivationId> {
    jobs.iter().map(|job| job.derivation).collect()
}

async fn entry_points(case: &Case, evaluation: EvaluationId) -> Vec<(String, DerivationId)> {
    case.entry_points(evaluation)
        .await
        .into_iter()
        .map(|point| (point.eval, point.derivation))
        .collect()
}

async fn overlapping_walks(e1_first: bool) -> EndState {
    let variant = if e1_first { "a" } else { "b" };
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(shape().evaluation(e1).evaluation(e2)).await;
    let a_above_b = vec![
        with_attr(with_output(drv(A, &[B]), A_OUT), "pkg.a"),
        with_output(drv(B, &[]), B_OUT),
    ];
    let b_alone = vec![with_attr(with_output(drv(B, &[]), B_OUT), "pkg.b")];
    let (of_e1, of_e2) = (
        case.tasked(e1, a_above_b).await,
        case.tasked(e2, b_alone).await,
    );
    let (first, second) = if e1_first {
        (of_e1, of_e2)
    } else {
        (of_e2, of_e1)
    };

    case.walk(first).await;

    let b = case.sb(B).await;
    if e1_first {
        assert_eq!(b.status, BuildStatus::Skipped, "a after 1: sb(B)");
    } else {
        assert_eq!(
            (b.status, b.wanted),
            (BuildStatus::Created, true),
            "b after 1: sb(B)"
        );
    }
    case.zero_drift("step 1").await;

    case.walk(second).await;

    let derivations = case.db.rows::<derivation::Entity>().await.len();
    assert_eq!(derivations, 2, "{variant} after 2: derivations");
    let edges = case.db.rows::<derivation_dependency::Entity>().await.len();
    assert_eq!(edges, 1, "{variant} after 2: edges");
    let outputs = case.db.rows::<derivation_output::Entity>().await.len();
    assert_eq!(outputs, 2, "{variant} after 2: outputs");
    assert_eq!(
        named(&case.jobs(e1).await),
        case.ids(&[A, B]).await,
        "{variant} after 2: build_job rows of E1"
    );
    assert_eq!(
        named(&case.jobs(e2).await),
        case.ids(&[B]).await,
        "{variant} after 2: build_job rows of E2"
    );
    let (id_of_a, id_of_b) = (case.db.id_of(A).await, case.db.id_of(B).await);
    assert_eq!(
        entry_points(&case, e1).await,
        [("pkg.a".to_owned(), id_of_a)],
        "{variant} after 2: entry points of E1"
    );
    assert_eq!(
        entry_points(&case, e2).await,
        [("pkg.b".to_owned(), id_of_b)],
        "{variant} after 2: entry points of E2"
    );
    let (a, b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::Created, true),
        "{variant} after 2: sb(A)"
    );
    assert_eq!(
        (b.status, b.wanted, b.probed),
        (BuildStatus::Created, true, false),
        "{variant} after 2: sb(B)"
    );
    let counters = [case.counters(e1).await, case.counters(e2).await];
    assert_eq!(
        counters,
        [[2, 2, 0, 0, 0], [1, 1, 0, 0, 0]],
        "{variant} after 2: counters(E1), counters(E2)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;

    (vec![without_identity(a), without_identity(b)], counters)
}

fn walked_a_above_a_stub(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
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
}

async fn state(case: &Case, evaluation: EvaluationId) -> (GraphRows, Option<i64>) {
    let graph_version = case
        .db
        .find::<evaluation::Entity>(evaluation)
        .await
        .map(|row| row.graph_version);

    (case.rows().await, graph_version)
}

#[tokio::test]
async fn o01_two_batches_of_overlapping_graphs_from_two_evaluations_converge_in_either_order() {
    let e2_first = overlapping_walks(false).await;
    let e1_first = overlapping_walks(true).await;

    assert_eq!(e1_first, e2_first, "end states equal except gv");
}

#[tokio::test]
async fn o02_a_hit_before_the_dependency_is_walked_is_lost_and_has_to_be_asked_again() {
    let e1 = EvaluationId::now_v7();
    let hit = [(B_OUT, narinfo(&[(B_OUT, "b")]))];
    let walk_of_b = || vec![with_output(drv(B, &[]), B_OUT)];

    let case = Case::start(walked_a_above_a_stub(e1)).await;
    case.upstream_hits(&hit).await;
    case.zero_drift("a step 1").await;
    let report = case.walk(case.tasked(e1, walk_of_b()).await).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available, b.probed),
        (BuildStatus::Created, false, false),
        "a after 2: sb(B)"
    );
    let output = case.output_of(B, "out").await;
    assert_eq!(
        output.external_url, None,
        "a after 2: output(B,out).external_url"
    );
    assert_eq!(
        as_set(&report.to_probe),
        case.ids(&[B]).await,
        "a after 2: report.to_probe"
    );
    case.zero_drift("a step 2").await;
    case.finish().await;

    let case = Case::start(walked_a_above_a_stub(e1)).await;
    case.walk(case.tasked(e1, walk_of_b()).await).await;
    case.zero_drift("b step 1").await;
    case.upstream_hits(&hit).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.cache_available),
        (BuildStatus::Queued, true),
        "b after 2: sb(B)"
    );
    case.zero_drift("b step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn o03_a_replay_of_every_mutating_operation_changes_nothing_the_second_time() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let case = Case::start(
        chain_done(e1)
            .into_cache(c1)
            .keyed(Case::key())
            .evaluation_status(EvaluationStatus::Building),
    )
    .await;

    let hit = [(A_OUT, narinfo(&[(A_OUT, "a")]))];
    let seeded = state(&case, e1).await;
    case.upstream_hits(&hit).await;
    let once = state(&case, e1).await;
    case.upstream_hits(&hit).await;

    assert_eq!(once.1, seeded.1, "1: gv unchanged by the first call");
    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "1: every row and gv equal to after the first");
    case.zero_drift("pair 1").await;

    case.build_started(A).await;
    let once = state(&case, e1).await;
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 1], "2: counters(E1) the first time");
    case.build_started(A).await;

    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "2: every row and gv equal to after the first");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 1], "2: counters(E1) the second time");
    case.zero_drift("pair 2").await;

    let commit = NarCommit {
        targets: case.project_caches(e1).await,
        ..with_references(nar(A_OUT, "a"), &[(A_OUT, "a")])
    };
    let started = state(&case, e1).await;
    let first = case.commit_nar(commit.clone()).await;
    let once = state(&case, e1).await;
    let second = case.commit_nar(commit).await;

    assert_eq!(once.1, started.1, "3: gv unchanged by the first call");
    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "3: every row equal, the signature bytes too");
    assert!(first.created, "3: the first report");
    assert!(!second.created, "3: the second report");
    case.zero_drift("pair 3").await;

    case.build_completed(A).await;
    let once = state(&case, e1).await;
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(verdict, EvaluationStatus::Completed, "4: the first time");
    case.build_completed(A).await;

    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "4: every row and gv equal to after the first");
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(verdict, EvaluationStatus::Completed, "4: the second time");
    case.zero_drift("pair 4").await;

    let missing = Demotion::MissingNar { hash: A_OUT.into() };
    case.demote(missing.clone()).await;
    let once = state(&case, e1).await;
    let reset = case.sb(A).await.status;
    assert!(
        matches!(reset, BuildStatus::Created | BuildStatus::Queued),
        "5: sb(A) Created or Queued after the first call: {reset:?}"
    );
    case.demote(missing).await;

    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "5: the second call changes no row");
    case.zero_drift("pair 5").await;

    let collected = GcRequest::Evaluations { ids: vec![e1] };
    case.gc(collected.clone()).await;
    let once = state(&case, e1).await;
    case.gc(collected).await;

    let twice = state(&case, e1).await;
    assert_eq!(twice, once, "6: the second call changes no row");
    case.zero_drift("pair 6").await;
    case.finish().await;
}

#[tokio::test]
async fn o04_a_runtime_reference_closing_a_cycle_is_refused() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain(e1)
            .derivation(B)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None),
    )
    .await;

    let commit = with_references(nar(B_OUT, "b"), &[(B_OUT, "b"), (A_OUT, "a")]);
    case.commit_nar(commit).await;

    let edge = case.edge(B, A).await;
    assert!(edge.is_none(), "after 1: no edge(B,A): {edge:?}");
    let (a, b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (b.status, b.missing_runtime_deps),
        (BuildStatus::Building, 0),
        "after 1: sb(B)"
    );
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 1: sb(A)"
    );
    case.zero_drift("step 1").await;

    case.build_completed(B).await;

    let (a, b) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Completed, true),
        "after 2: sb(B)"
    );
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Queued, 0),
        "after 2: sb(A)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}
