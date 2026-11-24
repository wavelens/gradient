/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::status::BuildRefusal;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DerivationBuildId, DerivationId, EvaluationId};
use gradient_entity::{
    build_job, derivation, derivation_build, derivation_dependency, entry_point, evaluation,
};
use gradient_fixtures::shape::{Shape, shape};
use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, IntoActiveModel, TransactionTrait};

use super::{
    A, A_OUT, A1, A1_OUT, A2, A2_OUT, B, B_OUT, BD, BD_OUT, C, C_OUT, Case, D, D_OUT, F, F_OUT, F1,
    F1_OUT, G, G_OUT, G1, G1_OUT, H, H_OUT, H1, H1_OUT, H2, H2_OUT, O, O_OUT, R, R_OUT, S, S_OUT,
    X, X_OUT, assert_only_unstored_outputs, chain, drv, nar, narinfo, pass, with_attr, with_output,
    with_references,
};
use crate::messages::Transition;

#[tokio::test]
async fn d01_any_wanted_build_passes_runtime_edges_and_only_a_builder_passes_build_edges() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .runtime_depends_on(R)
            .depends_on(BD)
            .derivation(R)
            .walked()
            .job()
            .output(R_OUT)
            .derivation(BD)
            .walked()
            .job()
            .output(BD_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    let report = case.drift().await;

    assert_eq!(report.total(), 0, "after 1: total: {report:?}");
    let r = case.sb(R).await;
    assert_eq!(
        (r.status, r.wanted),
        (BuildStatus::Created, true),
        "after 1: sb(R) is Created, wanted true"
    );
    let bd = case.sb(BD).await;
    assert_eq!(
        (bd.status, bd.wanted),
        (BuildStatus::Skipped, false),
        "after 1: sb(Bd) is Skipped, wanted false"
    );

    let gained = case.upstream_probed(&[A]).await;

    let bd = case.sb(BD).await;
    assert_eq!(
        (bd.status, bd.attempt, bd.wanted),
        (BuildStatus::Created, 0, true),
        "after 2: sb(Bd) is Created, attempt 0, wanted true"
    );
    let r_now = derivation_build::Model {
        updated_at: r.updated_at,
        ..case.sb(R).await
    };
    assert_eq!(r_now, r, "after 2: sb(R) unchanged");
    assert_eq!(gained, case.ids(&[BD]).await, "after 2: result == [Bd]");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 0, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

fn top_above_f_g_h(e1: EvaluationId) -> Shape {
    shape()
        .evaluation(e1)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .depends_on(F)
        .depends_on(G)
        .depends_on(H)
}

fn with_g_and_h(top: Shape, h1: BuildStatus) -> Shape {
    top.derivation(G)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(G_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(G)
        .depends_on(G1)
        .derivation(G1)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(G1_OUT)
        .build_status(BuildStatus::Skipped)
        .derivation(H)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(H_OUT)
        .build_status(BuildStatus::Completed)
        .runtime_depends_on(H1)
        .depends_on(H2)
        .derivation(H1)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(H1_OUT)
        .build_status(h1)
        .derivation(H2)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(H2_OUT)
        .build_status(BuildStatus::Skipped)
}

#[tokio::test]
async fn d02_the_walk_stops_at_failed_or_fetchable_builds_and_at_build_edges_of_completed_builds() {
    let e1 = EvaluationId::now_v7();
    let below_a_failed_top = top_above_f_g_h(e1)
        .build_status(BuildStatus::DependencyFailed)
        .derivation(F)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(F_OUT)
        .build_status(BuildStatus::FailedPermanent)
        .attempt(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::BuilderNonzero),
        )
        .depends_on(F1)
        .derivation(F1)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(F1_OUT)
        .build_status(BuildStatus::Skipped);
    let case = Case::start_with_unstored_outputs(
        with_g_and_h(below_a_failed_top, BuildStatus::Skipped),
        1,
    )
    .await;

    assert_only_unstored_outputs(case.drift().await, 1, "after 1");
    for path in [A, F, F1, G, G1, H, H1, H2] {
        let wanted = case.sb(path).await.wanted;
        assert!(!wanted, "after 1: wanted of {path} is false");
    }
    assert_only_unstored_outputs(case.finish_with_drift().await, 1, "at the end");

    let e1 = EvaluationId::now_v7();
    let below_an_open_top = top_above_f_g_h(e1)
        .derivation(F)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(F_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(F);
    let case =
        Case::start_with_unstored_outputs(with_g_and_h(below_an_open_top, BuildStatus::Queued), 1)
            .await;

    assert_only_unstored_outputs(case.drift().await, 1, "second shape after 1");
    for (path, expected) in [(G, false), (G1, false), (H, true), (H1, true), (H2, false)] {
        let wanted = case.sb(path).await.wanted;
        assert_eq!(wanted, expected, "second shape after 1: wanted of {path}");
    }
    let queued = case.status(H1).await;
    assert_eq!(
        queued,
        BuildStatus::Queued,
        "second shape after 1: sb(H1).status"
    );
    let counters = case.counters(e1).await;
    assert_eq!(
        counters,
        [7, 2, 0, 1, 0],
        "second shape after 1: counters(E1)"
    );
    let report = case.finish_with_drift().await;
    assert_only_unstored_outputs(report, 1, "second shape at the end");
}

#[tokio::test]
async fn d03_a_thaw_goes_to_created_with_attempt_zero_and_queues_when_the_conditions_are_met() {
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
    case.override_sb(B, |build| build.attempt = 2).await;

    let gv = case.gv(e1).await;
    case.upstream_probed(&[A]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted),
        (BuildStatus::Queued, 0, true),
        "after 1: sb(B) is Queued, attempt 0, wanted true"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.probed, a.blocking_deps),
        (BuildStatus::Created, true, 1),
        "after 1: sb(A) is Created, probed true, blocking_deps 1"
    );
    assert!(case.gv(e1).await > gv, "after 1: gv(E1) rose");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn d04_an_aborted_build_thaws_when_a_second_evaluation_wants_it() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(A1)
            .walked()
            .entry_point("pkg.a1")
            .job()
            .probed()
            .drv_cached()
            .output(A1_OUT)
            .depends_on(B)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Aborted)
            .evaluation(e2),
    )
    .await;
    case.override_job(e1, B, |job| job.aborted = true).await;

    let walk_of_a2 = case
        .tasked(
            e2,
            vec![with_attr(with_output(drv(A2, &[B]), A2_OUT), "pkg.a2")],
        )
        .await;
    case.walk(walk_of_a2).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Aborted, false),
        "after 1: sb(B) is Aborted, wanted false"
    );
    case.zero_drift("step 1").await;

    case.upstream_probed(&[A2]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted),
        (BuildStatus::Queued, 0, true),
        "after 2: sb(B) is Queued, attempt 0, wanted true"
    );
    let aborted_in_e1 = case.job(e1, B).await.map(|job| job.aborted);
    assert_eq!(
        aborted_in_e1,
        Some(true),
        "after 2: build_job(E1, B).aborted"
    );
    let aborted_in_e2 = case.job(e2, B).await.map(|job| job.aborted);
    assert_eq!(
        aborted_in_e2,
        Some(false),
        "after 2: build_job(E2, B).aborted"
    );
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(
        verdict,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status"
    );
    let failed = case.status(A1).await;
    assert_eq!(
        failed,
        BuildStatus::DependencyFailed,
        "after 2: sb(A1).status"
    );
    let counters = case.counters(e2).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 2: counters(E2)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn d05_aborting_an_evaluation_aborts_only_builds_nobody_else_names_and_releases_the_need() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A1)
            .walked()
            .entry_point("pkg.a1")
            .job()
            .probed()
            .drv_cached()
            .output(A1_OUT)
            .depends_on(S)
            .depends_on(O)
            .derivation(S)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(S_OUT)
            .build_status(BuildStatus::Queued)
            .derivation(O)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(O_OUT)
            .build_status(BuildStatus::Queued)
            .evaluation(e2)
            .derivation(A2)
            .walked()
            .entry_point("pkg.a2")
            .job()
            .probed()
            .output(A2_OUT)
            .cache_available()
            .depends_on(S)
            .build_status(BuildStatus::Queued)
            .derivation(S)
            .job(),
    )
    .await;
    case.set_evaluation_status(e1, EvaluationStatus::Aborted)
        .await;

    case.transition(Transition::AbortEvaluationSharedBuilds {
        evaluation: e1,
        hard: true,
    })
    .await;

    for path in [A1, O] {
        let build = case.sb(path).await;
        assert_eq!(
            (build.status, build.wanted),
            (BuildStatus::Aborted, false),
            "after 1: sb of {path} is Aborted, wanted false"
        );
    }
    let s = case.sb(S).await;
    assert_eq!(
        (s.status, s.wanted, s.attempt),
        (BuildStatus::Skipped, false, 0),
        "after 1: sb(S) is Skipped, wanted false, attempt 0"
    );
    let aborted = case.evaluation(e1).await.status;
    assert_eq!(
        aborted,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E1).status"
    );
    let counters = case.counters(e2).await;
    assert_eq!(counters, [2, 1, 0, 1, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn d06_collecting_the_only_wanting_evaluation_leaves_its_builds_skipped() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    case.collect_evaluations(&[e1]).await;

    let collected = case.db.find::<evaluation::Entity>(e1).await;
    assert!(collected.is_none(), "after 1: evaluation(E1) is gone");
    for path in [A, B] {
        let derivation = case.find_derivation(path).await;
        assert!(derivation.is_some(), "after 1: derivation {path} present");
    }
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::Skipped, false),
        "after 1: sb(A) is Skipped, wanted false"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted, b.attempt),
        (BuildStatus::Skipped, false, 0),
        "after 1: sb(B) is Skipped, wanted false, attempt 0"
    );
    let jobs = case.db.rows::<build_job::Entity>().await;
    assert!(jobs.is_empty(), "after 1: no build_job row: {jobs:?}");
    let entry_points = case.db.rows::<entry_point::Entity>().await;
    assert!(
        entry_points.is_empty(),
        "after 1: no entry_point row: {entry_points:?}"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn d07_a_completed_evaluation_still_wants_the_rebuild_of_its_entry_point_output() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(A)
            .depends_on(B)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Completed)
            .cached_output(B),
    )
    .await;
    let frozen = case.counters(e1).await;

    case.report_missing_nar_of_every_content_variant(A_OUT)
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted, a.fetchable, a.cache_available),
        (BuildStatus::Queued, true, false, false),
        "after 1: sb(A) is Queued, wanted true, fetchable false, cache_available false"
    );
    let stored = case.output_stored(A, "out").await;
    assert!(!stored, "after 1: cached_path(output(A,out).hash) stored");
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Completed, true, false),
        "after 1: sb(B) is Completed, fetchable true, wanted false"
    );
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(
        verdict,
        EvaluationStatus::Completed,
        "after 1: evaluation(E1).status"
    );
    let counters = case.counters(e1).await;
    assert_eq!(
        counters, frozen,
        "after 1: counters(E1), frozen at the verdict"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn d08_an_aborted_entry_point_wants_nothing_until_a_retry_and_a_new_probe_answer() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .build_status(BuildStatus::Queued),
    )
    .await;

    case.abort_build(e1, A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::Aborted, false),
        "after 1: sb(A) is Aborted, wanted false"
    );
    let aborted = case.job(e1, A).await.map(|job| job.aborted);
    assert_eq!(aborted, Some(true), "after 1: build_job(E1, A).aborted");
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted, b.attempt),
        (BuildStatus::Skipped, false, 0),
        "after 1: sb(B) is Skipped, wanted false, attempt 0"
    );
    let status = case.evaluation(e1).await.status;
    assert_eq!(
        status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status, X is still Queued"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 1, 1, 1, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    case.retry_build(e1, A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.attempt, a.probed, a.wanted),
        (BuildStatus::Created, 0, false, true),
        "after 2: sb(A) is Created, attempt 0, probed false, wanted true"
    );
    let aborted = case.job(e1, A).await.map(|job| job.aborted);
    assert_eq!(aborted, Some(false), "after 2: build_job(E1, A).aborted");
    let skipped = case.status(B).await;
    assert_eq!(skipped, BuildStatus::Skipped, "after 2: sb(B).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 1, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;

    case.upstream_probed(&[A]).await;

    let probed = case.sb(A).await.probed;
    assert!(probed, "after 3: sb(A).probed");
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Queued, true),
        "after 3: sb(B) is Queued, wanted true"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 3, 0, 2, 0], "after 3: counters(E1)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn d09_a_build_behind_a_passthrough_does_not_block_its_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(pass(e1).evaluation_status(EvaluationStatus::Building)).await;

    case.commit_nar(with_references(nar(A_OUT, "a"), &[(A_OUT, "a")]))
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.fetchable),
        (BuildStatus::Queued, false),
        "after 1: sb(A) is Queued, fetchable false"
    );
    case.zero_drift("step 1").await;

    case.build_started(A).await;

    case.zero_drift("step 2").await;

    // Report the substitution before the completion, as workers do.
    // Completions without that report end as Completed (answers X9).
    case.build_output(A, true).await;
    case.build_completed(A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.fetchable, a.wanted),
        (BuildStatus::Substituted, true, false),
        "after 3: sb(A) is Substituted, fetchable true, wanted false"
    );
    let skipped = case.status(B).await;
    assert_eq!(skipped, BuildStatus::Skipped, "after 3: sb(B).status");
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(
        verdict,
        EvaluationStatus::Completed,
        "after 3: evaluation(E1).status"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 0, 0, 0, 0], "after 3: counters(E1)");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn d10_a_lost_wanted_update_is_repaired_by_the_check() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    case.override_sb(B, |build| {
        build.wanted = false;
        build.status = BuildStatus::Skipped;
    })
    .await;

    let report = case.drift().await;

    assert_eq!(report.need_drift, 1, "after 1: need_drift: {report:?}");
    assert_eq!(
        report.skipped_moves, 1,
        "after 1: skipped_moves: {report:?}"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted, b.attempt),
        (BuildStatus::Queued, true, 0),
        "after 1: sb(B) is Queued, wanted true, attempt 0"
    );
    assert!(report.total() >= 2, "after 1: total >= 2: {report:?}");

    let report = case.drift().await;

    assert_eq!(report.total(), 0, "after 2: total: {report:?}");
    case.finish().await;
}

#[tokio::test]
async fn d11_a_build_wanted_through_two_parents_stays_wanted_until_both_stop_wanting_it() {
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
            .depends_on(D)
            .derivation(C)
            .walked()
            .entry_point("pkg.c")
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .depends_on(D)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::Queued),
    )
    .await;

    case.upstream_hits(&[(A_OUT, narinfo(&[(A_OUT, "a")]))])
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 1: sb(A) is Queued, cache_available true"
    );
    let d = case.sb(D).await;
    assert_eq!(
        (d.status, d.wanted),
        (BuildStatus::Queued, true),
        "after 1: sb(D) is Queued, wanted true"
    );
    case.zero_drift("step 1").await;

    case.upstream_hits(&[(C_OUT, narinfo(&[(C_OUT, "c")]))])
        .await;

    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.cache_available),
        (BuildStatus::Queued, true),
        "after 2: sb(C) is Queued, cache_available true"
    );
    let d = case.sb(D).await;
    assert_eq!(
        (d.status, d.wanted, d.attempt),
        (BuildStatus::Skipped, false, 0),
        "after 2: sb(D) is Skipped, wanted false, attempt 0"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 2, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn d12_a_chain_of_three_probed_skipped_builds_thaws_to_its_end() {
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
            .build_status(BuildStatus::Skipped)
            .depends_on(C)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::Skipped)
            .depends_on(D)
            .derivation(D)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(D_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    case.upstream_probed(&[A]).await;

    for path in [B, C] {
        let build = case.sb(path).await;
        assert_eq!(
            (build.status, build.wanted),
            (BuildStatus::Created, true),
            "after 1: sb({path}) is Created, wanted true"
        );
    }
    let d = case.sb(D).await;
    assert_eq!(
        (d.status, d.attempt, d.wanted),
        (BuildStatus::Queued, 0, true),
        "after 1: sb(D) is Queued, attempt 0, wanted true"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

fn failed_and_running_evaluation_share_s(failed: EvaluationId, running: EvaluationId) -> Shape {
    shape()
        .evaluation(failed)
        .evaluation_status(EvaluationStatus::Failed)
        .derivation(A1)
        .walked()
        .entry_point("pkg.a1")
        .job()
        .probed()
        .drv_cached()
        .output(A1_OUT)
        .depends_on(S)
        .derivation(S)
        .walked()
        .job()
        .probed()
        .drv_cached()
        .output(S_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .evaluation(running)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A2)
        .walked()
        .entry_point("pkg.a2")
        .job()
        .probed()
        .drv_cached()
        .output(A2_OUT)
        .depends_on(S)
        .derivation(S)
        .job()
}

#[tokio::test]
async fn d13_aborting_an_evaluation_leaves_a_build_a_failed_evaluation_still_wants() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(failed_and_running_evaluation_share_s(e1, e2)).await;
    case.set_evaluation_status(e2, EvaluationStatus::Aborted)
        .await;

    let report = case
        .transition(Transition::AbortEvaluationSharedBuilds {
            evaluation: e2,
            hard: true,
        })
        .await;

    let s = case.sb(S).await;
    assert_eq!(
        (s.status, s.wanted),
        (BuildStatus::Building, true),
        "after 1: sb(S) keeps building for E1"
    );
    let a2 = case.sb(A2).await;
    assert_eq!(
        report.aborted_shared_builds,
        vec![a2.id],
        "after 1: only A2 is aborted"
    );
    assert_eq!(a2.status, BuildStatus::Aborted, "after 1: sb(A2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn d14_an_abort_for_all_evaluations_sticks_once_no_running_evaluation_wants_the_build() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(failed_and_running_evaluation_share_s(e1, e2)).await;
    let s = case.sb(S).await.id;

    let report = case
        .transition(Transition::AbortBuildForAllEvaluations {
            evaluation: e1,
            shared_build: s,
        })
        .await;

    assert_eq!(
        report.refusal,
        Some(BuildRefusal::NeededByOtherEvaluation),
        "after 1: E2 still runs"
    );
    assert_eq!(
        case.sb(S).await.status,
        BuildStatus::Building,
        "after 1: sb(S)"
    );
    case.zero_drift("step 1").await;

    case.set_evaluation_status(e2, EvaluationStatus::Aborted)
        .await;
    case.transition(Transition::AbortEvaluationSharedBuilds {
        evaluation: e2,
        hard: true,
    })
    .await;
    let report = case
        .transition(Transition::AbortBuildForAllEvaluations {
            evaluation: e2,
            shared_build: s,
        })
        .await;

    assert_eq!(report.refusal, None, "after 2: refusal");
    assert_eq!(report.aborted_shared_builds, vec![s], "after 2: stopped");
    let stopped = case.sb(S).await;
    assert_eq!(
        (stopped.status, stopped.wanted),
        (BuildStatus::Aborted, false),
        "after 2: sb(S)"
    );
    assert!(
        case.job(e1, S).await.is_some_and(|job| job.aborted),
        "after 2: build_job(E1, S).aborted"
    );
    assert_eq!(
        case.sb(A1).await.status,
        BuildStatus::DependencyFailed,
        "after 2: sb(A1)"
    );
    case.zero_drift("step 2").await;

    super::signing::run_publish_round_and_assert_success(
        &case,
        gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
    )
    .await;
    case.graph
        .run_consistency_checks()
        .await
        .expect("the consistency check is written");

    assert_eq!(
        case.sb(S).await.status,
        BuildStatus::Aborted,
        "after 3: no recount thaws sb(S)"
    );
    case.finish().await;
}

#[tokio::test]
async fn d15_one_key_budget_defers_forgotten_miss_rewalk_no_build_dep() {
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
            .depends_on(C)
            .build_status(BuildStatus::Skipped)
            .derivation(C)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(C_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;
    let before_e1 = gradient_fixtures::test_date() - chrono::Duration::days(1);
    case.override_sb(B, |build| build.probed_at = Some(before_e1))
        .await;
    case.override_sb(A, |build| {
        build.probed = true;
        build.probed_at = Some(gradient_fixtures::test_date());
    })
    .await;
    let root_derivation_ids: Vec<_> = case.ids(&[A]).await.into_iter().collect();

    let single_lock_update =
        gradient_db::graph::can_start::update_and_settle_need_with_advisory_key_limit(
            &case.ctx.worker_db,
            &root_derivation_ids,
            1,
        )
        .await
        .expect("The bounded need update is written.");

    let deferred_derivation_ids = &single_lock_update.deferred_rewalk_derivations;
    assert!(
        case.ids(&[B])
            .await
            .iter()
            .all(|b| deferred_derivation_ids.contains(b)),
        "after step 1: sb(B) re-walk deferred: {:?}",
        deferred_derivation_ids
    );
    let unpromoted_ids =
        &single_lock_update.wanted_derivation_ids_to_promote_after_deferred_rewalks;
    assert!(
        case.ids(&[B, C])
            .await
            .iter()
            .all(|id| unpromoted_ids.contains(id)),
        "sb(B) and sb(C) wait for promotion after 1: {:?}",
        unpromoted_ids
    );
    assert_eq!(
        case.status(C).await,
        BuildStatus::Created,
        "After 1: sb(C) thawed but not queued"
    );

    gradient_db::graph::can_start::recheck_forgotten_need(
        &case.ctx.worker_db,
        deferred_derivation_ids,
    )
    .await
    .expect("the deferred re-walk is written");

    let c = case.sb(C).await;
    assert_eq!(
        (c.status, c.wanted),
        (BuildStatus::Skipped, false),
        "after 2: sb(C) status, wanted"
    );
    case.finish().await;
}

const TEST_DEPENDENCY_UNWANTED_PARENT_COUNT: usize = 200;

#[tokio::test]
async fn d16_update_need_reads_only_wanted_open_parents_through_partial_index() {
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
            .depends_on(X)
            .derivation(X)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .build_status(BuildStatus::Queued),
    )
    .await;
    insert_many_unwanted_parent_derivations_with_unwanted_shared_build(
        &case,
        X,
        TEST_DEPENDENCY_UNWANTED_PARENT_COUNT,
    )
    .await;
    case.sql("VACUUM (ANALYZE) derivation, derivation_build, derivation_dependency")
        .await;

    let update_need_query_from_statement_registry = gradient_db::sql::registry()
        .find(|query| query.name == "UPDATE_NEED")
        .expect("The statement registry contains `UPDATE_NEED`.");
    let root_derivation_id = case.db.id_of(X).await.into_inner();
    let txn = case
        .db
        .connection()
        .begin()
        .await
        .expect("a transaction opens");
    txn.execute_unprepared("SET LOCAL enable_seqscan = off")
        .await
        .expect("Sequential scan is off.");
    let parsed_explain_analyze_json: serde_json::Value = txn
        .query_one_raw(update_need_query_from_statement_registry.bind_built(
            format!(
                "EXPLAIN (ANALYZE, FORMAT JSON) {}",
                update_need_query_from_statement_registry.text()
            ),
            [sea_orm::Value::from(vec![root_derivation_id])],
        ))
        .await
        .expect("Postgres can run EXPLAIN on UPDATE_NEED and show the query plan.")
        .expect("EXPLAIN commands return one row.")
        .try_get("", "QUERY PLAN")
        .expect("The plan is JSON.");
    txn.rollback().await.expect("Transaction rollback");

    let plan = &parsed_explain_analyze_json;
    let flattened_plan_tree_nodes = collect_explain_plan_nodes(&plan[0]["Plan"]);
    assert!(
        flattened_plan_tree_nodes
            .iter()
            .any(|node| node["Index Name"] == "idx-derivation_build-wanted-open"),
        "The parent probe can use the index of needed open shared builds: {plan}"
    );
    let reverse_edge_index_scan_plan_nodes: Vec<_> = flattened_plan_tree_nodes
        .iter()
        .filter(|node| {
            node["Index Name"] == "idx-derivation_dependency-reverse-pair" && node["Alias"] != "k"
        })
        .collect();
    assert!(
        !reverse_edge_index_scan_plan_nodes.is_empty()
            && reverse_edge_index_scan_plan_nodes
                .iter()
                .all(|node| node["Node Type"] == "Index Only Scan"),
        "The edge list can read only the reverse edge index: {plan}"
    );
    let parent_shared_build_probe: Vec<_> = flattened_plan_tree_nodes
        .iter()
        .filter(|node| node["Relation Name"] == "derivation_build" && node["Alias"] == "p")
        .collect();
    assert!(
        !parent_shared_build_probe.is_empty()
            && parent_shared_build_probe.iter().all(|node| {
                node["Node Type"] == "Index Only Scan"
                    && node["Index Name"] == "idx-derivation_build-wanted-open"
            }),
        "A parent probe can read only the index entries of the open shared build that the probe needs: {plan}"
    );
    let plan_nodes_reading_parent_build_row: Vec<_> = flattened_plan_tree_nodes
        .iter()
        .filter(|node| node["Relation Name"] == "derivation_build" && node["Alias"] == "pb")
        .collect();
    assert!(
        !plan_nodes_reading_parent_build_row.is_empty()
            && plan_nodes_reading_parent_build_row
                .iter()
                .all(|node| node["Actual Loops"] == 1),
        "The builder check does one read of the shared build for the one wanted open parent, not one read for every parent: {plan}"
    );
    case.finish_with_drift().await;
}

#[tokio::test]
async fn d17_needed_outputs_reads_only_wanted_open_parents_through_partial_index() {
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
            .depends_on(X)
            .derivation(X)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .build_status(BuildStatus::Queued),
    )
    .await;
    insert_many_unwanted_parent_derivations_with_unwanted_shared_build(
        &case,
        X,
        TEST_DEPENDENCY_UNWANTED_PARENT_COUNT,
    )
    .await;
    case.sql("VACUUM (ANALYZE) derivation, derivation_build, derivation_dependency")
        .await;

    let needed_outputs_query = gradient_db::sql::registry()
        .find(|query| query.name == "NEEDED_OUTPUTS")
        .expect("The statement registry contains `NEEDED_OUTPUTS`.");
    let dependency_derivation_id = case.db.id_of(X).await.into_inner();
    let plan: serde_json::Value = case
        .db
        .connection()
        .query_one_raw(needed_outputs_query.bind_built(
            format!(
                "EXPLAIN (ANALYZE, FORMAT JSON) {}",
                needed_outputs_query.text()
            ),
            [sea_orm::Value::from(vec![dependency_derivation_id])],
        ))
        .await
        .expect("Postgres can run EXPLAIN on NEEDED_OUTPUTS and show the query plan.")
        .expect("EXPLAIN commands return one row.")
        .try_get("", "QUERY PLAN")
        .expect("The plan is JSON.");

    let parent_shared_build_reads: Vec<_> = collect_explain_plan_nodes(&plan[0]["Plan"])
        .into_iter()
        .filter(|node| node["Relation Name"] == "derivation_build" && node["Alias"] == "p")
        .collect();
    assert!(
        !parent_shared_build_reads.is_empty()
            && parent_shared_build_reads
                .iter()
                .all(|node| node["Index Cond"].is_string()),
        "All reads of the parent shared build in NEEDED_OUTPUTS look up a row by key. They never scan every incomplete shared build that evaluations still need: {plan}"
    );
    case.finish_with_drift().await;
}

async fn insert_many_unwanted_parent_derivations_with_unwanted_shared_build(
    case: &Case,
    dependency: &str,
    parent_count_to_insert: usize,
) {
    let dependency_derivation_row = case.derivation(dependency).await;
    let dependency_derivation_build_row = case.sb(dependency).await;
    let new_parent_derivation_ids: Vec<DerivationId> = (0..parent_count_to_insert)
        .map(|_| DerivationId::now_v7())
        .collect();
    let connection = case.db.connection();

    derivation::Entity::insert_many(new_parent_derivation_ids.iter().enumerate().map(
        |(index, id)| {
            derivation::Model {
                id: *id,
                hash: format!("{index:032}"),
                ..dependency_derivation_row.clone()
            }
            .into_active_model()
            .reset_all()
        },
    ))
    .exec_without_returning(&connection)
    .await
    .expect("Write the parent derivation.");
    derivation_build::Entity::insert_many(new_parent_derivation_ids.iter().map(|id| {
        derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: *id,
            status: BuildStatus::Skipped,
            wanted: false,
            ..dependency_derivation_build_row.clone()
        }
        .into_active_model()
        .reset_all()
    }))
    .exec_without_returning(&connection)
    .await
    .expect("Write the shared builds of the parents.");
    derivation_dependency::Entity::insert_many(new_parent_derivation_ids.iter().map(|id| {
        derivation_dependency::Model {
            derivation: *id,
            dependency: dependency_derivation_row.id,
            kind: EdgeKind::Buildtime,
            dependency_outputs: None,
        }
        .into_active_model()
        .reset_all()
    }))
    .exec_without_returning(&connection)
    .await
    .expect("The parent edges get written");
}

fn collect_explain_plan_nodes(node: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut flattened_plan_tree_nodes = vec![node];
    if let Some(children) = node["Plans"].as_array() {
        for child in children {
            flattened_plan_tree_nodes.extend(collect_explain_plan_nodes(child));
        }
    }

    flattened_plan_tree_nodes
}

#[tokio::test]
async fn d18_two_probe_answers_written_together_leave_same_need_state_as_two_answers_written_one_after_another()
 {
    let two_unprobed_roots = |evaluation| {
        shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .output(A_OUT)
            .depends_on(BD)
            .derivation(BD)
            .walked()
            .job()
            .output(BD_OUT)
            .build_status(BuildStatus::Skipped)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .output(X_OUT)
            .depends_on(O)
            .derivation(O)
            .walked()
            .job()
            .output(O_OUT)
            .build_status(BuildStatus::Skipped)
    };
    let separate = Case::start(two_unprobed_roots(EvaluationId::now_v7())).await;
    let together = Case::start(two_unprobed_roots(EvaluationId::now_v7())).await;

    separate.upstream_probed(&[A]).await;
    separate.upstream_probed(&[X]).await;
    let (a, x) = (
        together.ids(&[A]).await.into_iter().collect(),
        together.ids(&[X]).await.into_iter().collect(),
    );
    let (a_answer, x_answer) = tokio::join!(
        together.graph.upstream_probed(a),
        together.graph.upstream_probed(x)
    );
    a_answer.expect("the answer for A is written");
    x_answer.expect("the answer for X is written");

    for path in [A, BD, X, O] {
        let (s, t) = (separate.sb(path).await, together.sb(path).await);
        assert_eq!(
            (t.status, t.wanted, t.probed, t.attempt, t.blocking_deps),
            (s.status, s.wanted, s.probed, s.attempt, s.blocking_deps),
            "need state of {path}"
        );
    }
    assert!(
        together.sb(O).await.wanted,
        "the answer for X made O needed"
    );
    separate.zero_drift("separate").await;
    together.zero_drift("together").await;
    separate.finish().await;
    together.finish().await;
}
