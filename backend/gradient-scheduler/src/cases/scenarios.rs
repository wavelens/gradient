/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, CacheUpstreamId, DispatchedJobId, EvaluationId};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_fixtures::shape::shape;
use gradient_graph::NarCommit;
use gradient_pool::session_port::SessionSignal;
use gradient_types::WaitingReason;
use gradient_wire::types::{BuildFailureKind, BuildMetrics, BuildSpecKind, Job, JobKind};

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, CaseShapes, SchedulerCase, UpstreamServer, attr_of, build_output,
    drv, drv_nar, eval_key, failure, output_nar, w_all, with_attr, with_output, with_references,
};
use crate::jobs::Assignment;

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";

#[tokio::test]
async fn z01_an_evaluation_runs_from_the_queue_to_completed_with_a_leaf_built_mid_walk() {
    let (e1, c1) = (EvaluationId::now_v7(), CacheId::now_v7());
    let u1 = UpstreamServer::start().await;
    u1.miss(A_OUT).await;
    u1.miss(B_OUT).await;
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .into_cache(c1)
            .keyed(SchedulerCase::signing_key())
            .upstream(
                CacheUpstreamId::now_v7(),
                &u1.url(),
                &u1.public_key,
                CacheUpstreamKind::Http,
                CacheSubscriptionMode::ReadWrite,
                true,
            ),
    )
    .await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let eval = eval_key(e1);

    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let flake = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 1: W1 gets eval:E1");
    assert_eq!(flake.job_id(), eval, "after 1: result.job_id");
    let a1 = flake.assignment_id();
    case.check("step 1").await;

    case.advance_evaluation(W1, &eval, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 2").await;

    case.report_eval_result(W1, &eval, vec![with_output(drv(B, &[]), B_OUT)])
        .await;
    case.commit_nar(drv_nar(B)).await;
    case.probe_round(&[B]).await;
    case.admit_startable_moves().await;
    let b = case.key(B).await;
    let sb_b = case.sb(B).await;
    assert_eq!(
        (sb_b.status, sb_b.probed, sb_b.wanted),
        (BuildStatus::Skipped, false, false),
        "after 3: sb(B)"
    );
    assert!(!case.pending(&b).await, "after 3: pending(build:B)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Offers(_)]),
        "after 3: signals(W1) == [Offers(_)], got {signals:?}"
    );
    case.check("step 3").await;

    case.report_eval_result(
        W1,
        &eval,
        vec![with_attr(with_output(drv(A, &[B]), A_OUT), &attr_of(A))],
    )
    .await;
    case.commit_nar(drv_nar(A)).await;
    case.probe_round(&[A]).await;
    let sb_a = case.sb(A).await;
    assert_eq!(
        (sb_a.status, sb_a.probed, sb_a.blocking_deps),
        (BuildStatus::Created, true, 1),
        "after 4: sb(A)"
    );
    let sb_b = case.sb(B).await;
    assert_eq!(
        (sb_b.status, sb_b.wanted),
        (BuildStatus::Queued, true),
        "after 4: sb(B)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    case.admit_startable_moves().await;
    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let a2 = requested_build(&case, W1, &b, "after 5").await;
    case.report_building(W1, &b).await;
    assert!(!case.pending(&b).await, "after 5: pending(build:B)");
    assert_eq!(
        case.active(&b).await.as_deref(),
        Some(W1),
        "after 5: active(build:B)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Building,
        "after 5: sb(B).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 5: evaluation(E1).status"
    );
    case.check("step 5").await;

    case.complete(W1, &eval, a1, Vec::new(), 3000).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 6: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 1],
        "after 6: counters(E1)"
    );
    case.check("step 6").await;

    case.report_build_output(
        W1,
        &b,
        vec![build_output(B, B_OUT)],
        false,
        Some(BuildMetrics {
            peak_ram_mb: Some(100),
            ..Default::default()
        }),
    )
    .await;
    case.commit_nar(NarCommit {
        targets: case.project_caches(e1).await,
        ..output_nar(B, B_OUT)
    })
    .await;
    case.complete(W1, &b, a2, Vec::new(), 500).await;
    let sb_b = case.sb(B).await;
    assert_eq!(
        (sb_b.status, sb_b.fetchable),
        (BuildStatus::Completed, true),
        "after 7: sb(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 7: sb(A).status"
    );
    assert_eq!(
        case.derivation_metrics(B).await.len(),
        1,
        "after 7: derivation_metric(B) rows"
    );
    let signatures = case.db.signatures(B_OUT).await;
    assert!(
        signatures
            .iter()
            .any(|row| row.cache == c1 && row.signature.is_some()),
        "after 7: cached_path_signature(B_OUT, C1) signed, got {signatures:?}"
    );
    case.check("step 7").await;

    case.admit_startable_moves().await;
    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let a = case.key(A).await;
    let a3 = requested_build(&case, W1, &a, "after 8").await;
    case.report_building(W1, &a).await;
    case.report_build_output(W1, &a, vec![build_output(A, A_OUT)], false, None)
        .await;
    case.commit_nar(NarCommit {
        targets: case.project_caches(e1).await,
        ..with_references(output_nar(A, A_OUT), &[(B_OUT, "b")])
    })
    .await;
    case.complete(W1, &a, a3, Vec::new(), 700).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 8: sb(A).status"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.finished_at.is_some()),
        (EvaluationStatus::Completed, true),
        "after 8: evaluation(E1) == {{Completed, finished_at set}}"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 0, 0, 0],
        "after 8: counters(E1)"
    );
    for (key, id) in [(&eval, a1), (&b, a2), (&a, a3)] {
        assert_eq!(
            case.assignment_outcomes(key).await,
            vec![(id, Some(DispatchedJobOutcome::Completed))],
            "after 8: dj({key})"
        );
    }
    for path in [B, A] {
        assert_eq!(
            case.attempt_outcomes_with_reasons(path).await,
            vec![(AttemptOutcome::Built, None)],
            "after 8: att({path})"
        );
    }
    case.check("step 8").await;

    case.finish().await;
}

#[tokio::test]
async fn z02_two_evaluations_share_a_leaf_and_a_retry_reopens_the_aborted_one() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .chain(e1)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(C)
            .walked()
            .probed()
            .drv_cached()
            .entry_point(&attr_of(C))
            .job()
            .output(C_OUT)
            .depends_on(B)
            .derivation(B)
            .job(),
    )
    .await;
    case.connect(W1, w_all().slots(2), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = requested_build(&case, W1, &b, "after 1").await;
    case.report_building(W1, &b).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Building,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e2).await,
        [2, 2, 0, 0, 1],
        "after 2: counters(E2)"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.report_build_output(W1, &b, vec![build_output(B, B_OUT)], false, None)
        .await;
    case.commit_nar(NarCommit {
        targets: case.project_caches(e2).await,
        ..output_nar(B, B_OUT)
    })
    .await;
    case.complete(W1, &b, a1, Vec::new(), 10).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.sb(C).await.status,
        BuildStatus::Queued,
        "after 3: sb(C).status"
    );
    case.check("step 3").await;

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let c = case.key(C).await;
    let a2 = requested_build(&case, W2, &c, "after 4").await;
    let commit = NarCommit {
        targets: case.project_caches(e2).await,
        ..with_references(output_nar(C, C_OUT), &[(B_OUT, "b")])
    };
    build_and_complete(&case, W2, &c, a2, commit, 10).await;
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 4: evaluation(E2).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 4: evaluation(E1).status"
    );
    case.check("step 4").await;

    let retried = case.retry_build(e1, A).await;
    assert_eq!(retried, Ok(()), "after 5: retry_build(E1, A)");
    let sb_a = case.sb(A).await;
    assert_eq!(
        (sb_a.status, sb_a.probed, sb_a.wanted, sb_a.attempt),
        (BuildStatus::Created, false, true, 0),
        "after 5: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 5: evaluation(E1).status"
    );
    let job = case.job(e1, A).await.expect("after 5: build_job(E1, A)");
    assert!(!job.aborted, "after 5: build_job(E1, A).aborted");
    case.check("step 5").await;

    case.probe_round(&[A]).await;
    let sb_a = case.sb(A).await;
    assert_eq!(
        (sb_a.status, sb_a.probed),
        (BuildStatus::Queued, true),
        "after 5b: sb(A)"
    );
    case.check("step 5b").await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a3 = requested_build(&case, W1, &a, "after 6").await;
    let commit = NarCommit {
        targets: case.project_caches(e1).await,
        ..with_references(output_nar(A, A_OUT), &[(B_OUT, "b")])
    };
    build_and_complete(&case, W1, &a, a3, commit, 10).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 6: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 6: evaluation(E1).status"
    );
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn z03_a_lost_worker_and_a_transient_failure_both_end_in_completed_builds() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with(shape().two(e1), |c| c.build.retry_backoff_secs = 1).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.offers_full(W2).await;
    case.scores(
        W1,
        &[(a.as_str(), 0, 0, false), (b.as_str(), 5, 5_000_000, false)],
    )
    .await;
    case.score_all(W2).await;
    let a1 = requested_build(&case, W1, &a, "after 1").await;
    let a2 = requested_build(&case, W2, &b, "after 1").await;
    case.report_building(W1, &a).await;
    case.report_building(W2, &b).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;

    case.disconnect(W1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 2: att(A)"
    );
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 2: dj(build:A)"
    );
    assert_eq!(case.signals(W2), vec![], "after 2: signals(W2)");
    case.check("step 2").await;

    case.fail(
        W2,
        &b,
        a2,
        failure(BuildFailureKind::Transient).elapsed_ms(1),
    )
    .await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTransient,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes(B).await,
        vec![AttemptOutcome::Failed],
        "after 3: att(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 3: counters(E1)"
    );
    case.check("step 3").await;

    case.connect(W3, w_all().slots(2), &[]).await;
    case.offers_full(W3).await;
    case.score_all(W3).await;
    let a3 = requested_build(&case, W3, &a, "after 4").await;
    build_and_complete(&case, W3, &a, a3, output_nar(A, A_OUT), 1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 4: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 0, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    tokio::time::sleep(std::time::Duration::from_secs_f64(1.2)).await;
    case.requeue_transient_retries().await;
    case.admit_startable_moves().await;
    case.offers_full(W3).await;
    case.score_all(W3).await;
    let a4 = requested_build(&case, W3, &b, "after 5").await;
    build_and_complete(&case, W3, &b, a4, output_nar(B, B_OUT), 1).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 5: sb(B).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 5: evaluation(E1).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![
            (
                AttemptOutcome::Aborted,
                Some(AttemptFailureReason::WorkerLost)
            ),
            (AttemptOutcome::Built, None),
        ],
        "after 5: att(A)"
    );
    assert_eq!(
        case.attempt_outcomes(B).await,
        vec![AttemptOutcome::Failed, AttemptOutcome::Built],
        "after 5: att(B)"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn z04_a_passthrough_missing_twice_becomes_a_real_build_after_its_input() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .passthrough(e1)
            .drv_cached()
            .depends_on(B)
            .derivation(B)
            .walked()
            .probed()
            .drv_cached()
            .job()
            .output(B_OUT)
            .build_status(BuildStatus::Skipped),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    for (step, line) in [("step 1", "after 1"), ("step 2", "after 2")] {
        case.admit_startable_moves().await;
        case.offers_full(W1).await;
        case.score_all(W1).await;
        let assignment = case
            .request_job(W1, JobKind::Build)
            .await
            .unwrap_or_else(|| panic!("{line}: W1 gets build:A"));
        assert_eq!(assignment.job_id(), a, "{line}: result.job_id");
        assert_eq!(
            spec_kind(&assignment),
            BuildSpecKind::Substitute,
            "{line}: result.job.kind"
        );
        case.report_building(W1, &a).await;
        case.fail(
            W1,
            &a,
            assignment.assignment_id(),
            failure(BuildFailureKind::SubstituteUnavailable).elapsed_ms(1),
        )
        .await;
        case.check(step).await;
    }
    let sb_a = case.sb(A).await;
    assert_eq!(
        (
            sb_a.status,
            sb_a.cache_available,
            sb_a.attempt,
            sb_a.blocking_deps
        ),
        (BuildStatus::Created, false, 0, 1),
        "after 2: sb(A)"
    );
    let sb_b = case.sb(B).await;
    assert_eq!(
        (sb_b.status, sb_b.wanted),
        (BuildStatus::Queued, true),
        "after 2: sb(B)"
    );

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let b = case.key(B).await;
    let a3 = requested_build(&case, W1, &b, "after 3").await;
    build_and_complete(&case, W1, &b, a3, output_nar(B, B_OUT), 1).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    case.check("step 3").await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 4: W1 gets build:A");
    assert_eq!(assignment.job_id(), a, "after 4: result.job_id");
    assert_eq!(
        spec_kind(&assignment),
        BuildSpecKind::Build,
        "after 4: result.job.kind"
    );
    let commit = with_references(output_nar(A, A_OUT), &[(B_OUT, "b")]);
    build_and_complete(&case, W1, &a, assignment.assignment_id(), commit, 1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 4: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 4: evaluation(E1).status"
    );
    let unavailable = (
        AttemptOutcome::Failed,
        Some(AttemptFailureReason::SubstituteUnavailable),
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![unavailable, unavailable, (AttemptOutcome::Built, None)],
        "after 4: att(A)"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn z05_a_core_respawn_mid_run_loses_no_job() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().two(e1).evalq(e2)).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let (a, b, eval) = (case.key(A).await, case.key(B).await, eval_key(e2));

    case.admit_startable_moves().await;
    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.scores(
        W1,
        &[
            (a.as_str(), 0, 0, false),
            (b.as_str(), 5, 5_000_000, false),
            (eval.as_str(), 0, 0, false),
        ],
    )
    .await;
    let a1 = requested_build(&case, W1, &a, "after 1").await;
    case.report_building(W1, &a).await;
    let flake = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 1: W1 gets eval:E2");
    assert_eq!(flake.job_id(), eval, "after 1: result.job_id");
    let a2 = flake.assignment_id();
    case.report_status(W1, &eval, EvaluationStatus::Fetching)
        .await;
    case.signals(W1);
    let rows_before = (case.assignments(&a).await, case.assignments(&eval).await);
    case.check("step 1").await;

    case.respawn_core().await;
    case.check("step 2").await;

    case.reattach(W1, &[(&a, a1), (&eval, a2)]).await;
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 3: active(build:A)"
    );
    assert_eq!(
        case.active(&eval).await.as_deref(),
        Some(W1),
        "after 3: active(eval:E2)"
    );
    let rows_after = (case.assignments(&a).await, case.assignments(&eval).await);
    assert_eq!(
        rows_after, rows_before,
        "after 3: dj rows a1, a2 open and unchanged"
    );
    assert!(
        rows_after
            .0
            .iter()
            .chain(&rows_after.1)
            .all(|row| row.finished_at.is_none()),
        "after 3: dj rows a1, a2 open"
    );
    case.signals(W1);
    case.check("step 3").await;

    case.build_assign_pass(true, true).await;
    case.assign_queued_evals().await;
    assert!(case.pending(&b).await, "after 4: pending(build:B)");
    assert!(!case.pending(&eval).await, "after 4: pending(eval:E2)");
    let signals = case.signals(W1);
    assert!(
        signals
            .iter()
            .any(|s| matches!(s, SessionSignal::Offers(_)))
            && !signals
                .iter()
                .any(|s| matches!(s, SessionSignal::Abort { .. } | SessionSignal::Close { .. })),
        "after 4: signals(W1) contain Offers, no Abort, no Close, got {signals:?}"
    );
    case.check("step 4").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 1).await;
    case.check("step 5").await;

    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let a3 = requested_build(&case, W1, &b, "after 6").await;
    build_and_complete(&case, W1, &b, a3, output_nar(B, B_OUT), 1).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 6: evaluation(E1).status"
    );
    case.check("step 6").await;

    case.advance_evaluation(W1, &eval, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.complete(W1, &eval, a2, Vec::new(), 1).await;
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 7: evaluation(E2).status"
    );
    case.check("step 7").await;

    case.finish().await;
}

#[tokio::test]
async fn z06_two_instances_on_a_database_share_the_jobs_without_a_double_claim() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().two(e1).evalq(e2)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_all().slots(2), &[]).await;
    second.connect(W2, w_all().slots(2), &[]).await;
    let (a, b, eval) = (case.key(A).await, case.key(B).await, eval_key(e2));
    let builds: BTreeSet<String> = [a.clone(), b.clone()].into();

    case.admit_startable_moves().await;
    second.resync_startable_set().await;
    case.assign_queued_evals().await;
    second.assign_queued_evals().await;
    case.offers_full(W1).await;
    second.offers_full(W2).await;
    case.score_all(W1).await;
    second.score_all(W2).await;
    case.check("step 1").await;
    second.check("step 1").await;

    let mut first_builds = Vec::new();
    for _ in 0..2 {
        first_builds.push(case.request_job(W1, JobKind::Build).await);
    }
    let mut second_builds = Vec::new();
    for _ in 0..2 {
        second_builds.push(second.request_job(W2, JobKind::Build).await);
    }
    let first_flake = case.request_job(W1, JobKind::Flake).await;
    let second_flake = second.request_job(W2, JobKind::Flake).await;
    let taken: BTreeSet<String> = first_builds
        .iter()
        .flatten()
        .map(|j| j.job_id().to_owned())
        .collect();
    assert_eq!(taken, builds, "after 2: W1 gets build:A and build:B");
    assert!(
        second_builds.iter().all(Option::is_none),
        "after 2: the build requests of W2 get nothing"
    );
    assert_eq!(
        first_flake.as_ref().map(|j| j.job_id().to_owned()),
        Some(eval.clone()),
        "after 2: W1 gets eval:E2"
    );
    assert!(
        second_flake.is_none(),
        "after 2: the eval request of W2 gets nothing"
    );
    let mut open: Vec<String> = Vec::new();
    for worker in [W1, W2] {
        open.extend(
            case.assignments_of(worker)
                .await
                .into_iter()
                .filter(|row| row.finished_at.is_none())
                .filter_map(|row| row.job_id),
        );
    }
    open.sort();
    let mut expected = vec![a.clone(), b.clone(), eval.clone()];
    expected.sort();
    assert_eq!(
        open, expected,
        "after 2: 3 open rows, build:A, build:B, eval:E2"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    case.report_building(W1, &a).await;
    case.report_building(W1, &b).await;
    let first_rows: Vec<(String, DispatchedJobId)> = first_builds
        .iter()
        .chain([&first_flake])
        .flatten()
        .map(|j| (j.job_id().to_owned(), j.assignment_id()))
        .collect();
    case.disconnect(W1).await;
    for (key, id) in &first_rows {
        assert_eq!(
            case.assignment_outcomes(key).await,
            vec![(*id, Some(DispatchedJobOutcome::Abandoned))],
            "after 3: dj({key})"
        );
    }
    for path in [A, B] {
        assert_eq!(
            case.sb(path).await.status,
            BuildStatus::Queued,
            "after 3: sb({path}).status"
        );
        assert_eq!(
            case.attempt_outcomes_with_reasons(path).await,
            vec![(
                AttemptOutcome::Aborted,
                Some(AttemptFailureReason::WorkerLost)
            )],
            "after 3: att({path})"
        );
    }
    let status = case.evaluation(e2).await.status;
    assert!(
        matches!(status, EvaluationStatus::Waiting | EvaluationStatus::Queued),
        "after 3: evaluation(E2).status in {{Waiting, Queued}}, got {status:?}"
    );
    case.check("step 3").await;
    second.check("step 3").await;

    second.resync_startable_set().await;
    second.assign_queued_evals().await;
    second.offers_full(W2).await;
    second.score_all(W2).await;
    let mut taken_again = Vec::new();
    for _ in 0..2 {
        taken_again.push(
            second
                .request_job(W2, JobKind::Build)
                .await
                .expect("after 4: W2 gets a build W1 lost"),
        );
    }
    let flake = second
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 4: W2 gets eval:E2");
    let keys: BTreeSet<String> = taken_again.iter().map(|j| j.job_id().to_owned()).collect();
    assert_eq!(keys, builds, "after 4: W2 runs build:A and build:B");
    assert_eq!(flake.job_id(), eval, "after 4: W2 runs eval:E2");
    for key in [&a, &b, &eval] {
        assert!(case.pending(key).await, "after 4: I1.pending({key})");
    }
    case.check("step 4").await;
    second.check("step 4").await;

    for job in &taken_again {
        let key = job.job_id().to_owned();
        let (path, hash) = if key == a { (A, A_OUT) } else { (B, B_OUT) };
        build_and_complete(
            &second,
            W2,
            &key,
            job.assignment_id(),
            output_nar(path, hash),
            1,
        )
        .await;
    }
    second
        .advance_evaluation(W2, &eval, EvaluationStatus::EvaluatingDerivation)
        .await;
    second
        .complete(W2, &eval, flake.assignment_id(), Vec::new(), 1)
        .await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 5: evaluation(E1).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 5: evaluation(E2).status"
    );
    case.check("step 5").await;
    second.check("step 5").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn z07_a_server_drain_mid_run_pauses_admission_and_the_run_resumes() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = requested_build(&case, W1, &b, "after 1").await;
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    case.drain_server(true).await;
    case.build_assign_pass(true, true).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (
            EvaluationStatus::Waiting,
            Some(WaitingReason::Draining.to_json())
        ),
        "after 2: evaluation(E1) == {{Waiting, {{draining}}}}"
    );
    assert_eq!(
        case.active(&b).await.as_deref(),
        Some(W1),
        "after 2: active(build:B)"
    );
    case.check("step 2").await;

    case.commit_nar(output_nar(B, B_OUT)).await;
    case.complete(W1, &b, a1, Vec::new(), 1).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    case.signals(W1);
    case.check("step 3").await;

    case.build_assign_pass(true, true).await;
    assert!(!case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.drain_server(false).await;
    case.build_assign_pass(true, true).await;
    case.refresh_waiting_state().await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 5: evaluation(E1).status"
    );
    assert!(case.pending(&a).await, "after 5: pending(build:A)");
    let signals = case.signals(W1);
    assert!(
        matches!(
            signals.as_slice(),
            [SessionSignal::Offers(_), SessionSignal::Offers(_)]
        ),
        "after 5: signals(W1) == [Offers(_), Offers(_)], got {signals:?}"
    );
    case.check("step 5").await;

    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a2 = requested_build(&case, W1, &a, "after 6").await;
    let commit = with_references(output_nar(A, A_OUT), &[(B_OUT, "b")]);
    build_and_complete(&case, W1, &a, a2, commit, 1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 6: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 6: evaluation(E1).status"
    );
    case.check("step 6").await;

    case.finish().await;
}

async fn requested_build(
    case: &SchedulerCase,
    worker: &str,
    key: &str,
    line: &str,
) -> DispatchedJobId {
    let assignment = case
        .request_job(worker, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("{line}: {worker} gets {key}"));
    assert_eq!(assignment.job_id(), key, "{line}: result.job_id");
    assignment.assignment_id()
}

async fn build_and_complete(
    case: &SchedulerCase,
    worker: &str,
    key: &str,
    assignment: DispatchedJobId,
    commit: NarCommit,
    elapsed_ms: u64,
) {
    case.report_building(worker, key).await;
    case.commit_nar(commit).await;
    case.complete(worker, key, assignment, Vec::new(), elapsed_ms)
        .await;
}

fn spec_kind(assignment: &Assignment) -> BuildSpecKind {
    let Job::Build(build) = &assignment.job else {
        panic!("a build job, got {:?}", assignment.job);
    };
    build.build.kind
}
