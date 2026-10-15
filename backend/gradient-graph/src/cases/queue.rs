/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::ids::EvaluationId;
use gradient_entity::{dispatched_job, evaluation};
use gradient_fixtures::shape::shape;

use super::{
    A, A_OUT, B, B_OUT, Case, X, X_OUT, Y, Y_OUT, assert_only_unstored_outputs, chain, chain_done,
    drv, drv_nar, nar, narinfo, pass, with_attr, with_output, with_references,
};
use crate::messages::{Demotion, GcRequest, Transition};

#[tokio::test]
async fn q01_a_build_queues_only_after_the_walk_and_the_probe_answer() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .probed()
            .drv_cached()
            .output(X_OUT)
            .depends_on(A)
            .derivation(A)
            .job()
            .drv_cached(),
    )
    .await;

    let walk_of_a = case.tasked(e1, vec![with_output(drv(A, &[]), A_OUT)]).await;
    case.walk(walk_of_a).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.probed, a.wanted),
        (BuildStatus::Created, false, true),
        "after 1: sb(A) is Created, probed false, wanted true"
    );
    let walked = case.derivation(A).await.walked;
    assert!(walked, "after 1: derivation(A).walked");
    case.zero_drift("step 1").await;

    let gv = case.gv(e1).await;
    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.probed),
        (BuildStatus::Queued, true),
        "after 2: sb(A) is Queued, probed true"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 1, 0], "after 2: counters(E1)");
    assert!(case.gv(e1).await > gv, "after 2: gv(E1) rose");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn q02_adoption_supplies_the_job_when_the_only_naming_evaluation_is_collected() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        shape()
            .evaluation(e1)
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
            .drv_cached()
            .output(A_OUT)
            .build_status(BuildStatus::Queued)
            .evaluation(e2)
            .derivation(Y)
            .walked()
            .entry_point("pkg.y")
            .job()
            .probed()
            .drv_cached()
            .output(Y_OUT)
            .depends_on(X)
            .derivation(X)
            .job(),
    )
    .await;

    let gv = case.gv(e2).await;
    case.gc(GcRequest::Evaluations { ids: vec![e1] }).await;

    let collected = case.db.find::<evaluation::Entity>(e1).await;
    assert!(collected.is_none(), "after 1: evaluation(E1) is gone");
    let jobs = case.jobs(e1).await;
    assert!(jobs.is_empty(), "after 1: no build_job of E1: {jobs:?}");
    let entry_points = case.entry_points(e1).await;
    assert!(
        entry_points.is_empty(),
        "after 1: no entry_point of E1: {entry_points:?}"
    );
    let adopted = case.job(e2, A).await;
    assert!(adopted.is_some(), "after 1: build_job(E2, A) exists");
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::Queued, true),
        "after 1: sb(A) is Queued, wanted true"
    );
    assert!(case.gv(e2).await > gv, "after 1: gv(E2) rose");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [3, 3, 0, 1, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn q03_an_entry_point_of_a_second_evaluation_thaws_and_queues_the_skipped_build() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(pass(e1).evaluation(e2)).await;

    let walk_of_b = case
        .tasked(
            e2,
            vec![with_attr(with_output(drv(B, &[]), B_OUT), "pkg.b")],
        )
        .await;
    case.walk(walk_of_b).await;

    let entry_point = case.entry_point(e2, "pkg.b").await.map(|p| p.derivation);
    assert_eq!(
        entry_point,
        Some(case.db.id_of(B).await),
        "after 1: entry_point(E2, pkg.b) exists"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.attempt, b.wanted),
        (BuildStatus::Queued, 0, true),
        "after 1: sb(B) is Queued, attempt 0, wanted true"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 1: sb(A) is Queued, cache_available true"
    );
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 2, 0], "after 1: counters(E1)");
    let counters = case.counters(e2).await;
    assert_eq!(counters, [1, 1, 0, 1, 0], "after 1: counters(E2)");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn q04_a_probe_answer_and_a_hit_both_queue_and_an_assignment_of_the_wrong_kind_is_refused() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .entry_point("pkg.a")
            .job()
            .drv_cached()
            .output(A_OUT)
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

    case.upstream_probed(&[A]).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, false),
        "after 1: sb(A) is Queued, cache_available false"
    );
    case.zero_drift("step 1").await;

    case.upstream_hits(&[(A_OUT, narinfo(&[(A_OUT, "a")]))])
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available),
        (BuildStatus::Queued, true),
        "after 2: sb(A) is Queued, cache_available true"
    );
    case.zero_drift("step 2").await;

    let as_a_build = case.assigned(e1, A, false).await;

    assert_eq!(as_a_build, None, "after 3: refused");
    let open = case.open_assignment(A).await;
    assert!(open.is_none(), "after 3: no open dispatched_job of A");
    let queued = case.status(A).await;
    assert_eq!(queued, BuildStatus::Queued, "after 3: sb(A).status");
    case.zero_drift("step 3").await;

    let as_a_substitution = case.assigned(e1, A, true).await;

    assert!(as_a_substitution.is_some(), "after 4: accepted");
    let open = case.open_assignment(A).await.map(|job| job.id);
    assert_eq!(open, as_a_substitution, "after 4: dispatched_job J2 open");
    case.zero_drift("step 4").await;
    case.finish().await;
}

#[tokio::test]
async fn q05_the_build_queues_only_after_its_dependency_and_its_drv_in_either_order() {
    for drv_first in [false, true] {
        let variant = if drv_first { "b" } else { "a" };
        let e1 = EvaluationId::now_v7();
        let case = Case::start_with_unstored_outputs(
            shape()
                .evaluation(e1)
                .derivation(A)
                .walked()
                .entry_point("pkg.a")
                .job()
                .probed()
                .output(A_OUT)
                .depends_on(B)
                .derivation(B)
                .walked()
                .job()
                .probed()
                .drv_cached()
                .output(B_OUT)
                .build_status(BuildStatus::Completed),
            1,
        )
        .await;

        let output_of_b = with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]);
        let (first, second) = if drv_first {
            (drv_nar(A), output_of_b)
        } else {
            (output_of_b, drv_nar(A))
        };

        case.commit_nar(first).await;

        let waiting = case.status(A).await;
        assert_eq!(
            waiting,
            BuildStatus::Created,
            "variant {variant} after 1: sb(A).status"
        );
        if drv_first {
            assert_only_unstored_outputs(case.drift().await, 1, "variant b after 1");
        } else {
            case.zero_drift("step 1").await;
        }

        case.commit_nar(second).await;

        let a = case.sb(A).await;
        assert_eq!(
            (a.status, a.blocking_deps),
            (BuildStatus::Queued, 0),
            "variant {variant} after 2: sb(A) is Queued, blocking_deps 0"
        );
        let fetchable = case.sb(B).await.fetchable;
        assert!(fetchable, "variant {variant} after 2: sb(B).fetchable");
        let counters = case.counters(e1).await;
        assert_eq!(
            counters,
            [2, 1, 0, 1, 0],
            "variant {variant} after 2: counters(E1)"
        );
        case.zero_drift("step 2").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn q06_a_regressing_condition_unqueues_a_build_only_without_an_open_assignment() {
    for assigned in [false, true] {
        let e1 = EvaluationId::now_v7();
        let seed = chain_done(e1);
        let case = Case::start(if assigned {
            seed.derivation(A).open_assignment()
        } else {
            seed
        })
        .await;

        case.demote(Demotion::MissingNar { hash: B_OUT.into() })
            .await;

        let (variant, status_of_a, queued) = if assigned {
            ("b", BuildStatus::Queued, 2)
        } else {
            ("a", BuildStatus::Created, 1)
        };
        let a = case.sb(A).await;
        assert_eq!(
            (a.status, a.blocking_deps),
            (status_of_a, 1),
            "variant {variant} after 1: sb(A) status, blocking_deps 1"
        );
        let b = case.sb(B).await;
        assert_eq!(
            (b.status, b.fetchable),
            (BuildStatus::Queued, false),
            "variant {variant} after 1: sb(B) is Queued, fetchable false"
        );
        let counters = case.counters(e1).await;
        assert_eq!(
            counters,
            [2, 2, 0, queued, 0],
            "variant {variant} after 1: counters(E1)"
        );
        case.zero_drift("step 1").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn q07_a_passthrough_queues_without_probe_dependencies_or_drv_and_is_skipped_unwanted() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(X)
            .walked()
            .entry_point("pkg.x")
            .job()
            .output(X_OUT)
            .cache_available()
            .depends_on(A)
            .build_status(BuildStatus::Queued)
            .derivation(A)
            .walked()
            .job()
            .output(A_OUT)
            .cache_available()
            .depends_on(B)
            .build_status(BuildStatus::Skipped)
            .derivation(B)
            .job()
            .build_status(BuildStatus::Skipped),
    )
    .await;

    let report = case.drift().await;

    assert_eq!(report.total(), 0, "after 1: total: {report:?}");

    let walk_of_a = case
        .tasked(
            e1,
            vec![with_attr(with_output(drv(A, &[B]), A_OUT), "pkg.a")],
        )
        .await;
    case.walk(walk_of_a).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.cache_available, a.attempt, a.wanted),
        (BuildStatus::Queued, true, 0, true),
        "after 2: sb(A) is Queued, cache_available true, attempt 0, wanted true"
    );
    let skipped = case.status(B).await;
    assert_eq!(skipped, BuildStatus::Skipped, "after 2: sb(B).status");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [3, 2, 0, 2, 0], "after 2: counters(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn q08_a_created_build_with_all_conditions_is_reported_and_queued_by_the_check() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    case.override_sb(B, |build| build.status = BuildStatus::Created)
        .await;

    let report = case.drift().await;

    assert_eq!(
        report.unpromoted_startable, 1,
        "after 1: report.unpromoted_startable: {report:?}"
    );
    assert_eq!(
        report.total() - report.eval_counter_drift,
        1,
        "after 1: nothing else drifted besides the counters of E1: {report:?}"
    );
    let queued = case.status(B).await;
    assert_eq!(queued, BuildStatus::Queued, "after 1: sb(B).status");

    let report = case.drift().await;

    assert_eq!(report.total(), 0, "after 2: total: {report:?}");
    case.finish().await;
}

#[tokio::test]
async fn q09_ready_stamps_the_builds_and_their_closure_sizes_and_moves_no_status() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (statuses, gv) = (
        (case.status(A).await, case.status(B).await),
        case.gv(e1).await,
    );

    case.transition(Transition::Ready {
        shared_builds: case.sb_ids(&[A, B]).await,
        closure_sizes: vec![(case.db.id_of(B).await, 7)],
    })
    .await;

    for path in [A, B] {
        let stamped = case.sb(path).await.ready_at.is_some();
        assert!(stamped, "after 1: sb({path}).ready_at set");
    }
    let size = case.derivation(B).await.closure_size;
    assert_eq!(size, Some(7), "after 1: derivation(B).closure_size");
    let now = (case.status(A).await, case.status(B).await);
    assert_eq!(now, statuses, "after 1: sb(A), sb(B) status unchanged");
    assert_eq!(case.gv(e1).await, gv, "after 1: gv(E1) unchanged");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn q10_an_assignment_is_refused_on_a_build_that_left_the_queue() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    let (drv_of_b, _) = B.split_once('-').expect("B is a drv path");

    case.demote(Demotion::MissingNar {
        hash: drv_of_b.into(),
    })
    .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Created, true),
        "after 1: sb(B) is Created, wanted true"
    );
    let stored = case.find_cached_path(drv_of_b).await;
    assert!(stored.is_none(), "after 1: cached_path(B.drv) is gone");
    let counters = case.counters(e1).await;
    assert_eq!(counters, [2, 2, 0, 0, 0], "after 1: counters(E1)");
    case.zero_drift("step 1").await;

    let assignment = case.assigned(e1, B, false).await;

    assert_eq!(assignment, None, "after 2: refused");
    let assignments = case.db.rows::<dispatched_job::Entity>().await;
    assert!(
        assignments.is_empty(),
        "after 2: no dispatched_job row: {assignments:?}"
    );
    let created = case.status(B).await;
    assert_eq!(created, BuildStatus::Created, "after 2: sb(B).status");
    case.zero_drift("step 2").await;
    case.finish().await;
}
