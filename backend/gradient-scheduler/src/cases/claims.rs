/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::{BuildRequirement, BuildSpecKind, Job, JobKind};

use super::{
    A, A_OUT, B, CaseShapes, SchedulerCase, SlotKind, X86, drv_path, is_recent, store_path, w_all,
};

const W1: &str = "w1";
const W2: &str = "w2";

#[tokio::test]
async fn c01_a_won_claim_opens_the_row_and_attempt_and_leaves_the_build_queued() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 1: pending(build:A)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    case.check("step 1").await;

    let offer = case.offers_full(W1).await;
    let candidates: Vec<_> = offer
        .candidates
        .iter()
        .map(|c| (c.job_id.clone(), c.requirement.clone()))
        .collect();
    let x86 = BuildRequirement {
        architecture: X86.into(),
        required_features: Vec::new(),
    };
    assert_eq!(
        candidates,
        vec![(a.clone(), Some(x86))],
        "after 2: candidates"
    );
    assert_eq!(offer.generation, 1, "after 2: generation");
    case.check("step 2").await;

    case.score_all(W1).await;
    case.check("step 3").await;

    let assignment = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 4: W1 gets build:A");
    let a1 = assignment.assignment_id();
    assert_eq!(assignment.job_id(), a, "after 4: result.job_id");
    let Job::Build(build) = &assignment.job else {
        panic!("after 4: a build job, got {:?}", assignment.job);
    };
    let [spec] = build.builds.as_slice() else {
        panic!("after 4: a single build spec, got {:?}", build.builds);
    };
    let outputs: Vec<&str> = spec.outputs.iter().map(|o| o.path.as_str()).collect();
    assert_eq!(
        (
            spec.build_id.clone(),
            spec.drv_path.clone(),
            spec.kind,
            spec.is_fixed_output,
            outputs,
            spec.timeout_secs,
            spec.max_silent_secs,
        ),
        (
            case.sb(A).await.id.to_string(),
            drv_path(A),
            BuildSpecKind::Build,
            false,
            vec![store_path(A_OUT, "a").as_str()],
            Some(14400),
            Some(3600),
        ),
        "after 4: result.job"
    );
    let rows = case.assignments(&a).await;
    let [row] = rows.as_slice() else {
        panic!("after 4: dj(build:A) has a single row, got {rows:?}");
    };
    assert_eq!(
        (
            row.id,
            row.kind,
            row.evaluation_id,
            row.project,
            row.task,
            row.worker_id.as_str(),
            row.cluster_attempt,
        ),
        (
            a1,
            DispatchedJobKind::Build,
            e1,
            case.db.project_of(e1).await,
            None,
            W1,
            None,
        ),
        "after 4: dj(build:A)"
    );
    assert!(is_recent(row.dispatched_at), "after 4: dispatched_at ~now");
    assert!(row.ready_at.is_some(), "after 4: ready_at set");
    assert_eq!(
        (row.finished_at, row.outcome),
        (None, None),
        "after 4: open row"
    );
    let attempts = case.attempts(A).await;
    let [attempt] = attempts.as_slice() else {
        panic!("after 4: att(A) has a single row, got {attempts:?}");
    };
    assert_eq!(
        (
            attempt.outcome,
            attempt.dispatched_job,
            attempt.substitute,
            attempt.build_started_at,
        ),
        (AttemptOutcome::Running, Some(a1), false, None),
        "after 4: att(A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 4: sb(A).status"
    );
    assert!(!case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 4: active(build:A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    let before = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
    );
    case.accept(W1, &a).await;
    let after = (
        case.assignments(&a).await,
        case.attempts(A).await,
        case.sb(A).await,
    );
    assert_eq!(
        after, before,
        "after 5: dj(build:A), att(A), sb(A) unchanged"
    );
    assert_eq!(case.signals(W1), vec![], "after 5: signals(W1)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c02_a_second_request_on_a_free_slot_gets_nothing_and_leaves_an_idle_slot() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.check("step 1").await;

    let first = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        first.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 2: result.job_id"
    );
    case.check("step 2").await;

    let second = case.request_job(W1, JobKind::Build).await;
    assert!(second.is_none(), "after 3: result == nothing");
    assert_eq!(
        case.assignments(&a).await.len(),
        1,
        "after 3: dj(build:A) rows"
    );
    assert!(
        case.idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots contains (W1, Build)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn c03_two_workers_ask_for_the_only_candidate_and_the_loser_idles() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    for worker in [W1, W2] {
        case.offers_full(worker).await;
    }
    for worker in [W1, W2] {
        case.score_all(worker).await;
    }
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    assert_eq!(
        case.signals(W2),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W2)"
    );
    case.check("step 1").await;

    let won = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        won.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 2: result.job_id"
    );
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 2: open(build:A)");
    assert_eq!(open.worker_id, W1, "after 2: open(build:A).worker_id");
    case.check("step 2").await;

    let lost = case.request_job(W2, JobKind::Build).await;
    assert!(lost.is_none(), "after 3: result == nothing");
    assert_eq!(
        case.assignments(&a).await.len(),
        1,
        "after 3: dj(build:A) rows"
    );
    assert!(
        case.idle_slots()
            .await
            .contains(&(W2.into(), SlotKind::Build)),
        "after 3: idle_slots contains (W2, Build)"
    );
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn c04_a_row_of_a_second_instance_makes_the_local_claim_lose() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_all(), &[]).await;
    second.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    second.resync_startable_set().await;
    case.offers_full(W1).await;
    second.offers_full(W2).await;
    case.score_all(W1).await;
    second.score_all(W2).await;
    case.check("step 1").await;
    second.check("step 1").await;

    let a1 = second
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:A")
        .assignment_id();
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 2: open(build:A)");
    assert_eq!(open.worker_id, W2, "after 2: open(build:A).worker_id");
    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.dispatched_job))
        .collect();
    assert_eq!(
        attempts,
        vec![(AttemptOutcome::Running, Some(a1))],
        "after 2: att(A)"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    let attempts_before = case.attempts(A).await;
    let lost = case.request_job(W1, JobKind::Build).await;
    assert!(lost.is_none(), "after 3: result == nothing");
    let rows = case.assignments(&a).await;
    assert!(
        matches!(rows.as_slice(), [row] if row.worker_id == W2),
        "after 3: dj(build:A) has a single row of W2, got {rows:?}"
    );
    assert!(!case.pending(&a).await, "after 3: I1.pending(build:A)");
    assert!(!case.tracked(&a).await, "after 3: I1.tracked(build:A)");
    assert_eq!(
        case.attempts(A).await,
        attempts_before,
        "after 3: att(A) unchanged"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    case.check("step 3").await;
    second.check("step 3").await;

    case.resync_startable_set().await;
    assert!(!case.pending(&a).await, "after 4: I1.pending(build:A)");
    case.check("step 4").await;
    second.check("step 4").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn c05_a_lost_claim_moves_on_to_the_next_candidate() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    let second = case.second_instance().await;
    case.connect(W1, w_all(), &[]).await;
    second.connect(W2, w_all(), &[]).await;
    let (x, y) = (case.key(A).await, case.key(B).await);
    let scores = [(x.as_str(), 0, 0, false), (y.as_str(), 5, 5_000_000, false)];

    case.admit_startable_moves().await;
    second.resync_startable_set().await;
    case.offers_full(W1).await;
    second.offers_full(W2).await;
    case.scores(W1, &scores).await;
    second.scores(W2, &scores).await;
    case.check("step 1").await;
    second.check("step 1").await;

    let taken = second.request_job(W2, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(x.clone()),
        "after 2: W2 takes the better scored X"
    );
    let open = case.open_assignment(&x).await.expect("after 2: open(X)");
    assert_eq!(open.worker_id, W2, "after 2: open(X).worker_id");
    case.check("step 2").await;
    second.check("step 2").await;

    let moved = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        moved.as_ref().map(|j| j.job_id().to_owned()),
        Some(y.clone()),
        "after 3: result.job_id == Y"
    );
    let open = case.open_assignment(&y).await.expect("after 3: open(Y)");
    assert_eq!(open.worker_id, W1, "after 3: open(Y).worker_id");
    assert_eq!(case.assignments(&x).await.len(), 1, "after 3: dj(X) rows");
    assert!(!case.pending(&x).await, "after 3: I1.pending(X)");
    assert!(!case.pending(&y).await, "after 3: I1.pending(Y)");
    for path in [A, B] {
        let outcomes: Vec<_> = case
            .attempts(path)
            .await
            .into_iter()
            .map(|t| t.outcome)
            .collect();
        assert_eq!(
            outcomes,
            vec![AttemptOutcome::Running],
            "after 3: att({path})"
        );
    }
    case.check("step 3").await;
    second.check("step 3").await;

    second.finish().await;
    case.finish().await;
}
