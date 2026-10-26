/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;
use std::time::Duration;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::cluster_attempt::ClusterAttemptOutcome;
use gradient_entity::cluster_job::ClusterJobStatus;
use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{ClusterAttemptId, ClusterJobId, EvaluationId};
use gradient_entity::{cluster_job, cluster_member, eval_split};
use gradient_fixtures::shape::shape;
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::{
    BuildFailureKind, ClusterPeer, JobKind, SPLIT_COMPANION_ROLE, SPLIT_PRIMARY_ROLE,
};

use super::{
    A, A_OUT, B, B_OUT, CaseShapes, ClusterAssignment, D, D_OUT, SchedulerCase, SlotKind, ago,
    eval_key, failure, output_nar, w_all, w_eval, w_network_eval, without_offers,
};

const W1: &str = "w1";
const W2: &str = "w2";
const W3: &str = "w3";
const W4: &str = "w4";
const SEEDED_MEMBERS: [&str; 2] = ["member-0", "member-1"];

#[tokio::test]
async fn k01_members_join_the_book_and_are_not_handed_out_as_single_jobs() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);

    case.assign_queued_evals().await;
    assert!(case.tracked(&eval).await, "after 1: tracked(eval:E1)");
    assert!(!case.pending(&eval).await, "after 1: pending(eval:E1)");
    assert_eq!(
        present_members(&case, c).await,
        vec![eval.clone()],
        "after 1: members_present"
    );
    case.signals(W1);
    case.check("step 1").await;

    let offer = case.offers_full(W1).await;
    assert_eq!(
        candidate_keys(&offer.candidates),
        vec![eval.clone()],
        "after 2: candidates"
    );
    case.score_all(W1).await;
    let assignment = case.request_job(W1, JobKind::Flake).await;
    assert!(assignment.is_none(), "after 2: result == nothing");
    assert!(
        case.assignments_of(W1).await.is_empty(),
        "after 2: dj == []"
    );
    assert!(
        case.idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Eval)),
        "after 2: idle_slots contains (W1, Flake)"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(case.tracked(&build).await, "after 3: tracked(build:A)");
    assert!(!case.pending(&build).await, "after 3: pending(build:A)");
    assert!(
        case.waiting_cluster(c).await.is_some_and(|w| w.ready()),
        "after 3: can_start"
    );
    case.signals(W1);
    case.check("step 3").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        candidate_keys(&offer.candidates),
        vec![build.clone()],
        "after 4: candidates"
    );
    case.score_all(W1).await;
    let assignment = case.request_job(W1, JobKind::Build).await;
    assert!(assignment.is_none(), "after 4: result == nothing");
    assert!(
        case.assignments(&eval).await.is_empty() && case.assignments(&build).await.is_empty(),
        "after 4: dj == []"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn k02_a_planning_pass_claims_the_attempt_and_both_member_rows() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start_with(shape().cl2(c, e1, e2, A, A_OUT), |config| {
        config.scheduler.cluster_prepare_timeout_secs = seconds(30);
    })
    .await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    case.check("step 1").await;

    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Build).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 2").await;

    case.plan_clusters().await;
    let attempts = case.cluster_attempts(c).await;
    let [attempt] = attempts.as_slice() else {
        panic!("after 3: cluster_attempt(C) has a single row, got {attempts:?}");
    };
    assert_eq!(
        (attempt.started_at, attempt.finished_at, attempt.outcome),
        (None, None, None),
        "after 3: cluster_attempt(C)"
    );
    let t1 = attempt.id;
    let rows = case.assignments(&eval).await;
    let [eval_row] = rows.as_slice() else {
        panic!("after 3: dj(eval:E1) has a single row, got {rows:?}");
    };
    assert_eq!(
        (
            eval_row.finished_at,
            eval_row.worker_id.as_str(),
            eval_row.cluster_attempt,
            eval_row.kind,
            eval_row.project,
            eval_row.task,
        ),
        (
            None,
            W1,
            Some(t1),
            DispatchedJobKind::Eval,
            case.db.project_of(e1).await,
            Some(case.db.task_of(e1).await),
        ),
        "after 3: dj(eval:E1)"
    );
    let rows = case.assignments(&build).await;
    let [build_row] = rows.as_slice() else {
        panic!("after 3: dj(build:A) has a single row, got {rows:?}");
    };
    assert_eq!(
        (
            build_row.finished_at,
            build_row.worker_id.as_str(),
            build_row.cluster_attempt
        ),
        (None, W2, Some(t1)),
        "after 3: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![],
        "after 3: att(A) == []"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 0),
        "after 3: cluster_job(C)"
    );
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![SessionSignal::ClusterAssign {
            job_id: eval.clone()
        }],
        "after 3: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![SessionSignal::ClusterAssign {
            job_id: build.clone()
        }],
        "after 3: signals(W2)"
    );
    assert_eq!(
        case.take_cluster_assignment(W1, &eval).await,
        ClusterAssignment {
            assignment_id: eval_row.id,
            attempt: t1.to_string(),
            role: "head".into(),
            index: 0,
            prepare_secs: 40,
        },
        "after 3: AssignJob of W1"
    );
    assert_eq!(
        case.take_cluster_assignment(W2, &build).await,
        ClusterAssignment {
            assignment_id: build_row.id,
            attempt: t1.to_string(),
            role: "node".into(),
            index: 0,
            prepare_secs: 40,
        },
        "after 3: AssignJob of W2"
    );
    assert_eq!(case.idle_slots().await, vec![], "after 3: idle_slots");
    assert!(
        case.tracked(&eval).await && case.tracked(&build).await,
        "after 3: tracked(eval:E1) and tracked(build:A)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k03_the_last_acceptance_starts_the_attempt_and_sends_the_roster() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let claimed = claim(&case, c, e1).await;
    let t1 = claimed.attempt;

    case.accept(W1, &eval).await;
    assert_eq!(
        only_attempt(&case, c).await.started_at,
        None,
        "after 1: cluster_attempt(t1).started_at"
    );
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Queued,
        "after 1: cluster_job(C).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 1: signals(W2)");
    case.check("step 1").await;

    case.accept(W2, &build).await;
    assert!(
        only_attempt(&case, c).await.started_at.is_some(),
        "after 2: cluster_attempt(t1).started_at set"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Running, 1),
        "after 2: cluster_job(C)"
    );
    let start = SessionSignal::StartCluster {
        attempt: t1.to_string(),
        roster: vec![peer("head", W1), peer("node", W2)],
    };
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![start.clone()],
        "after 2: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![start],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    let before = cluster_rows(&case, c, &[&eval, &build]).await;
    case.accept(W2, &build).await;
    assert_eq!(
        cluster_rows(&case, c, &[&eval, &build]).await,
        before,
        "after 3: nothing changes"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 3: signals(W2)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k04_a_member_reject_fails_the_prepare_and_leaves_the_budget() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let claimed = claim(&case, c, e1).await;
    let t1 = claimed.attempt;

    case.accept(W1, &eval).await;
    case.check("step 1").await;

    case.reject(W2, &build).await;
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (
            attempt.finished_at.is_some(),
            attempt.outcome,
            attempt.started_at
        ),
        (true, Some(ClusterAttemptOutcome::PrepareFailed), None),
        "after 2: cluster_attempt(t1)"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 0),
        "after 2: cluster_job(C)"
    );
    assert_eq!(
        row_outcomes(&case, &eval).await,
        vec![Some(DispatchedJobOutcome::Abandoned)],
        "after 2: dj(eval:E1)"
    );
    assert_eq!(
        row_outcomes(&case, &build).await,
        vec![Some(DispatchedJobOutcome::Abandoned)],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![],
        "after 2: att(A) == []"
    );
    assert!(
        aborts_cluster(&case.signals(W1), t1),
        "after 2: signals(W1) == [AbortCluster{{t1, _}}]"
    );
    assert!(
        aborts_cluster(&case.signals(W2), t1),
        "after 2: signals(W2) == [AbortCluster{{t1, _}}]"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 2: evaluation(E1).status"
    );
    assert!(
        case.tracked(&eval).await && case.tracked(&build).await,
        "after 2: tracked(eval:E1) and tracked(build:A)"
    );
    assert_eq!(
        (case.active(&eval).await, case.active(&build).await),
        (None, None),
        "after 2: active == none"
    );
    case.check("step 2").await;

    let before = cluster_rows(&case, c, &[&eval, &build]).await;
    let member = case.reject(W2, &build).await;
    assert!(!member, "after 3: result == false");
    assert_eq!(
        cluster_rows(&case, c, &[&eval, &build]).await,
        before,
        "after 3: nothing touched"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k05_a_prepare_that_times_out_is_closed_by_the_next_planning_pass() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start_with(shape().cl2(c, e1, e2, A, A_OUT), |config| {
        config.scheduler.cluster_prepare_timeout_secs = seconds(1);
    })
    .await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Build).await;
    case.signals(W1);
    case.signals(W2);
    case.plan_clusters().await;
    assert_eq!(
        without_offers(case.signals(W1)),
        vec![SessionSignal::ClusterAssign {
            job_id: eval.clone()
        }],
        "after 1: signals(W1)"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![SessionSignal::ClusterAssign {
            job_id: build.clone()
        }],
        "after 1: signals(W2)"
    );
    let prepare_secs = (
        case.take_cluster_assignment(W1, &eval).await.prepare_secs,
        case.take_cluster_assignment(W2, &build).await.prepare_secs,
    );
    assert_eq!(prepare_secs, (11, 11), "after 1: prepare_secs 11");
    let t1 = only_attempt(&case, c).await.id;
    case.check("step 1").await;

    case.accept(W1, &eval).await;
    tokio::time::sleep(Duration::from_secs_f64(1.2)).await;
    case.check("step 2").await;

    case.plan_clusters().await;
    let attempts = case.cluster_attempts(c).await;
    assert_eq!(
        attempts
            .iter()
            .map(|a| (a.id, a.outcome))
            .collect::<Vec<_>>(),
        vec![(t1, Some(ClusterAttemptOutcome::PrepareFailed))],
        "after 3: cluster_attempt(C), no new attempt in the same pass"
    );
    for key in [&eval, &build] {
        assert_eq!(
            row_outcomes(&case, key).await,
            vec![Some(DispatchedJobOutcome::Abandoned)],
            "after 3: dj({key})"
        );
    }
    assert!(
        aborts_cluster(&case.signals(W1), t1),
        "after 3: signals(W1) == [AbortCluster{{t1, _}}]"
    );
    assert!(
        aborts_cluster(&case.signals(W2), t1),
        "after 3: signals(W2) == [AbortCluster{{t1, _}}]"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 0),
        "after 3: cluster_job(C)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k07_a_member_open_as_a_single_assignment_makes_the_whole_claim_lose() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Build).await;
    case.check("step 1").await;

    let seeded = case.open_build_row(e2, A, "other", ago(60)).await;
    case.signals(W1);
    case.signals(W2);
    case.plan_clusters().await;
    assert_eq!(
        case.cluster_attempts(c).await,
        vec![],
        "after 2: cluster_attempt(C)"
    );
    assert_eq!(
        case.assignments(&eval).await,
        vec![],
        "after 2: dj(eval:E1)"
    );
    let rows = case.assignments(&build).await;
    assert!(
        matches!(rows.as_slice(), [row] if row.id == seeded && row.finished_at.is_none()),
        "after 2: dj(build:A) == [the seeded open row], got {rows:?}"
    );
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 2: signals(W2)");
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 0),
        "after 2: cluster_job(C)"
    );
    assert!(!case.tracked(&eval).await, "after 2: tracked(eval:E1)");
    case.check("step 2").await;

    case.assign_queued_evals().await;
    assert!(case.tracked(&eval).await, "after 3: tracked(eval:E1)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k08_an_attempt_of_a_second_instance_makes_the_local_claim_insert_nothing() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    second.connect(W3, w_all(), &[]).await;
    second.connect(W4, w_all(), &[]).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    second.assign_queued_evals().await;
    second.resync_startable_set().await;
    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Build).await;
    second.idle(W3, JobKind::Flake).await;
    second.idle(W4, JobKind::Build).await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 1").await;
    second.check("step 1").await;

    second.plan_clusters().await;
    let attempts = case.cluster_attempts(c).await;
    assert!(
        matches!(attempts.as_slice(), [attempt] if attempt.finished_at.is_none()),
        "after 2: cluster_attempt(C) has a single open row, got {attempts:?}"
    );
    let workers = (
        case.open_assignment(&eval).await.map(|r| r.worker_id),
        case.open_assignment(&build).await.map(|r| r.worker_id),
    );
    assert_eq!(
        workers,
        (Some(W3.to_owned()), Some(W4.to_owned())),
        "after 2: rows for E1 and A on W3, W4"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    case.plan_clusters().await;
    assert_eq!(
        case.cluster_attempts(c).await,
        attempts,
        "after 3: cluster_attempt(C) still a single row"
    );
    for worker in [W1, W2] {
        assert_eq!(
            case.assignments_of(worker).await,
            vec![],
            "after 3: no rows on {worker}"
        );
        assert_eq!(case.signals(worker), vec![], "after 3: signals({worker})");
    }
    case.check("step 3").await;
    second.check("step 3").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn k12_member_transitions_wait_for_the_complete_verdict_then_apply() {
    let (c, e1, e2) = cluster_ids();
    let given = shape()
        .evalq(e1)
        .evaluation_member_of(c, "head", false, None)
        .one(e2, A, A_OUT)
        .build_member_of(c, "node", false, None)
        .cluster(c, false, 2, ClusterJobStatus::Queued, 0, ago(0));
    let case = SchedulerCase::start(given).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let started = start(&case, c, e1).await;

    case.report_status(W1, &eval, EvaluationStatus::EvaluatingFlake)
        .await;
    case.report_status(W1, &eval, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.complete(W1, &eval, started.eval.assignment_id, Vec::new(), 10)
        .await;
    assert_eq!(
        case.assignment_outcomes(&eval).await,
        vec![(
            started.eval.assignment_id,
            Some(DispatchedJobOutcome::Completed)
        )],
        "after 1: dj(eval:E1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 1: evaluation(E1).status waits for the verdict"
    );
    assert_eq!(
        only_attempt(&case, c).await.outcome,
        None,
        "after 1: cluster_attempt(t1).outcome"
    );
    case.check("step 1").await;

    case.report_building(W2, &build).await;
    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W2, &build, started.build.assignment_id, Vec::new(), 20)
        .await;
    assert_eq!(
        case.assignment_outcomes(&build).await,
        vec![(
            started.build.assignment_id,
            Some(DispatchedJobOutcome::Completed)
        )],
        "after 2: dj(build:A)"
    );
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (attempt.outcome, attempt.finished_at.is_some()),
        (Some(ClusterAttemptOutcome::Succeeded), true),
        "after 2: cluster_attempt(t1)"
    );
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Completed,
        "after 2: cluster_job(C).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Completed,
        "after 2: evaluation(E2).status"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn k14_a_transient_member_failure_with_budget_left_requeues_the_cluster() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let started = start(&case, c, e1).await;
    case.report_building(W2, &build).await;
    let build_attempt_before = case.sb(A).await.attempt;
    case.check("step 0").await;

    case.fail(
        W2,
        &build,
        started.build.assignment_id,
        failure(BuildFailureKind::Transient).elapsed_ms(2),
    )
    .await;
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (attempt.outcome, attempt.finished_at.is_some()),
        (Some(ClusterAttemptOutcome::Failed), true),
        "after 1: cluster_attempt(t1)"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 1),
        "after 1: cluster_job(C)"
    );
    assert_eq!(
        case.assignment_outcomes(&build).await,
        vec![(
            started.build.assignment_id,
            Some(DispatchedJobOutcome::Failed)
        )],
        "after 1: dj(build:A)"
    );
    assert_eq!(
        row_outcomes(&case, &eval).await,
        vec![Some(DispatchedJobOutcome::Abandoned)],
        "after 1: dj(eval:E1)"
    );
    assert!(
        aborts_cluster(&case.signals(W1), started.attempt),
        "after 1: signals(W1) == [AbortCluster{{t1, _}}]"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::Queued, build_attempt_before),
        "after 1: sb(A) Queued, attempt unchanged"
    );
    let attempts = case.attempts(A).await;
    assert!(
        attempts
            .first()
            .is_some_and(|a| a.outcome == AttemptOutcome::Aborted
                && a.reason != Some(AttemptFailureReason::WorkerLost)),
        "after 1: att(A)[0] == {{Aborted, reason != WorkerLost}}, got {attempts:?}"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert!(
        !case.tracked(&eval).await && !case.tracked(&build).await,
        "after 1: tracked(eval:E1) and tracked(build:A) are false"
    );
    case.check("step 1").await;

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    assert_members_return(&case, c, &eval, &build, "after 2").await;
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn k15_a_retry_budget_of_1_still_allows_a_retry() {
    let (c, e1, e2) = cluster_ids();
    let given = shape()
        .cl2(c, e1, e2, A, A_OUT)
        .cluster_option(c, |cluster| cluster.retry_budget = 1);
    let case = SchedulerCase::start(given).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let started = start(&case, c, e1).await;
    case.report_building(W2, &build).await;
    case.check("step 0").await;

    case.fail(
        W2,
        &build,
        started.build.assignment_id,
        failure(BuildFailureKind::Transient).elapsed_ms(2),
    )
    .await;
    assert_eq!(
        only_attempt(&case, c).await.outcome,
        Some(ClusterAttemptOutcome::Failed),
        "after 1: cluster_attempt(t1).outcome"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 1),
        "after 1: cluster_job(C)"
    );
    case.check("step 1").await;

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    assert_members_return(&case, c, &eval, &build, "after 2").await;
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn k16_a_retry_budget_of_0_fails_the_cluster_on_the_first_member_failure() {
    let (c, e1, e2) = cluster_ids();
    let given = shape()
        .cl2(c, e1, e2, A, A_OUT)
        .cluster_option(c, |cluster| cluster.retry_budget = 0);
    let case = SchedulerCase::start(given).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let started = start(&case, c, e1).await;
    case.report_building(W2, &build).await;
    case.check("step 0").await;

    case.fail(
        W2,
        &build,
        started.build.assignment_id,
        failure(BuildFailureKind::Transient).elapsed_ms(2),
    )
    .await;
    assert_eq!(
        only_attempt(&case, c).await.outcome,
        Some(ClusterAttemptOutcome::Failed),
        "after 1: cluster_attempt(t1).outcome"
    );
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Failed,
        "after 1: cluster_job(C).status"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes(A).await.first(),
        Some(&AttemptOutcome::Failed),
        "after 1: att(A)[0]"
    );
    assert!(
        aborts_cluster(&case.signals(W1), started.attempt),
        "after 1: signals(W1) == [AbortCluster{{t1, _}}]"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        row_outcomes(&case, &eval).await,
        vec![Some(DispatchedJobOutcome::Abandoned)],
        "after 1: dj(eval:E1)"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E2).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn k17_aborting_a_member_evaluation_aborts_the_attempt_and_the_survivors() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    let started = start(&case, c, e1).await;
    case.report_building(W2, &build).await;
    case.report_status(W1, &eval, EvaluationStatus::EvaluatingFlake)
        .await;
    case.signals(W1);
    case.signals(W2);
    case.check("step 0").await;

    case.abort_evaluation(e1).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 1: evaluation(E1).status"
    );
    let signals = without_offers(case.signals(W1));
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == eval),
        "after 1: signals(W1) == [Abort{{eval:E1, _}}], got {signals:?}"
    );
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (attempt.outcome, attempt.finished_at),
        (None, None),
        "after 1: cluster_attempt(t1) open"
    );
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Running,
        "after 1: cluster_job(C).status"
    );
    assert_eq!(
        without_offers(case.signals(W2)),
        vec![],
        "after 1: signals(W2)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 1: sb(A).status"
    );
    case.check("step 1").await;

    case.fail(
        W1,
        &eval,
        started.eval.assignment_id,
        failure(BuildFailureKind::Aborted).elapsed_ms(1),
    )
    .await;
    assert_eq!(
        only_attempt(&case, c).await.outcome,
        Some(ClusterAttemptOutcome::Aborted),
        "after 2: cluster_attempt(t1).outcome"
    );
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Aborted,
        "after 2: cluster_job(C).status"
    );
    assert!(
        aborts_cluster(&case.signals(W2), started.attempt),
        "after 2: signals(W2) == [AbortCluster{{t1, _}}]"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.evaluation(e2).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation(E2).status"
    );
    assert_eq!(
        row_outcomes(&case, &build).await,
        vec![Some(DispatchedJobOutcome::Abandoned)],
        "after 2: dj(build:A)"
    );
    case.check("step 2").await;

    let before = (
        cluster_rows(&case, c, &[&eval, &build]).await,
        case.sb(A).await,
        case.attempts(A).await,
    );
    case.fail(
        W2,
        &build,
        started.build.assignment_id,
        failure(BuildFailureKind::Aborted).elapsed_ms(1),
    )
    .await;
    let after = (
        cluster_rows(&case, c, &[&eval, &build]).await,
        case.sb(A).await,
        case.attempts(A).await,
    );
    assert_eq!(after, before, "after 2: the fail of W2 moves nothing");
    for key in [&eval, &build] {
        let rows = case.assignments(key).await;
        assert!(
            rows.iter().all(|r| r.finished_at.is_some()),
            "after 2: dj({key}) closed, got {rows:?}"
        );
        assert_eq!(case.active(key).await, None, "after 2: active({key})");
    }
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k17b_the_last_acceptance_does_not_start_an_aborted_cluster() {
    let (c, e1, e2) = cluster_ids();
    let case = SchedulerCase::start(shape().cl2(c, e1, e2, A, A_OUT)).await;
    let (eval, build) = (eval_key(e1), case.key(A).await);
    claim(&case, c, e1).await;

    case.abort_evaluation(e2).await;
    case.check("step 1").await;

    case.plan_clusters().await;
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Aborted,
        "after 2: cluster_job(C).status"
    );
    case.signals(W1);
    case.signals(W2);
    case.check("step 2").await;

    case.accept(W1, &eval).await;
    case.accept(W2, &build).await;
    assert_eq!(
        case.cluster(c).await.status,
        ClusterJobStatus::Aborted,
        "after 3: cluster_job(C).status"
    );
    for worker in [W1, W2] {
        let signals = case.signals(worker);
        assert!(
            !signals
                .iter()
                .any(|s| matches!(s, SessionSignal::StartCluster { .. })),
            "after 3: no StartCluster to {worker}, got {signals:?}"
        );
    }
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (attempt.started_at, attempt.finished_at.is_some()),
        (None, true),
        "after 3: cluster_attempt(t1) closed without a start"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn k27_a_cluster_created_long_ago_but_only_now_able_to_start_is_not_aged_at_once() {
    let (c, e1, e2) = cluster_ids();
    let e4 = EvaluationId::now_v7();
    let given = shape()
        .cl2(c, e1, e2, A, A_OUT)
        .cluster_option(c, |cluster| {
            cluster.created_at = ago(700);
            cluster.updated_at = ago(700);
        })
        .one(e4, D, D_OUT);
    let case = SchedulerCase::start_with(given, |config| {
        config.scheduler.cluster_reserve_after_secs = seconds(600);
    })
    .await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let d = case.key(D).await;

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    case.offers_full(W2).await;
    case.score_all(W2).await;
    let busy = case.request_job(W2, JobKind::Build).await;
    assert_eq!(
        busy.as_ref().map(|a| a.job_id().to_owned()),
        Some(d),
        "after 1: W2 runs the single build D"
    );
    case.check("step 1").await;

    case.idle(W1, JobKind::Flake).await;
    case.plan_clusters().await;
    assert!(
        case.reservation().await.is_none(),
        "after 2: cluster_snapshot.reservation == none"
    );
    assert_eq!(
        case.cluster_attempts(c).await,
        vec![],
        "after 2: cluster_attempt(C)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn k28_startup_recovery_requeues_a_running_cluster_and_aborts_a_finished_one() {
    let (c1, c2) = (ClusterJobId::now_v7(), ClusterJobId::now_v7());
    let (e1, e2, e5, e6) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let given = shape()
        .cl2(c1, e1, e2, A, A_OUT)
        .cluster_option(c1, |cluster| {
            cluster.retry_budget = 1;
            cluster.status = ClusterJobStatus::Running;
            cluster.attempts = 1;
            cluster.created_at = ago(60);
            cluster.updated_at = ago(60);
        })
        .cluster_attempt(c1, ClusterAttemptId::now_v7(), Some(ago(60)), None, None)
        .cl2(c2, e5, e6, B, B_OUT)
        .cluster_option(c2, |cluster| {
            cluster.status = ClusterJobStatus::Running;
            cluster.attempts = 1;
        })
        .cluster_attempt(c2, ClusterAttemptId::now_v7(), Some(ago(60)), None, None)
        .evaluation(e5)
        .evaluation_status(EvaluationStatus::Completed);
    let case = SchedulerCase::start(given).await;

    case.startup_recovery().await;
    assert_eq!(
        only_attempt(&case, c1).await.outcome,
        Some(ClusterAttemptOutcome::Aborted),
        "after 1: cluster_attempt(C1)[0].outcome"
    );
    assert_eq!(
        case.cluster(c1).await.status,
        ClusterJobStatus::Queued,
        "after 1: cluster_job(C1).status"
    );
    assert_eq!(
        only_attempt(&case, c2).await.outcome,
        Some(ClusterAttemptOutcome::Aborted),
        "after 1: cluster_attempt(C2)[0].outcome"
    );
    assert_eq!(
        case.cluster(c2).await.status,
        ClusterJobStatus::Aborted,
        "after 1: cluster_job(C2).status"
    );
    assert_eq!(
        seeded_member_outcomes(&case).await,
        vec![Some(DispatchedJobOutcome::Abandoned); 4],
        "after 1: all member rows Abandoned"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn k29_startup_recovery_closes_an_unstarted_attempt_as_prepare_failed() {
    let (c, e1, e2) = cluster_ids();
    let given = shape().cl2(c, e1, e2, A, A_OUT).cluster_attempt(
        c,
        ClusterAttemptId::now_v7(),
        None,
        None,
        None,
    );
    let case = SchedulerCase::start(given).await;

    case.startup_recovery().await;
    let attempt = only_attempt(&case, c).await;
    assert_eq!(
        (attempt.outcome, attempt.finished_at.is_some()),
        (Some(ClusterAttemptOutcome::PrepareFailed), true),
        "after 1: cluster_attempt(C)[0]"
    );
    assert_eq!(
        cluster_state(&case, c).await,
        (ClusterJobStatus::Queued, 0),
        "after 1: cluster_job(C)"
    );
    assert_eq!(
        seeded_member_outcomes(&case).await,
        vec![Some(DispatchedJobOutcome::Abandoned); 2],
        "after 1: member rows Abandoned"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn k30_a_split_cluster_shrinks_to_the_free_seats() {
    let (c, e1, _) = cluster_ids();
    let case = SchedulerCase::start(split_of_three(c, e1)).await;
    case.connect(W1, w_network_eval(), &[]).await;
    case.connect(W2, w_eval(), &[]).await;
    case.assign_queued_evals().await;
    assert_eq!(
        present_members(&case, c).await.len(),
        3,
        "after 1: members_present"
    );
    case.check("step 1").await;

    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Flake).await;
    case.plan_clusters().await;
    assert_eq!(
        split_indexes(&case, c).await,
        vec![0, 1],
        "after 2: cluster_member(C) lost the companion with index 2"
    );
    only_attempt(&case, c).await;
    let seated = [eval_key(e1), format!("{}:1", eval_key(e1))];
    for key in &seated {
        assert_eq!(case.assignments(key).await.len(), 1, "after 2: dj({key})");
    }
    case.signals(W1);
    case.signals(W2);
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn k31_a_split_cluster_with_one_free_seat_dissolves_at_once() {
    let (c, e1, _) = cluster_ids();
    let case = SchedulerCase::start(split_of_three(c, e1)).await;
    case.connect(W1, w_eval(), &[]).await;
    case.assign_queued_evals().await;
    case.idle(W1, JobKind::Flake).await;
    case.check("step 1").await;

    case.plan_clusters().await;
    assert!(
        case.db.rows::<cluster_job::Entity>().await.is_empty(),
        "after 2: cluster_job == []"
    );
    assert!(
        split_indexes(&case, c).await.is_empty(),
        "after 2: cluster_member == []"
    );
    assert!(
        case.db.rows::<eval_split::Entity>().await.is_empty(),
        "after 2: eval_split == []"
    );
    assert!(
        case.waiting_cluster(c).await.is_none(),
        "after 2: waiting_cluster(C)"
    );
    case.check("step 2").await;

    case.assign_queued_evals().await;
    assert!(
        case.pending(&eval_key(e1)).await,
        "after 3: eval:E1 routes as a single job"
    );
    case.check("step 3").await;

    case.finish().await;
}

fn split_of_three(
    cluster: ClusterJobId,
    evaluation: EvaluationId,
) -> gradient_fixtures::shape::Shape {
    shape()
        .evalq(evaluation)
        .split_member_of(cluster, SPLIT_PRIMARY_ROLE, 0)
        .split_member_of(cluster, SPLIT_COMPANION_ROLE, 1)
        .split_member_of(cluster, SPLIT_COMPANION_ROLE, 2)
        .eval_split_of(cluster)
        .cluster(cluster, false, 2, ClusterJobStatus::Queued, 0, ago(0))
        .cluster_option(cluster, |c| {
            c.dissolve_after = Some(gradient_types::now() + chrono::TimeDelta::seconds(30));
        })
}

async fn split_indexes(case: &SchedulerCase, cluster: ClusterJobId) -> Vec<i16> {
    let mut indexes: Vec<i16> = case
        .db
        .rows::<cluster_member::Entity>()
        .await
        .into_iter()
        .filter(|m| m.cluster_job == cluster)
        .filter_map(|m| m.eval_index)
        .collect();
    indexes.sort_unstable();
    indexes
}

struct Claimed {
    attempt: ClusterAttemptId,
    eval: ClusterAssignment,
    build: ClusterAssignment,
}

fn cluster_ids() -> (ClusterJobId, EvaluationId, EvaluationId) {
    (
        ClusterJobId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    )
}

fn seconds(secs: u64) -> NonZeroU64 {
    NonZeroU64::new(secs).expect("a positive number of seconds")
}

async fn claim(case: &SchedulerCase, cluster: ClusterJobId, e1: EvaluationId) -> Claimed {
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    case.idle(W1, JobKind::Flake).await;
    case.idle(W2, JobKind::Build).await;
    case.plan_clusters().await;
    let attempt = only_attempt(case, cluster).await.id;
    let eval = case.take_cluster_assignment(W1, &eval_key(e1)).await;
    let build = case.take_cluster_assignment(W2, &case.key(A).await).await;
    case.signals(W1);
    case.signals(W2);
    case.check("the claim").await;
    Claimed {
        attempt,
        eval,
        build,
    }
}

async fn start(case: &SchedulerCase, cluster: ClusterJobId, e1: EvaluationId) -> Claimed {
    let claimed = claim(case, cluster, e1).await;
    case.accept(W1, &eval_key(e1)).await;
    case.accept(W2, &case.key(A).await).await;
    case.signals(W1);
    case.signals(W2);
    case.check("the start").await;
    claimed
}

async fn only_attempt(
    case: &SchedulerCase,
    cluster: ClusterJobId,
) -> gradient_entity::cluster_attempt::Model {
    let attempts = case.cluster_attempts(cluster).await;
    let [attempt] = attempts.as_slice() else {
        panic!("cluster {cluster} has a single attempt, got {attempts:?}");
    };
    attempt.clone()
}

async fn cluster_state(case: &SchedulerCase, cluster: ClusterJobId) -> (ClusterJobStatus, i32) {
    let row = case.cluster(cluster).await;
    (row.status, row.attempts)
}

async fn cluster_rows(
    case: &SchedulerCase,
    cluster: ClusterJobId,
    keys: &[&str],
) -> (
    Vec<gradient_entity::cluster_attempt::Model>,
    gradient_entity::cluster_job::Model,
    Vec<gradient_entity::dispatched_job::Model>,
) {
    let mut rows = Vec::new();
    for key in keys {
        rows.extend(case.assignments(key).await);
    }
    (
        case.cluster_attempts(cluster).await,
        case.cluster(cluster).await,
        rows,
    )
}

async fn row_outcomes(case: &SchedulerCase, key: &str) -> Vec<Option<DispatchedJobOutcome>> {
    case.assignments(key)
        .await
        .into_iter()
        .map(|r| r.outcome)
        .collect()
}

async fn seeded_member_outcomes(case: &SchedulerCase) -> Vec<Option<DispatchedJobOutcome>> {
    let mut outcomes = Vec::new();
    for worker in SEEDED_MEMBERS {
        outcomes.extend(
            case.assignments_of(worker)
                .await
                .into_iter()
                .map(|r| r.outcome),
        );
    }
    outcomes
}

async fn present_members(case: &SchedulerCase, cluster: ClusterJobId) -> Vec<String> {
    let mut keys: Vec<String> = case
        .waiting_cluster(cluster)
        .await
        .map(|w| {
            w.members
                .iter()
                .filter(|m| m.job.is_some())
                .map(|m| m.key.clone())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

async fn assert_members_return(
    case: &SchedulerCase,
    cluster: ClusterJobId,
    eval: &str,
    build: &str,
    after: &str,
) {
    let mut expected = vec![eval.to_owned(), build.to_owned()];
    expected.sort();
    assert_eq!(
        present_members(case, cluster).await,
        expected,
        "{after}: members in the book"
    );
    for key in [eval, build] {
        assert!(!case.pending(key).await, "{after}: pending({key})");
        assert!(case.tracked(key).await, "{after}: tracked({key})");
    }
}

fn candidate_keys(candidates: &[gradient_wire::types::JobCandidate]) -> Vec<String> {
    let mut keys: Vec<String> = candidates.iter().map(|c| c.job_id.clone()).collect();
    keys.sort();
    keys
}

fn peer(role: &str, worker: &str) -> ClusterPeer {
    ClusterPeer {
        role: role.into(),
        index: 0,
        worker: worker.into(),
        zone: None,
        endpoint: None,
    }
}

fn aborts_cluster(signals: &[SessionSignal], attempt: ClusterAttemptId) -> bool {
    let attempt = attempt.to_string();
    matches!(
        without_offers(signals.to_vec()).as_slice(),
        [SessionSignal::AbortCluster { attempt: aborted, .. }] if *aborted == attempt
    )
}
