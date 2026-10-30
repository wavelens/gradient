/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_entity::{build_job, derivation_build, entry_point, evaluation};
use gradient_fixtures::shape::{Shape, shape};

use super::{
    A, A_OUT, A1, A1_OUT, A2, A2_OUT, B, B_OUT, BD, BD_OUT, C, C_OUT, Case, D, D_OUT, F, F_OUT, F1,
    F1_OUT, G, G_OUT, G1, G1_OUT, H, H_OUT, H1, H1_OUT, H2, H2_OUT, O, O_OUT, R, R_OUT, S, S_OUT,
    assert_only_unstored_outputs, chain, drv, nar, narinfo, pass, with_attr, with_output,
    with_references,
};
use crate::messages::{Demotion, GcRequest, Transition};

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

    case.gc(GcRequest::Evaluations { ids: vec![e1] }).await;

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

    case.demote(Demotion::MissingNar { hash: A_OUT.into() })
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
    let case = Case::start(chain(e1).evaluation_status(EvaluationStatus::Building)).await;

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
    let verdict = case.evaluation(e1).await.status;
    assert_eq!(
        verdict,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1).status"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 0, 1, 0, 0], "after 1: counters(E1)");
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
    let reopened = case.evaluation(e1).await.status;
    assert_eq!(
        reopened,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 1, 0, 0, 0], "after 2: counters(E1)");
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
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 3: counters(E1)");
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
