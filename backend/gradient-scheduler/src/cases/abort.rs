/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use gradient_db::status::BuildRefusal;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::cache_upstream::CacheUpstreamKind;
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{CacheId, CacheUpstreamId, EvaluationId};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_fixtures::shape::shape;
use gradient_graph::NarCommit;
use gradient_pool::session_port::SessionSignal;
use gradient_types::waiting_reason::{UnmetRequirement, WaitingReason};
use gradient_wire::types::{BuildFailureKind, BuildMetrics, BuildSpecKind, Job, JobKind};

use super::{
    A, A_OUT, ARM, B, B_OUT, C, C_OUT, CaseShapes, SchedulerCase, UpstreamServer, X86, ago,
    attr_of, build_output, eval_key, failure, is_recent, output_nar, planned_targets, w_all,
};

const W1: &str = "w1";
const W2: &str = "w2";

#[tokio::test]
async fn a01_aborting_an_evaluation_with_a_running_eval_job_keeps_it_active_until_confirmed() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let a1 = case.evalrun(W1, e1).await;
    let key = eval_key(e1);
    case.report_status(W1, &key, EvaluationStatus::Fetching)
        .await;
    case.signals(W1);
    case.check("step 0").await;

    case.abort_evaluation(e1).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (
            evaluation.status,
            evaluation.waiting_reason,
            evaluation.prioritized
        ),
        (EvaluationStatus::Aborted, None, false),
        "after 1: evaluation(E1)"
    );
    assert!(
        evaluation.finished_at.is_some(),
        "after 1: evaluation(E1).finished_at set"
    );
    assert_eq!(
        case.active(&key).await.as_deref(),
        Some(W1),
        "after 1: active(eval:E1)"
    );
    assert!(
        case.open_assignment(&key).await.is_some(),
        "after 1: open(eval:E1) exists"
    );
    assert!(!case.pending(&key).await, "after 1: pending(eval:E1)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Abort {
            job_id: key.clone(),
            reason: "evaluation aborted".into(),
        }],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    let next = case.request_job(W1, JobKind::Flake).await;
    assert!(next.is_none(), "after 2: result == nothing");
    case.check("step 2").await;

    case.fail(
        W1,
        &key,
        a1,
        failure(BuildFailureKind::Aborted)
            .error("aborted by server")
            .elapsed_ms(20),
    )
    .await;
    let rows: Vec<_> = case
        .assignments(&key)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome, r.worker_elapsed_ms))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned), Some(20))],
        "after 3: dj(eval:E1)"
    );
    assert_eq!(case.active(&key).await, None, "after 3: active(eval:E1)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E1).status"
    );
    case.check("step 3").await;

    let before = (case.evaluation(e1).await, case.assignments(&key).await);
    case.abort_evaluation(e1).await;
    let after = (case.evaluation(e1).await, case.assignments(&key).await);
    assert_eq!(after, before, "after 4: nothing changed");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a01b_a_worker_cancel_of_a_build_requeues_it_without_failure() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 1: sb(A).status"
    );
    case.signals(W1);
    case.check("step 1").await;

    case.fail(
        W1,
        &a,
        a1,
        failure(BuildFailureKind::Canceled).error("the worker stopped before the job finished"),
    )
    .await;
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.reason))
        .collect();
    assert_eq!(
        attempts,
        vec![(AttemptOutcome::Aborted, None)],
        "after 2: att(A) without a failed attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 2: counters(E1)"
    );
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a01c_a_worker_cancel_of_an_eval_job_requeues_it_without_failure() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let a1 = case.evalrun(W1, e1).await;
    let key = eval_key(e1);
    case.report_status(W1, &key, EvaluationStatus::Fetching)
        .await;
    case.signals(W1);
    case.check("step 0").await;

    case.fail(
        W1,
        &key,
        a1,
        failure(BuildFailureKind::Canceled).error("the worker stopped before the job finished"),
    )
    .await;
    let rows: Vec<_> = case
        .assignments(&key)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 1: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert!(
        !evaluation.status.is_terminal(),
        "after 1: evaluation(E1) is not finished, got {:?}",
        evaluation.status
    );
    assert_eq!(
        evaluation.finished_at, None,
        "after 1: evaluation(E1).finished_at"
    );
    assert_eq!(case.active(&key).await, None, "after 1: active(eval:E1)");
    case.check("step 1").await;

    case.assign_queued_evals().await;
    assert!(case.pending(&key).await, "after 2: pending(eval:E1)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn a02_aborting_an_evaluation_stops_its_unshared_running_build() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    case.connect(W1, w_all(), &[]).await;
    let b = case.key(B).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:B");
    assert_eq!(assignment.job_id(), b, "after 1: result.job_id");
    let a1 = assignment.assignment_id();
    case.report_building(W1, &b).await;
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.active(&b).await.as_deref(),
        Some(W1),
        "after 2: active(build:B)"
    );
    assert!(
        case.open_assignment(&b).await.is_some(),
        "after 2: open(build:B) exists"
    );
    assert_eq!(
        case.attempts(B).await.first().map(|t| t.outcome),
        Some(AttemptOutcome::Aborted),
        "after 2: att(B)[0].outcome"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == b),
        "after 2: signals(W1) == [Abort{{build:B}}], got {signals:?}"
    );
    case.check("step 2").await;

    case.fail(
        W1,
        &b,
        a1,
        failure(BuildFailureKind::Aborted)
            .error("aborted by server")
            .elapsed_ms(7),
    )
    .await;
    let rows: Vec<_> = case
        .assignments(&b)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 3: dj(build:B)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 3: sb(B).status"
    );
    assert_eq!(
        case.attempts(B).await.len(),
        1,
        "after 3: no new attempt of B"
    );
    assert_eq!(case.active(&b).await, None, "after 3: active(build:B)");
    assert!(!case.pending(&b).await, "after 3: pending(build:B)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a03_a_build_wanted_by_a_second_live_evaluation_is_not_stopped() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().shared(e1, e2)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    let driver = case
        .open_assignment(&a)
        .await
        .expect("after 1: open(build:A)")
        .evaluation_id;
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E2).status"
    );
    let build = case.sb(A).await;
    assert_eq!(
        (build.status, build.wanted),
        (BuildStatus::Building, true),
        "after 2: sb(A) {{status, wanted}}"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 2: active(build:A)"
    );
    assert_eq!(
        case.counters(e2).await,
        [1, 1, 0, 0, 1],
        "after 2: counters(E2)"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.commit_nar(NarCommit {
        targets: case.project_caches(e2).await,
        ..output_nar(A, A_OUT)
    })
    .await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 3: evaluation(E2).status"
    );
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome, r.evaluation_id))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Completed), driver)],
        "after 3: dj(build:A)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a04_aborting_the_last_live_wanting_evaluation_stops_the_build() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().shared(e1, e2)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    let a1 = assignment.assignment_id();
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.abort_evaluation(e2).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E2).status"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 3: active(build:A)"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 3: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 3").await;

    case.fail(W1, &a, a1, failure(BuildFailureKind::Aborted).elapsed_ms(1))
        .await;
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 4: dj(build:A)"
    );
    assert_eq!(case.active(&a).await, None, "after 4: active(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 4: sb(A).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a05_a_pending_build_another_live_evaluation_wants_stays_pending() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().shared(e1, e2)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&a).await, "after 1: pending(build:A)");
    assert!(
        case.open_assignment(&a).await.is_none(),
        "after 1: open(build:A) == none"
    );
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let handed = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        handed.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 3: result.job_id"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a06_aborting_an_evaluation_drops_only_its_own_pending_jobs() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).one(e2, B, B_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let (k, b) = (eval_key(e1), case.key(B).await);

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    assert!(case.pending(&k).await, "after 1: pending(eval:E1)");
    assert!(case.pending(&b).await, "after 1: pending(build:B)");
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert!(!case.pending(&k).await, "after 2: pending(eval:E1)");
    assert!(case.pending(&b).await, "after 2: pending(build:B)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(case.assignments_of(W1).await, vec![], "after 2: dj == []");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let offer = case.offers_full(W1).await;
    let candidates: Vec<_> = offer.candidates.iter().map(|c| c.job_id.clone()).collect();
    assert_eq!(candidates, vec![b.clone()], "after 3: result.candidates");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a07_an_unconfirmed_abort_is_reaped_after_the_grace() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().two(e1)).await;
    case.connect(W1, w_all().slots(1), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    assert_eq!(assignment.job_id(), a, "after 1: result.job_id");
    let a1 = assignment.assignment_id();
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    assert_eq!(case.abort_build(e1, A).await, Ok(()), "after 2: result");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 2: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 2").await;

    case.reap_overdue_aborts(Duration::ZERO).await;
    assert_eq!(case.active(&a).await, None, "after 3: active(build:A)");
    let rows = case.assignments(&a).await;
    assert!(
        matches!(
            rows.as_slice(),
            [row] if row.id == a1
                && row.outcome == Some(DispatchedJobOutcome::Abandoned)
                && row.finished_at.is_some_and(is_recent)
        ),
        "after 3: dj(build:A) == [{{a1, Abandoned, finished_at ~now}}], got {rows:?}"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_ne!(
        case.attempts(A).await.first().map(|t| t.outcome),
        Some(AttemptOutcome::Running),
        "after 3: att(A)[0].outcome"
    );
    case.check("step 3").await;

    let handed = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        handed.as_ref().map(|j| j.job_id().to_owned()),
        Some(b.clone()),
        "after 4: result.job_id"
    );
    case.signals(W1);
    case.check("step 4").await;

    let before = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
        case.sb(B).await,
        case.evaluation(e1).await,
    );
    case.fail(W1, &a, a1, failure(BuildFailureKind::Aborted).elapsed_ms(3))
        .await;
    let after = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
        case.sb(B).await,
        case.evaluation(e1).await,
    );
    assert_eq!(after, before, "after 5: nothing moved");
    assert_eq!(case.signals(W1), vec![], "after 5: signals(W1)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn a08_a_running_job_nobody_aborted_is_not_reaped_even_with_a_zero_grace() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    let reaped = case.reap_overdue_aborts(Duration::ZERO).await;
    assert_eq!(reaped, Vec::<String>::new(), "after 2: nothing reaped");
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 2: active(build:A)"
    );
    assert!(
        case.open_assignment(&a).await.is_some(),
        "after 2: open(build:A) exists"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let before = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
    );
    case.abandoned_dispatch_sweep().await;
    let after = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
    );
    assert_eq!(after, before, "after 3: unchanged");
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 3: active(build:A)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a09_a_building_report_for_an_aborted_build_is_answered_with_abort() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.signals(W1);
    case.check("step 1").await;

    assert_eq!(case.abort_build(e1, A).await, Ok(()), "after 2: result");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.job(e1, A).await.map(|j| j.aborted),
        Some(true),
        "after 2: build_job(E1, A).aborted"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 1, 0, 0],
        "after 2: counters(E1)"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 2: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 2").await;

    case.report_building(W1, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A) == []");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 3: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 3").await;

    case.fail(W1, &a, a1, failure(BuildFailureKind::Aborted).elapsed_ms(1))
        .await;
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 4: dj(build:A)"
    );
    assert_eq!(case.active(&a).await, None, "after 4: active(build:A)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a10_abort_build_is_refused_while_another_live_evaluation_names_the_build() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().shared(e1, e2)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    assert_eq!(
        case.abort_build(e1, A).await,
        Err(BuildRefusal::NeededByOtherEvaluation),
        "after 2: result"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.job(e1, A).await.map(|j| j.aborted),
        Some(false),
        "after 2: build_job(E1, A).aborted"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.abort_evaluation(e2).await;
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E2).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    assert_eq!(case.abort_build(e1, A).await, Ok(()), "after 4: result");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 4: sb(A).status"
    );
    assert_eq!(
        case.job(e1, A).await.map(|j| j.aborted),
        Some(true),
        "after 4: build_job(E1, A).aborted"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 4: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a11_aborting_a_queued_pending_build_fails_its_dependents_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    let b = case.key(B).await;

    case.admit_startable_moves().await;
    case.check("step 1").await;

    assert_eq!(case.abort_build(e1, B).await, Ok(()), "after 2: result");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.job(e1, B).await.map(|j| j.aborted),
        Some(true),
        "after 2: build_job(E1, B).aborted"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 2: counters(E1)"
    );
    assert!(!case.pending(&b).await, "after 2: pending(build:B)");
    assert_eq!(case.assignments(&b).await, vec![], "after 2: dj == []");
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&b).await, "after 3: pending(build:B)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a12_a_retry_is_refused_while_the_aborted_job_is_active_and_reopens_after_confirmation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    assert_eq!(case.abort_build(e1, A).await, Ok(()), "after 2: result");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    case.signals(W1);
    case.check("step 2").await;

    assert_eq!(
        case.retry_build(e1, A).await,
        Err(BuildRefusal::WorkerStillStopping),
        "after 3: result"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    case.fail(W1, &a, a1, failure(BuildFailureKind::Aborted).elapsed_ms(1))
        .await;
    case.check("step 4").await;

    assert_eq!(case.retry_build(e1, A).await, Ok(()), "after 5: result");
    let build = case.sb(A).await;
    assert_eq!(
        (build.status, build.probed, build.attempt),
        (BuildStatus::Created, false, 0),
        "after 5: sb(A) {{status, probed, attempt}}"
    );
    assert_eq!(
        case.job(e1, A).await.map(|j| j.aborted),
        Some(false),
        "after 5: build_job(E1, A).aborted"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 5: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 0],
        "after 5: counters(E1)"
    );
    case.check("step 5").await;

    case.upstream_probed(&[A]).await;
    case.admit_startable_moves().await;
    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let a2 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 6: W1 gets build:A again")
        .assignment_id();
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 6: dj(build:A)"
    );
    let outcomes: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| t.outcome)
        .collect();
    assert_eq!(
        outcomes,
        vec![AttemptOutcome::Aborted],
        "after 6: att(A) == [{{Aborted}}]"
    );
    case.check("step 6").await;

    case.report_building(W1, &a).await;

    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.dispatched_job))
        .collect();
    assert!(
        matches!(
            attempts.as_slice(),
            [(AttemptOutcome::Aborted, _), (AttemptOutcome::Running, Some(open))] if *open == a2
        ),
        "after 7: att(A) == [{{Aborted}}, {{Running, a2}}], got {attempts:?}"
    );
    case.check("step 7").await;

    case.finish().await;
}

#[tokio::test]
async fn a13_a_retry_clears_probed_so_the_build_waits_for_a_new_probe_answer() {
    let u1 = UpstreamServer::start().await;
    u1.miss(A_OUT).await;
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .one(e1, A, A_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            )
            .evaluation_status(EvaluationStatus::Failed)
            .into_cache(CacheId::now_v7())
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
    let a = case.key(A).await;

    assert_eq!(case.retry_build(e1, A).await, Ok(()), "after 1: result");
    let build = case.sb(A).await;
    assert_eq!(
        (build.status, build.probed, build.attempt),
        (BuildStatus::Created, false, 0),
        "after 1: sb(A) {{status, probed, attempt}}"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    assert!(!case.pending(&a).await, "after 1: pending(build:A)");
    case.check("step 1").await;

    let plan = case.plan_probes(&[A]).await;
    assert_eq!(
        planned_targets(&plan),
        BTreeMap::from([(e1, BTreeSet::from([A_OUT.to_owned()]))]),
        "after 2: result.targets"
    );
    case.check("step 2").await;

    case.probe_round(&[A]).await;
    let build = case.sb(A).await;
    assert_eq!(
        (build.status, build.probed),
        (BuildStatus::Queued, true),
        "after 3: sb(A) {{status, probed}}"
    );
    case.check("step 3").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 4: pending(build:A)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a14_cancel_aborts_running_keys_and_drops_pending_keys_without_moving_the_graph() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .startable(A, A_OUT)
            .startable(B, B_OUT)
            .startable(C, C_OUT),
    )
    .await;
    let key = eval_key(e1);
    case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.connect(W2, w_all().slots(2), &[]).await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);
    case.check("step 0").await;

    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.scores(
        W2,
        &[
            (c.as_str(), 0, 0, false),
            (a.as_str(), 5, 5_000_000, false),
            (b.as_str(), 5, 5_000_000, false),
        ],
    )
    .await;
    let handed = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 1: W2 gets build:C");
    assert_eq!(handed.job_id(), c, "after 1: result.job_id");
    case.report_building(W2, &c).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;

    case.cancel_evaluation_jobs(e1, &[B, C]).await;
    assert_eq!(
        case.active(&key).await.as_deref(),
        Some(W1),
        "after 2: active(eval:E1)"
    );
    assert_eq!(
        case.active(&c).await.as_deref(),
        Some(W2),
        "after 2: active(build:C)"
    );
    assert!(!case.pending(&b).await, "after 2: pending(build:B)");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.sb(C).await.status,
        BuildStatus::Building,
        "after 2: sb(C).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 2: evaluation(E1).status"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == key),
        "after 2: signals(W1) == [Abort{{eval:E1}}], got {signals:?}"
    );
    let signals = case.signals(W2);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == c),
        "after 2: signals(W2) == [Abort{{build:C}}], got {signals:?}"
    );
    case.check("step 2").await;

    case.resync_startable_set().await;
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a15_a_late_completed_report_after_an_abort_moves_nothing_in_the_graph() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 2: signals(W1) == [Abort{{build:A}}], got {signals:?}"
    );
    case.check("step 2").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 3: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(case.active(&a).await, None, "after 3: active(build:A)");
    assert_ne!(
        case.attempts(A).await.first().map(|t| t.outcome),
        Some(AttemptOutcome::Running),
        "after 3: att(A)[0].outcome"
    );
    case.signals(W1);
    case.check("step 3").await;

    let before = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
        case.evaluation(e1).await,
    );
    case.fail(W1, &a, a1, failure(BuildFailureKind::Aborted).elapsed_ms(5))
        .await;
    let after = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
        case.evaluation(e1).await,
    );
    assert_eq!(after, before, "after 4: nothing changed");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn a16_a_waiting_evaluation_with_unmet_systems_is_aborted_unless_the_task_waits() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let unmet = WaitingReason::workers(
        vec![UnmetRequirement {
            architecture: ARM.into(),
            required_features: Vec::new(),
            build_count: 1,
        }],
        1,
        vec![X86.into()],
    )
    .to_json();
    let case = SchedulerCase::start(
        shape()
            .one(e1, A, A_OUT)
            .system(ARM)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(unmet.clone())
            .one(e2, B, B_OUT)
            .system(ARM)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(unmet.clone())
            .task_option(|t| t.wait_for_workers = true),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    case.backdate_evaluation(e1, ago(301)).await;
    case.backdate_evaluation(e2, ago(301)).await;

    case.refresh_waiting_state().await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (EvaluationStatus::Aborted, None),
        "after 1: evaluation(E1) {{status, waiting_reason}}"
    );
    assert!(
        evaluation.finished_at.is_some(),
        "after 1: evaluation(E1).finished_at set"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 1: counters(E1)"
    );
    let messages: Vec<_> = case
        .messages(e1)
        .await
        .into_iter()
        .map(|m| (m.level, m.source, m.message))
        .collect();
    assert_eq!(
        messages,
        vec![(
            MessageLevel::Warning,
            Some("scheduler".to_owned()),
            "Aborted: no connected worker provides aarch64-linux (1 builds). Enable waiting for workers on the task to wait instead.".to_owned(),
        )],
        "after 1: evaluation_message(E1)"
    );
    let waiting = case.evaluation(e2).await;
    assert_eq!(
        (waiting.status, waiting.waiting_reason),
        (EvaluationStatus::Waiting, Some(unmet)),
        "after 1: evaluation(E2) {{status, waiting_reason}}"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 1: sb(B).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn a17_aborting_a_waiting_evaluation_clears_its_waiting_reason() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .one(e1, A, A_OUT)
            .evaluation_status(EvaluationStatus::Waiting)
            .waiting_reason(WaitingReason::graph_stuck(1).to_json()),
    )
    .await;

    case.abort_evaluation(e1).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (EvaluationStatus::Aborted, None),
        "after 1: evaluation(E1) {{status, waiting_reason}}"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 1: sb(A).status"
    );
    case.check("step 1").await;

    let before = (case.evaluation(e1).await, case.sb(A).await);
    case.refresh_waiting_state().await;
    let after = (case.evaluation(e1).await, case.sb(A).await);
    assert_eq!(after, before, "after 2: unchanged");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn a18_a_prioritized_evaluation_wins_against_an_equal_smaller_id_candidate() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e2, B, B_OUT).one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    assert!(b < a, "seed: build:B sorts before build:A");

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.check("step 1").await;

    case.prioritize_evaluation(e1).await;
    assert!(
        case.evaluation(e1).await.prioritized,
        "after 2: evaluation(E1).prioritized"
    );
    assert!(!case.sb(A).await.prioritized, "after 2: sb(A).prioritized");
    case.check("step 2").await;

    let handed = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        handed.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 3: result.job_id"
    );
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 3: open(build:A)");
    assert!(
        open.score_breakdown["rules"]["QosRule"].as_f64() > Some(0.0),
        "after 3: open(build:A).score_breakdown lifts by QosRule, got {}",
        open.score_breakdown
    );
    case.check("step 3").await;

    case.abort_evaluation(e1).await;
    assert!(
        !case.evaluation(e1).await.prioritized,
        "after 4: evaluation(E1).prioritized"
    );
    case.signals(W1);
    case.check("step 4").await;

    case.prioritize_evaluation(e1).await;
    assert!(
        !case.evaluation(e1).await.prioritized,
        "after 5: evaluation(E1).prioritized"
    );
    assert_eq!(case.signals(W1), vec![], "after 5: signals(W1)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn a19_prioritize_build_marks_the_shared_build_and_the_pending_job() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    assert!(a < b, "seed: build:A sorts before build:B");

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.check("step 1").await;

    case.prioritize_build(B).await;
    assert!(case.sb(B).await.prioritized, "after 2: sb(B).prioritized");
    assert!(!case.sb(A).await.prioritized, "after 2: sb(A).prioritized");
    assert_eq!(
        case.pending_build_job(&b).await.map(|j| j.prioritized),
        Some(true),
        "after 2: the pending job of B is prioritized"
    );
    case.check("step 2").await;

    let handed = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        handed.as_ref().map(|j| j.job_id().to_owned()),
        Some(b.clone()),
        "after 3: result.job_id"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn a20_aborting_one_of_two_evaluations_sharing_a_passthrough_keeps_the_substitute_job() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .passthrough(e1)
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .entry_point(&attr_of(A))
            .job(),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    let Job::Build(build) = &assignment.job else {
        panic!("after 1: a build job, got {:?}", assignment.job);
    };
    assert_eq!(
        build.build.kind,
        BuildSpecKind::Substitute,
        "after 1: result.job.kind"
    );
    let a1 = assignment.assignment_id();
    case.report_building(W1, &a).await;
    case.signals(W1);
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.report_build_output(
        W1,
        &a,
        vec![build_output(A, A_OUT)],
        true,
        Some(BuildMetrics {
            peak_ram_mb: Some(100),
            ..Default::default()
        }),
    )
    .await;
    case.commit_nar(NarCommit {
        targets: case.project_caches(e2).await,
        ..output_nar(A, A_OUT)
    })
    .await;
    case.complete(W1, &a, a1, Vec::new(), 2).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Substituted,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 3: evaluation(E2).status"
    );
    assert_eq!(
        case.derivation_metrics(A).await,
        vec![],
        "after 3: derivation_metric(A)"
    );
    case.check("step 3").await;

    case.finish().await;
}
