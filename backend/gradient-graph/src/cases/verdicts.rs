/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::graph::repair::RepairScope;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::{Shape, shape};
use gradient_wire::types::BuildFailureKind;

use super::{
    A, A_OUT, A1, A1_OUT, A2, A2_OUT, B, B_OUT, BUILDER_EXIT, Case, F, F_OUT, S, T, T_OUT, X,
    X_OUT, Z, Z_OUT, chain, drv, nar, pass, with_attr, with_output, with_references,
};
use crate::messages::{NarCommit, OrphanCause, Transition};

async fn stream_completed(case: &Case, evaluation: EvaluationId) {
    case.transition(Transition::EvalStreamCompleted { evaluation })
        .await;
}

fn output_commit(hash: &str, package: &str) -> NarCommit {
    with_references(nar(hash, package), &[(hash, package)])
}

#[tokio::test]
async fn v01_stream_completion_moves_a_blocked_evaluation_to_building_and_queues_the_closure() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    case.override_sb(B, |build| {
        build.status = BuildStatus::Created;
        build.wanted = true;
    })
    .await;
    let version = case.gv(e1).await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.status(B).await,
        BuildStatus::Queued,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert!(case.gv(e1).await > version, "after 1: gv(E1) rose");
    case.zero_drift("step 1").await;
    case.finish().await;
}

fn finished_entry_point(evaluation: EvaluationId) -> Shape {
    shape()
        .evaluation(evaluation)
        .derivation(A)
        .walked()
        .entry_point("pkg.a")
        .job()
        .probed()
        .drv_cached()
        .output(A_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(A)
}

#[tokio::test]
async fn v02_stream_completion_with_nothing_blocking_goes_straight_to_the_verdict() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(finished_entry_point(e1)).await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "a after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 0, 0, 0],
        "a after 1: counters(E1)"
    );
    case.zero_drift("a step 1").await;
    case.finish().await;

    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        finished_entry_point(e1)
            .task_option(|task| task.retry_failed_builds = false)
            .derivation(F)
            .walked()
            .entry_point("pkg.f")
            .job()
            .probed()
            .drv_cached()
            .output(F_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(AttemptOutcome::Failed, BUILDER_EXIT),
    )
    .await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "b after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.status(F).await,
        BuildStatus::FailedPermanent,
        "b after 1: sb(F).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 1, 0, 0],
        "b after 1: counters(E1)"
    );
    case.zero_drift("b step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn v03_an_open_stream_gets_no_verdict_even_with_nothing_blocking() {
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
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None),
    )
    .await;

    case.commit_nar(output_commit(A_OUT, "a")).await;

    case.zero_drift("step 1").await;

    case.build_completed(A).await;

    let built = case.sb(A).await;
    assert_eq!(
        built.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert!(built.fetchable, "after 2: sb(A).fetchable");
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::EvaluatingDerivation,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 0, 0, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 3: evaluation(E1).status"
    );
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn v04_the_last_blocker_writes_the_verdict_and_a_named_aborted_build_turns_it_to_failed() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        pass(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(Z)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(Z_OUT)
            .build_status(BuildStatus::Aborted),
    )
    .await;
    case.override_job(e1, Z, |job| job.aborted = true).await;

    case.commit_nar(output_commit(A_OUT, "a")).await;
    case.build_started(A).await;
    case.build_completed(A).await;

    let passthrough = case.sb(A).await;
    assert_eq!(
        passthrough.status,
        BuildStatus::Completed,
        "after 1: sb(A).status, Completed without a substituted BuildOutput (X9)"
    );
    assert!(passthrough.fetchable, "after 1: sb(A).fetchable");
    assert_eq!(
        case.status(B).await,
        BuildStatus::Skipped,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 0, 1, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn v05_a_queued_build_blocks_an_evaluation_that_does_not_want_it() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        pass(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(B)
            .build_status(BuildStatus::Queued)
            .evaluation(e2)
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

    case.commit_nar(output_commit(A_OUT, "a")).await;
    case.build_started(A).await;
    case.build_completed(A).await;

    let passthrough = case.sb(A).await;
    assert_eq!(
        passthrough.status,
        BuildStatus::Completed,
        "after 1: sb(A).status, Completed without a substituted BuildOutput (X9)"
    );
    assert!(passthrough.fetchable, "after 1: sb(A).fetchable");
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    case.build_started(B).await;
    case.commit_nar(output_commit(B_OUT, "b")).await;
    case.build_completed(B).await;

    assert!(case.sb(B).await.fetchable, "after 2: sb(B).fetchable");
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.status(A2).await,
        BuildStatus::Queued,
        "after 2: sb(A2).status"
    );
    assert_eq!(
        case.evaluation_status(e2).await,
        EvaluationStatus::EvaluatingDerivation,
        "after 2: evaluation(E2).status"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn v06_stream_completion_rebuilds_an_older_permanent_failure_unless_retries_are_off() {
    for retries_failures in [true, false] {
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
                .build_status(BuildStatus::FailedPermanent)
                .attempt(AttemptOutcome::Failed, BUILDER_EXIT)
                .evaluation(e2)
                .task_option(|task| task.retry_failed_builds = retries_failures)
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

        stream_completed(&case, e2).await;

        let (failed, above) = (case.sb(B).await, case.sb(A2).await);
        let (verdict, counters) = (case.evaluation_status(e2).await, case.counters(e2).await);
        if retries_failures {
            assert_eq!(
                failed.status,
                BuildStatus::Queued,
                "a after 1: sb(B).status"
            );
            assert_eq!(failed.attempt, 0, "a after 1: sb(B).attempt");
            assert!(failed.probed, "a after 1: sb(B).probed");
            assert!(failed.wanted, "a after 1: sb(B).wanted");
            assert_eq!(
                above.status,
                BuildStatus::Created,
                "a after 1: sb(A2).status"
            );
            assert_eq!(
                verdict,
                EvaluationStatus::Building,
                "a after 1: evaluation(E2).status"
            );
            assert_eq!(
                case.status(A1).await,
                BuildStatus::DependencyFailed,
                "a after 1: sb(A1).status"
            );
            assert_eq!(
                case.evaluation_status(e1).await,
                EvaluationStatus::Failed,
                "a after 1: evaluation(E1).status"
            );
            assert_eq!(counters, [2, 2, 0, 1, 0], "a after 1: counters(E2)");
        } else {
            assert_eq!(
                failed.status,
                BuildStatus::FailedPermanent,
                "b after 1: sb(B).status"
            );
            assert_eq!(
                above.status,
                BuildStatus::DependencyFailed,
                "b after 1: sb(A2).status"
            );
            assert_eq!(
                verdict,
                EvaluationStatus::Failed,
                "b after 1: evaluation(E2).status"
            );
            assert_eq!(counters, [2, 0, 2, 0, 0], "b after 1: counters(E2)");
        }

        case.zero_drift("step 1").await;
        case.finish().await;
    }
}

#[tokio::test]
async fn v07_repair_completes_a_build_with_every_output_in_the_cache_but_no_stub() {
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
            .depends_on(S)
            .derivation(A)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .cached_output(A)
            .build_status(BuildStatus::Queued)
            .derivation(S)
            .job(),
    )
    .await;

    stream_completed(&case, e1).await;

    let (cached, stub, top) = (case.sb(A).await, case.sb(S).await, case.sb(X).await);
    assert_eq!(
        cached.status,
        BuildStatus::Completed,
        "after 1: sb(A).status"
    );
    assert!(cached.fetchable, "after 1: sb(A).fetchable");
    assert_eq!(stub.status, BuildStatus::Created, "after 1: sb(S).status");
    assert!(stub.wanted, "after 1: sb(S).wanted");
    assert!(!stub.fetchable, "after 1: sb(S).fetchable");
    assert_eq!(top.status, BuildStatus::Created, "after 1: sb(X).status");
    assert_eq!(top.blocking_deps, 1, "after 1: sb(X).blocking_deps");
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 2, 0, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

async fn probed_above_a_cut_off_subtree(e1: EvaluationId) -> Case {
    let case = Case::start(
        shape()
            .evaluation(e1)
            .derivation(A)
            .walked()
            .probed()
            .drv_cached()
            .output(A_OUT)
            .depends_on(B)
            .build_status(BuildStatus::Skipped)
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;

    let walk_of_x = vec![with_attr(with_output(drv(X, &[A]), X_OUT), "pkg.x")];
    case.walk(case.tasked(e1, walk_of_x).await).await;

    assert!(
        case.job(e1, A).await.is_some(),
        "after 1: build_job(E1,A) exists"
    );
    assert!(
        case.job(e1, B).await.is_none(),
        "after 1: no build_job(E1,B)"
    );
    assert_eq!(
        case.status(A).await,
        BuildStatus::Skipped,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.status(B).await,
        BuildStatus::Skipped,
        "after 1: sb(B).status"
    );
    case.zero_drift("step 1").await;

    assert_eq!(
        case.known(&[A]).await,
        vec![A.to_owned()],
        "after 2: result == [A]"
    );
    case.zero_drift("step 2").await;

    case.upstream_probed(&[X]).await;

    let (above, below) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(above.status, BuildStatus::Created, "after 3: sb(A).status");
    assert_eq!(above.attempt, 0, "after 3: sb(A).attempt");
    assert!(above.wanted, "after 3: sb(A).wanted");
    assert_eq!(below.status, BuildStatus::Queued, "after 3: sb(B).status");
    assert_eq!(below.attempt, 0, "after 3: sb(B).attempt");
    assert!(below.wanted, "after 3: sb(B).wanted");
    assert!(
        case.job(e1, B).await.is_some(),
        "after 3: build_job(E1,B) exists"
    );

    case
}

#[tokio::test]
async fn v08_an_open_build_below_a_cut_off_subtree_gets_its_job_when_it_gains_need() {
    let e1 = EvaluationId::now_v7();
    let case = probed_above_a_cut_off_subtree(e1).await;

    case.zero_drift("step 3").await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 4: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 3, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.zero_drift("step 4").await;
    case.finish().await;
}

async fn evaluation_failed(case: &Case, evaluation: EvaluationId, kind: BuildFailureKind) {
    case.transition(Transition::EvalFailed {
        evaluation,
        error: "boom".into(),
        kind,
        missing_paths: Vec::new(),
    })
    .await;
}

#[tokio::test]
async fn v09_a_canceled_or_transient_evaluation_failure_requeues_and_a_permanent_fails() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    evaluation_failed(&case, e1, BuildFailureKind::Canceled).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.status(B).await,
        BuildStatus::Queued,
        "after 1: sb(B).status"
    );
    case.zero_drift("step 1").await;

    case.set_evaluation_status(e1, EvaluationStatus::EvaluatingDerivation)
        .await;
    evaluation_failed(&case, e1, BuildFailureKind::Transient).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Queued,
        "after 2: evaluation(E1).status, attempts remain"
    );
    case.zero_drift("step 2").await;

    case.set_evaluation_status(e1, EvaluationStatus::EvaluatingDerivation)
        .await;
    evaluation_failed(&case, e1, BuildFailureKind::Permanent).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 3: evaluation(E1).status"
    );
    let errors = case.errors(e1).await;
    assert!(
        errors.contains(&"boom".to_owned()),
        "after 3: evaluation(E1) has message boom: {errors:?}"
    );
    let (top, dependency) = (case.sb(A).await, case.sb(B).await);
    assert_eq!(top.status, BuildStatus::Created, "after 3: sb(A).status");
    assert!(top.wanted, "after 3: sb(A).wanted");
    assert_eq!(
        dependency.status,
        BuildStatus::Queued,
        "after 3: sb(B).status"
    );
    assert!(dependency.wanted, "after 3: sb(B).wanted");
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn v10_a_wrong_evaluation_counter_is_recounted() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;
    case.sql(&format!(
        "UPDATE evaluation SET active_shared_builds = 7 WHERE id = '{e1}'"
    ))
    .await;

    let report = case.drift().await;

    assert_eq!(
        report.eval_counter_drift, 1,
        "after 1: report.eval_counter_drift"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(report.total(), 1, "after 1: total: {report:?}");
    case.finish().await;
}

#[tokio::test]
async fn v11_unstick_leaves_a_deterministic_builder_failure_alone_and_retry_thaws_everything() {
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
            .depends_on(T)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(B)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(B_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(AttemptOutcome::Failed, BUILDER_EXIT)
            .derivation(T)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(T_OUT)
            .build_status(BuildStatus::FailedTimeout)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::WallClockTimeout),
            ),
    )
    .await;

    case.transition(Transition::Repair {
        scope: RepairScope::Unstick(e1),
    })
    .await;

    let timed_out = case.sb(T).await;
    assert_eq!(
        timed_out.status,
        BuildStatus::Skipped,
        "after 1: sb(T).status, nothing wants T below the failed A (Q17)"
    );
    assert_eq!(timed_out.attempt, 0, "after 1: sb(T).attempt");
    assert!(timed_out.probed, "after 1: sb(T).probed");
    assert!(!timed_out.wanted, "after 1: sb(T).wanted");
    assert_eq!(
        case.status(B).await,
        BuildStatus::FailedPermanent,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.status(A).await,
        BuildStatus::DependencyFailed,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 0, 3, 0, 0],
        "after 1: counters(E1), frozen at the verdict (X7)"
    );
    case.zero_drift("step 1").await;

    case.transition(Transition::Repair {
        scope: RepairScope::Retry(e1),
    })
    .await;

    let (failed, top, timed_out) = (case.sb(B).await, case.sb(A).await, case.sb(T).await);
    assert_eq!(failed.status, BuildStatus::Skipped, "after 2: sb(B).status");
    assert_eq!(failed.attempt, 0, "after 2: sb(B).attempt");
    assert!(!failed.probed, "after 2: sb(B).probed");
    assert!(!failed.wanted, "after 2: sb(B).wanted");
    assert_eq!(top.status, BuildStatus::Created, "after 2: sb(A).status");
    assert_eq!(top.attempt, 0, "after 2: sb(A).attempt");
    assert!(!top.probed, "after 2: sb(A).probed");
    assert!(top.wanted, "after 2: sb(A).wanted");
    assert_eq!(
        timed_out.status,
        BuildStatus::Skipped,
        "after 2: sb(T).status"
    );
    assert!(timed_out.probed, "after 2: sb(T).probed");
    assert!(!timed_out.wanted, "after 2: sb(T).wanted");
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status, a repair reopens nothing (Q17)"
    );
    assert_eq!(
        case.counters(e1).await,
        [3, 0, 3, 0, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn v12_stream_completion_leaves_a_user_aborted_build_and_everything_above_it_failed() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain(e1)
            .derivation(B)
            .build_status(BuildStatus::Aborted)
            .derivation(A)
            .build_status(BuildStatus::DependencyFailed),
    )
    .await;
    case.override_job(e1, B, |job| job.aborted = true).await;

    stream_completed(&case, e1).await;

    let aborted = case.sb(B).await;
    assert_eq!(
        aborted.status,
        BuildStatus::Aborted,
        "after 1: sb(B).status"
    );
    assert!(!aborted.wanted, "after 1: sb(B).wanted");
    assert_eq!(
        case.status(A).await,
        BuildStatus::DependencyFailed,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1).status"
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
async fn v13_a_replay_of_the_two_resent_transitions_is_harmless() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    stream_completed(&case, e1).await;

    let rows = case.rows().await;
    case.zero_drift("step 1").await;

    stream_completed(&case, e1).await;

    assert_eq!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(case.rows().await, rows, "after 2: rows equal to after 1");
    case.zero_drift("step 2").await;

    let assignment = case.assigned(e1, B, false).await;
    assert!(assignment.is_some(), "step 3: a worker takes B (X9)");
    case.build_started(B).await;
    case.builds_lost(&[B], OrphanCause::WorkerLost).await;
    case.builds_lost(&[B], OrphanCause::WorkerLost).await;

    let attempts = case.attempts(B).await;
    assert_eq!(attempts.len(), 1, "after 3: a single attempt of B");
    assert_eq!(
        attempts[0].outcome,
        AttemptOutcome::Aborted,
        "after 3: the attempt of B"
    );
    assert_eq!(
        case.status(B).await,
        BuildStatus::Queued,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 3: counters(E1)"
    );
    case.zero_drift("step 3").await;
    case.finish().await;
}
