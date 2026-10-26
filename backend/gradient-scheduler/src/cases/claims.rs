/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::dispatched_job::{DispatchedJobKind, DispatchedJobOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId};
use gradient_fixtures::shape::shape;
use gradient_graph::{NarCommit, Transition};
use gradient_pool::session_port::SessionSignal;
use gradient_wire::types::{
    BuildFailureKind, BuildMetrics, BuildRequirement, BuildSpec, BuildSpecKind, FlakeSource,
    FlakeStep, Job, JobKind, JobPhase,
};

use super::{
    A, A_OUT, ARM, B, B_OUT, C, C_OUT, CaseShapes, D, D_OUT, REPOSITORY, SchedulerCase, SlotKind,
    X86, ago, build_output, drv_nar, drv_path, eval_key, failure, from_now, is_recent, job_ids,
    narinfo, output_nar, span, store_path, w_all,
};
use crate::actor::Offer;
use crate::jobs::Assignment;

const W1: &str = "w1";
const W2: &str = "w2";

#[tokio::test]
async fn c01_a_won_claim_opens_the_row_and_leaves_the_build_queued_until_the_start() {
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
    let spec = &build.build;
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
    assert_eq!(case.attempts(A).await, vec![], "after 4: att(A) == []");
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

    case.report_building(W1, &a).await;

    let attempts = case.attempts(A).await;
    let [attempt] = attempts.as_slice() else {
        panic!("after 6: att(A) has a single row, got {attempts:?}");
    };
    assert_eq!(
        (
            attempt.outcome,
            attempt.dispatched_job,
            attempt.substitute,
            attempt.build_started_at.is_some(),
        ),
        (AttemptOutcome::Running, Some(a1), false, true),
        "after 6: att(A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 6: sb(A).status"
    );
    case.check("step 6").await;

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

    second
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:A");
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 2: open(build:A)");
    assert_eq!(open.worker_id, W2, "after 2: open(build:A).worker_id");
    assert_eq!(case.attempts(A).await, vec![], "after 2: att(A) == []");
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
        let attempts = case.attempts(path).await;
        assert_eq!(attempts, vec![], "after 3: att({path}) == []");
    }
    case.check("step 3").await;
    second.check("step 3").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn c06_three_lost_claims_end_the_request_and_the_next_request_gets_the_fourth() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .one(e1, A, A_OUT)
            .startable(B, B_OUT)
            .startable(C, C_OUT)
            .startable(D, D_OUT),
    )
    .await;
    let second = case.second_instance().await;
    case.connect(W1, w_all(), &[]).await;
    second.connect(W2, w_all().slots(3), &[]).await;
    let lost_keys = vec![case.key(A).await, case.key(B).await, case.key(C).await];
    let remaining = case.key(D).await;
    let all_keys = [lost_keys.clone(), vec![remaining.clone()]].concat();
    let mut scores: Vec<(&str, u32, u64, bool)> = lost_keys
        .iter()
        .map(|k| (k.as_str(), 0, 0, false))
        .collect();
    scores.push((remaining.as_str(), 5, 5_000_000, false));

    case.admit_startable_moves().await;
    second.resync_startable_set().await;
    case.offers_full(W1).await;
    second.offers_full(W2).await;
    case.scores(W1, &scores).await;
    second.scores(W2, &scores).await;
    case.check("step 1").await;
    second.check("step 1").await;

    for _ in 0..3 {
        second
            .request_job(W2, JobKind::Build)
            .await
            .expect("after 2: W2 gets a better scored job");
    }
    let expected_w2: Vec<(String, String)> =
        lost_keys.iter().map(|k| (k.clone(), W2.into())).collect();
    assert_eq!(
        open_rows(&case, &all_keys).await,
        expected_w2,
        "after 2: open rows on W2"
    );
    assert!(
        !second
            .idle_slots()
            .await
            .contains(&(W2.into(), SlotKind::Build)),
        "after 2: I2.idle_slots does not contain (W2, Build)"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    let lost = case.request_job(W1, JobKind::Build).await;
    assert!(lost.is_none(), "after 3: result == nothing");
    assert_eq!(
        open_rows(&case, &all_keys).await,
        expected_w2,
        "after 3: open rows over all four keys"
    );
    assert!(
        case.pending(&remaining).await,
        "after 3: I1.pending(remaining)"
    );
    for key in &lost_keys {
        assert!(!case.pending(key).await, "after 3: I1.pending({key})");
    }
    assert!(
        !case
            .idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots does not contain (W1, Build)"
    );
    case.check("step 3").await;
    second.check("step 3").await;

    let moved = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken(moved.as_ref()),
        Some(remaining.clone()),
        "after 4: result.job_id == remaining key"
    );
    let open = case
        .open_assignment(&remaining)
        .await
        .expect("after 4: open(remaining)");
    assert_eq!(open.worker_id, W1, "after 4: open(remaining).worker_id");
    case.check("step 4").await;
    second.check("step 4").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn c07_a_build_that_left_queued_before_the_claim_hands_out_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&a).await, "after 1: pending(build:A)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.demote_path(A).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 0],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert_eq!(case.assignments(&a).await, vec![], "after 3: dj(build:A)");
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A)");
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    assert!(
        case.idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots contains (W1, Build)"
    );
    case.check("step 3").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn c08_an_upstream_hit_before_the_claim_fails_the_build_claim_and_offers_a_substitute() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let pending = case
        .pending_build_job(&a)
        .await
        .expect("after 1: pending(build:A)");
    assert_eq!(
        pending.job.build.kind,
        BuildSpecKind::Build,
        "after 1: pending kind"
    );
    case.check("step 1").await;

    case.upstream_hits(&[(A_OUT, narinfo(&[]))]).await;
    let build = case.sb(A).await;
    assert_eq!(
        (build.cache_available, build.status),
        (true, BuildStatus::Queued),
        "after 2: sb(A) cache_available and status"
    );
    assert!(
        case.output_of(A, "out").await.external_url.is_some(),
        "after 2: output(A, out).external_url set"
    );
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert_eq!(case.assignments(&a).await, vec![], "after 3: dj(build:A)");
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A)");
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.resync_startable_set().await;
    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(job_ids(&offer), vec![a.clone()], "after 4: candidates");
    case.check("step 4").await;

    let substitute = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 5: W1 gets build:A");
    assert_eq!(
        build_spec(&substitute).kind,
        BuildSpecKind::Substitute,
        "after 5: result.job.kind"
    );
    assert!(
        substitute.pending.required_paths().is_empty(),
        "after 5: result.job.required_paths"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 5: att(A) == []");
    assert!(
        case.open_assignment(&a).await.is_some(),
        "after 5: open(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 5: sb(A).status"
    );
    case.check("step 5").await;

    case.report_building(W1, &a).await;

    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.substitute))
        .collect();
    assert_eq!(
        attempts,
        vec![(AttemptOutcome::Running, true)],
        "after 6: att(A)"
    );
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn c09_a_build_aborted_in_the_graph_before_the_claim_is_not_handed_out() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.signals(W1);
    case.check("step 1").await;

    let shared_build = case.sb(A).await.id;
    case.transition(Transition::AbortBuild {
        evaluation: e1,
        shared_build,
    })
    .await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert!(
        case.job(e1, A)
            .await
            .expect("after 2: build_job(E1, A)")
            .aborted,
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
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert_eq!(case.assignments(&a).await, vec![], "after 3: dj(build:A)");
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A)");
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn c10_an_eval_claim_after_an_abort_on_the_second_instance_writes_no_row() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&key).await, "after 1: I1.pending(eval:E1)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;
    second.check("step 1").await;

    second.abort_evaluation(e1).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        evaluation.status,
        EvaluationStatus::Aborted,
        "after 2: evaluation(E1).status"
    );
    assert!(
        evaluation.finished_at.is_some(),
        "after 2: evaluation(E1).finished_at set"
    );
    case.check("step 2").await;
    second.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Flake).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert_eq!(case.assignments(&key).await, vec![], "after 3: dj(eval:E1)");
    assert!(!case.pending(&key).await, "after 3: I1.pending(eval:E1)");
    case.check("step 3").await;
    second.check("step 3").await;

    case.assign_queued_evals().await;
    assert!(!case.pending(&key).await, "after 4: I1.pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;
    second.check("step 4").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn c11_a_won_eval_claim_writes_the_row_and_leaves_the_evaluation_queued() {
    let e1 = EvaluationId::now_v7();
    let case =
        SchedulerCase::start(shape().evalq(e1).commit_hash(&[0xc1; 20]).wildcard("pkg.*")).await;
    case.connect(W1, w_all(), &[]).await;
    let key = eval_key(e1);

    case.assign_queued_evals().await;
    let offer = case.offers_full(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&key).await, "after 1: pending(eval:E1)");
    assert_eq!(
        offered(&offer),
        vec![(key.clone(), None)],
        "after 1: candidates"
    );
    case.check("step 1").await;

    let assignment = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 2: W1 gets eval:E1");
    let a1 = assignment.assignment_id();
    assert_eq!(assignment.job_id(), key, "after 2: result.job_id");
    let Job::Flake(flake) = &assignment.job else {
        panic!("after 2: a flake job, got {:?}", assignment.job);
    };
    assert_eq!(
        (
            flake.steps.clone(),
            flake.source.clone(),
            flake.wildcards.clone(),
            flake.input_overrides.clone(),
            flake.input_update.clone(),
        ),
        (
            vec![
                FlakeStep::FetchFlake,
                FlakeStep::EvaluateFlake,
                FlakeStep::EvaluateDerivations,
            ],
            FlakeSource::Repository {
                url: REPOSITORY.into(),
                commit: "c1".repeat(20),
            },
            vec!["pkg.*".to_owned()],
            Vec::new(),
            None,
        ),
        "after 2: result.job"
    );
    let rows = case.assignments(&key).await;
    let [row] = rows.as_slice() else {
        panic!("after 2: dj(eval:E1) has a single row, got {rows:?}");
    };
    assert_eq!(
        (
            row.id,
            row.kind,
            row.evaluation_id,
            row.project,
            row.task,
            row.worker_id.as_str(),
            row.outcome,
        ),
        (
            a1,
            DispatchedJobKind::Eval,
            e1,
            case.db.project_of(e1).await,
            Some(case.db.task_of(e1).await),
            W1,
            None,
        ),
        "after 2: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.fetch_started_at),
        (EvaluationStatus::Queued, None),
        "after 2: evaluation(E1) status and fetch_started_at"
    );
    case.check("step 2").await;

    case.report_status(W1, &key, EvaluationStatus::Fetching)
        .await;
    let reported = case.evaluation(e1).await;
    assert_eq!(
        reported.status,
        EvaluationStatus::Fetching,
        "after 3: evaluation(E1).status"
    );
    assert!(
        reported.fetch_started_at.is_some(),
        "after 3: fetch_started_at set"
    );
    assert!(
        reported.updated_at > evaluation.updated_at,
        "after 3: updated_at moved"
    );
    assert_eq!(reported.waiting_reason, None, "after 3: waiting_reason");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn c12_a_reject_closes_the_row_and_returns_the_job_to_every_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
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
    case.check("step 1").await;

    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 2: W1 gets build:A")
        .assignment_id();
    case.check("step 2").await;

    let offer = case.offers_delta(W2).await;
    assert_eq!(job_ids(&offer), Vec::<String>::new(), "after 3: candidates");
    case.check("step 3").await;

    case.reject(W1, &a).await;
    let rows = case.assignments(&a).await;
    let [row] = rows.as_slice() else {
        panic!("after 4: dj(build:A) has a single row, got {rows:?}");
    };
    assert_eq!(
        (row.id, row.outcome),
        (a1, Some(DispatchedJobOutcome::Abandoned)),
        "after 4: dj(build:A)"
    );
    assert!(row.finished_at.is_some(), "after 4: finished_at set");
    assert_eq!(case.attempts(A).await, vec![], "after 4: att(A) == []");
    assert!(case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(case.active(&a).await, None, "after 4: active(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 4: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    let offer = case.offers_delta(W2).await;
    case.score_all(W2).await;
    assert_eq!(job_ids(&offer), vec![a.clone()], "after 5: candidates");
    case.check("step 5").await;

    let retaken = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 6: W2 gets build:A");
    let a2 = retaken.assignment_id();
    assert_eq!(retaken.job_id(), a, "after 6: result.job_id");
    assert_ne!(a2, a1, "after 6: a2 != a1");
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome, r.worker_id))
        .collect();
    assert_eq!(
        rows,
        vec![
            (a1, Some(DispatchedJobOutcome::Abandoned), W1.to_owned()),
            (a2, None, W2.to_owned()),
        ],
        "after 6: dj(build:A)"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 6: att(A) == []");
    case.check("step 6").await;

    case.report_building(W2, &a).await;

    let running: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .filter(|t| t.outcome == AttemptOutcome::Running)
        .map(|t| t.dispatched_job)
        .collect();
    assert_eq!(running, vec![Some(a2)], "after 7: Running rows of att(A)");
    case.check("step 7").await;

    case.finish().await;
}

#[tokio::test]
async fn c13_a_claim_whose_assigned_transition_fails_closes_its_row_and_keeps_the_job_pending() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    case.check("step 1").await;

    case.graph_writer_down().await;
    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 2: result == nothing");
    let rows = case.assignments(&a).await;
    let [row] = rows.as_slice() else {
        panic!("after 2: dj(build:A) has a single row, got {rows:?}");
    };
    let a1 = row.id;
    assert_eq!(
        row.outcome,
        Some(DispatchedJobOutcome::Abandoned),
        "after 2: dj(build:A).outcome"
    );
    assert!(row.finished_at.is_some(), "after 2: finished_at set");
    assert_eq!(case.attempts(A).await, vec![], "after 2: att(A)");
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    case.graph_writer_up().await;
    case.check("step 2").await;

    case.offers_delta(W1).await;
    case.score_all(W1).await;
    let retaken = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 3: W1 gets build:A");
    let a2 = retaken.assignment_id();
    assert_eq!(retaken.job_id(), a, "after 3: result.job_id");
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 3: dj(build:A)"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 3: att(A) == []");
    case.check("step 3").await;

    case.report_building(W1, &a).await;

    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.dispatched_job))
        .collect();
    assert_eq!(
        attempts,
        vec![(AttemptOutcome::Running, Some(a2))],
        "after 4: att(A)"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn c14_admission_enqueues_a_build_that_left_and_reentered_queued_once() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    assert!(case.pending(&a).await, "after 1: pending(build:A)");
    assert!(case.pending(&b).await, "after 1: pending(build:B)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1), SessionSignal::Offers(2)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    case.demote_path(B).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Created,
        "after 2: sb(B).status"
    );
    case.check("step 2").await;

    case.commit_nar(drv_nar(B)).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 3: sb(B).status"
    );
    case.check("step 3").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&b).await, "after 4: pending(build:B)");
    assert!(case.pending(&a).await, "after 4: pending(build:A)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(3)],
        "after 4: signals(W1)"
    );
    case.check("step 4").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(job_ids(&offer), vec![b], "after 5: candidates");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c15_a_resync_drops_unstartable_builds_and_enqueues_startable_ones_again() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).evalq(e2)).await;
    let (a, eval) = (case.key(A).await, eval_key(e2));

    case.assign_queued_evals().await;
    case.admit_startable_moves().await;
    assert!(case.pending(&eval).await, "after 1: pending(eval:E2)");
    assert!(case.pending(&a).await, "after 1: pending(build:A)");
    case.check("step 1").await;

    case.demote_path(A).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 2: sb(A).status"
    );
    case.check("step 2").await;

    case.resync_startable_set().await;
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    assert!(case.pending(&eval).await, "after 3: pending(eval:E2)");
    assert_eq!(case.totals().await.pending, 1, "after 3: pending jobs");
    case.check("step 3").await;

    case.commit_nar(drv_nar(A)).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 4: sb(A).status"
    );
    case.check("step 4").await;

    case.resync_startable_set().await;
    assert!(case.pending(&a).await, "after 5: pending(build:A)");
    assert!(case.pending(&eval).await, "after 5: pending(eval:E2)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c16_a_resync_skips_a_build_with_an_open_row_until_the_sweep_frees_it() {
    let case = SchedulerCase::start(
        shape()
            .one(EvaluationId::now_v7(), A, A_OUT)
            .open_assignment(),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;
    let attempts_before = case.attempts(A).await;

    case.resync_startable_set().await;
    assert!(!case.pending(&a).await, "after 1: pending(build:A)");
    assert!(!case.tracked(&a).await, "after 1: tracked(build:A)");
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 1: open(build:A)");
    assert_eq!(open.worker_id, "worker", "after 1: open(build:A).worker_id");
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    case.check("step 1").await;

    case.abandoned_dispatch_sweep().await;
    let rows = case.assignments(&a).await;
    let [row] = rows.as_slice() else {
        panic!("after 2: dj(build:A) has a single row, got {rows:?}");
    };
    assert_eq!(
        row.outcome,
        Some(DispatchedJobOutcome::Abandoned),
        "after 2: dj(build:A).outcome"
    );
    assert!(
        row.finished_at.is_some_and(is_recent),
        "after 2: finished_at ~now"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.attempts(A).await,
        attempts_before,
        "after 2: att(A) unchanged"
    );
    case.check("step 2").await;

    case.resync_startable_set().await;
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Offers(_)]),
        "after 3: signals(W1), got {signals:?}"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn c17_a_server_drain_buffers_startable_moves_until_it_ends() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.drain_server(true).await;
    case.check("step 1").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert_eq!(case.assignments(&a).await, vec![], "after 3: dj(build:A)");
    case.check("step 3").await;

    case.drain_server(false).await;
    case.check("step 4").await;

    case.admit_startable_moves().await;
    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    assert!(case.pending(&a).await, "after 5: pending(build:A)");
    assert_eq!(job_ids(&offer), vec![a.clone()], "after 5: candidates");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 5: signals(W1)"
    );
    case.check("step 5").await;

    let taken_job = case.request_job(W1, JobKind::Build).await;
    assert_eq!(taken(taken_job.as_ref()), Some(a), "after 6: result.job_id");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn c18_a_draining_worker_gets_no_offers_no_job_and_no_idle_slot() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    let a = case.key(A).await;

    case.draining(W1).await;
    case.check("step 1").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    assert_eq!(
        case.signals(W2),
        vec![SessionSignal::Offers(1)],
        "after 2: signals(W2)"
    );
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert!(
        !case
            .idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots does not contain (W1, Build)"
    );
    assert_eq!(case.assignments(&a).await, vec![], "after 3: dj(build:A)");
    case.check("step 3").await;

    case.offers_full(W2).await;
    case.score_all(W2).await;
    let taken_job = case.request_job(W2, JobKind::Build).await;
    assert_eq!(taken(taken_job.as_ref()), Some(a), "after 4: result.job_id");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn c19_a_worker_with_full_build_slots_is_refused_until_the_completion() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all().slots(1), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.scores(W1, &a_before_b(&a, &b)).await;
    case.check("step 1").await;

    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 2: W1 gets build:A")
        .assignment_id();
    case.check("step 2").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 3: result == nothing");
    assert!(
        !case
            .idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 3: idle_slots does not contain (W1, Build)"
    );
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    case.check("step 3").await;

    case.report_building(W1, &a).await;
    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, vec![span(JobPhase::Build, 0, 1000)], 1000)
        .await;
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome, r.worker_elapsed_ms))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Completed), Some(1000))],
        "after 4: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 4: sb(A).status"
    );
    case.check("step 4").await;

    let next = case.request_job(W1, JobKind::Build).await;
    assert_eq!(taken(next.as_ref()), Some(b), "after 5: result.job_id");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c20_an_abort_pending_job_keeps_its_slot_until_the_worker_confirms() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().two(e1)).await;
    case.connect(W1, w_all().slots(1), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.scores(W1, &a_before_b(&a, &b)).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.signals(W1);
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    case.check("step 2").await;

    assert!(
        case.abort_build(e1, A).await.is_ok(),
        "after 3: abort_build(E1, A) succeeds"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 3: active(build:A)"
    );
    assert!(
        case.open_assignment(&a).await.is_some(),
        "after 3: open(build:A)"
    );
    let signals = case.signals(W1);
    assert!(
        matches!(signals.as_slice(), [SessionSignal::Abort { job_id, .. }] if *job_id == a),
        "after 3: signals(W1), got {signals:?}"
    );
    case.check("step 3").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 4: result == nothing");
    assert!(case.pending(&b).await, "after 4: pending(build:B)");
    case.check("step 4").await;

    case.fail(
        W1,
        &a,
        a1,
        failure(BuildFailureKind::Aborted)
            .error("aborted by server")
            .spans(vec![span(JobPhase::Build, 0, 10)])
            .elapsed_ms(10),
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
        "after 5: dj(build:A)"
    );
    assert_eq!(case.active(&a).await, None, "after 5: active(build:A)");
    assert_eq!(
        case.attempts(A).await.first().map(|t| t.outcome),
        Some(AttemptOutcome::Aborted),
        "after 5: att(A)[0].outcome"
    );
    case.check("step 5").await;

    let next = case.request_job(W1, JobKind::Build).await;
    assert_eq!(taken(next.as_ref()), Some(b), "after 6: result.job_id");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn c21_a_project_restricted_worker_gets_only_its_project_until_the_set_widens() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).one(e2, B, B_OUT)).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    case.connect(W1, w_all().slots(2), &[p1]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.check("step 1").await;

    let offer = case.offers_full(W1).await;
    case.score_all(W1).await;
    assert_eq!(job_ids(&offer), vec![a.clone()], "after 2: candidates");
    case.check("step 2").await;

    let first = case.request_job(W1, JobKind::Build).await;
    assert_eq!(taken(first.as_ref()), Some(a), "after 3: result.job_id");
    case.check("step 3").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 4: result == nothing");
    assert!(case.pending(&b).await, "after 4: pending(build:B)");
    case.check("step 4").await;

    case.authorize(W1, &[]).await;
    case.check("step 5").await;

    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    let widened = case.request_job(W1, JobKind::Build).await;
    assert_eq!(job_ids(&offer), vec![b.clone()], "after 6: candidates");
    assert_eq!(
        taken(widened.as_ref()),
        Some(b.clone()),
        "after 6: result.job_id"
    );
    let open = case
        .open_assignment(&b)
        .await
        .expect("after 6: open(build:B)");
    assert_eq!(open.project, p2, "after 6: open(build:B).project");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn c22_a_build_needing_a_feature_or_system_waits_for_matching_capabilities() {
    let case = SchedulerCase::start(
        shape()
            .one(EvaluationId::now_v7(), A, A_OUT)
            .feature("kvm")
            .startable(B, B_OUT)
            .system(ARM),
    )
    .await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    case.score_all(W1).await;
    assert_eq!(offered(&offer), vec![], "after 1: candidates");
    case.check("step 1").await;

    let refused = case.request_job(W1, JobKind::Build).await;
    assert!(refused.is_none(), "after 2: result == nothing");
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert!(case.pending(&b).await, "after 2: pending(build:B)");
    case.check("step 2").await;

    case.capabilities(W1, w_all().features(&["kvm"]).slots(2))
        .await;
    case.check("step 3").await;

    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    let with_kvm = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        offered(&offer),
        vec![(a.clone(), requirement(X86, &["kvm"]))],
        "after 4: candidates"
    );
    assert_eq!(taken(with_kvm.as_ref()), Some(a), "after 4: result.job_id");
    case.check("step 4").await;

    case.capabilities(W1, w_all().arch(&[X86, ARM]).features(&["kvm"]).slots(2))
        .await;
    case.check("step 5").await;

    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    let with_arm = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        offered(&offer),
        vec![(b.clone(), requirement(ARM, &[]))],
        "after 6: candidates"
    );
    assert_eq!(taken(with_arm.as_ref()), Some(b), "after 6: result.job_id");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn c23_offer_generations_rise_per_enqueue_and_deltas_do_not_repeat_a_candidate() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(1)],
        "after 1: signals(W1)"
    );
    case.check("step 1").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        (offer.generation, job_ids(&offer)),
        (1, vec![b.clone()]),
        "after 2: result"
    );
    case.check("step 2").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        (offer.generation, job_ids(&offer)),
        (1, Vec::new()),
        "after 3: result"
    );
    case.check("step 3").await;

    case.commit_nar(output_nar(B, B_OUT)).await;
    let shared_build = case.sb(B).await.id;
    case.transition(Transition::BuildStarted {
        shared_build,
        dispatched_job: DispatchedJobId::now_v7(),
        substitute: false,
        build_context: serde_json::json!({}),
    })
    .await;
    case.transition(Transition::BuildCompleted { shared_build })
        .await;
    let build_b = case.sb(B).await;
    assert_eq!(
        (build_b.status, build_b.fetchable),
        (BuildStatus::Completed, true),
        "after 4: sb(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 4: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 5: pending(build:A)");
    assert!(!case.pending(&b).await, "after 5: pending(build:B)");
    assert_eq!(
        case.signals(W1),
        vec![SessionSignal::Offers(2)],
        "after 5: signals(W1)"
    );
    case.check("step 5").await;

    let offer = case.offers_delta(W1).await;
    assert_eq!(
        (offer.generation, job_ids(&offer)),
        (2, vec![a.clone()]),
        "after 6: result"
    );
    case.check("step 6").await;

    let offer = case.offers_full(W1).await;
    assert_eq!(job_ids(&offer), vec![a], "after 7: candidates");
    case.check("step 7").await;

    case.finish().await;
}

#[tokio::test]
async fn c24_a_capability_update_keeps_running_jobs_and_offers_new_jobs_as_a_delta() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7()).system(ARM)).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let a1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A")
        .assignment_id();
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    let open_before = case.open_assignment(&a).await;
    case.capabilities(W1, w_all().arch(&[X86, ARM]).slots(2))
        .await;
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W1),
        "after 2: active(build:A)"
    );
    assert_eq!(
        case.open_assignment(&a).await,
        open_before,
        "after 2: open(build:A) unchanged"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    case.check("step 2").await;

    let offer = case.offers_delta(W1).await;
    case.score_all(W1).await;
    assert_eq!(job_ids(&offer), vec![b.clone()], "after 3: candidates");
    case.check("step 3").await;

    let second_job = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken(second_job.as_ref()),
        Some(b),
        "after 4: result.job_id"
    );
    case.check("step 4").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 5: sb(A).status"
    );
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 5: dj(build:A)"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c25_a_passthrough_is_a_substitute_job_and_finishes_substituted_without_a_metric() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().passthrough(e1)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.score_all(W1).await;
    let substitute = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:A");
    let a1 = substitute.assignment_id();
    assert_eq!(
        build_spec(&substitute).kind,
        BuildSpecKind::Substitute,
        "after 1: result.job.kind"
    );
    assert!(
        substitute.pending.required_paths().is_empty(),
        "after 1: result.job.required_paths"
    );
    assert_eq!(case.attempts(A).await, vec![], "after 1: att(A) == []");
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 2: sb(A).status"
    );
    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.substitute))
        .collect();
    assert_eq!(
        attempts,
        vec![(AttemptOutcome::Running, true)],
        "after 2: att(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 1],
        "after 2: counters(E1)"
    );
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
    assert!(
        case.derivation_metrics(A).await.is_empty(),
        "after 3: derivation_metric of A"
    );
    case.check("step 3").await;

    case.commit_nar(NarCommit {
        targets: case.project_caches(e1).await,
        ..output_nar(A, A_OUT)
    })
    .await;
    case.check("step 4").await;

    case.complete(
        W1,
        &a,
        a1,
        vec![
            span(JobPhase::SubstituteFetch, 0, 200),
            span(JobPhase::NarPush, 200, 300),
        ],
        300,
    )
    .await;
    let build = case.sb(A).await;
    assert_eq!(
        (build.status, build.fetchable),
        (BuildStatus::Substituted, true),
        "after 5: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 5: evaluation(E1).status"
    );
    let rows: Vec<_> = case
        .assignments(&a)
        .await
        .into_iter()
        .map(|r| (r.id, r.outcome))
        .collect();
    assert_eq!(
        rows,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 5: dj(build:A)"
    );
    assert_eq!(
        case.attempts(A).await.first().map(|t| t.outcome),
        Some(AttemptOutcome::Substituted),
        "after 5: att(A)[0].outcome"
    );
    let phases: Vec<i16> = case.phases(a1).await.into_iter().map(|p| p.phase).collect();
    assert_eq!(
        phases,
        vec![
            JobPhase::SubstituteFetch.as_i16(),
            JobPhase::NarPush.as_i16()
        ],
        "after 5: dispatched_job_phase(a1)"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn c26_a_builtin_fixed_output_derivation_is_a_download_job_for_any_architecture() {
    let case = SchedulerCase::start(
        shape()
            .one(EvaluationId::now_v7(), A, A_OUT)
            .system("builtin")
            .fixed_output(),
    )
    .await;
    case.connect(W1, w_all().arch(&[ARM]), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    let offer = case.offers_full(W1).await;
    case.score_all(W1).await;
    assert_eq!(
        offered(&offer),
        vec![(a.clone(), requirement("builtin", &[]))],
        "after 1: candidates"
    );
    case.check("step 1").await;

    let download = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 2: W1 gets build:A");
    let spec = build_spec(&download);
    assert_eq!(
        (spec.kind, spec.is_fixed_output),
        (BuildSpecKind::Download, true),
        "after 2: result.job kind and is_fixed_output"
    );
    let open = case
        .open_assignment(&a)
        .await
        .expect("after 2: open(build:A)");
    assert_eq!(open.worker_id, W1, "after 2: open(build:A).worker_id");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn c27_connecting_closes_only_rows_dispatched_before_the_scheduler_started() {
    let case = SchedulerCase::start(
        shape()
            .one(EvaluationId::now_v7(), A, A_OUT)
            .open_assignment()
            .by_worker(W1)
            .dispatched_at(ago(3600))
            .startable(B, B_OUT)
            .open_assignment()
            .by_worker(W1)
            .dispatched_at(from_now(60)),
    )
    .await;
    case.connect(W2, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.connect(W1, w_all(), &[]).await;
    let rows_a = case.assignments(&a).await;
    let [row_a] = rows_a.as_slice() else {
        panic!("after 1: dj(build:A) has a single row, got {rows_a:?}");
    };
    assert_eq!(
        row_a.outcome,
        Some(DispatchedJobOutcome::Abandoned),
        "after 1: dj(build:A).outcome"
    );
    assert!(
        row_a.finished_at.is_some_and(is_recent),
        "after 1: dj(build:A).finished_at ~now"
    );
    let outcomes_b: Vec<_> = case
        .assignments(&b)
        .await
        .into_iter()
        .map(|r| r.outcome)
        .collect();
    assert_eq!(outcomes_b, vec![None], "after 1: dj(build:B)");
    let connections = case.connections(W1).await;
    let [connection] = connections.as_slice() else {
        panic!("after 1: worker_connection(W1) has a single row, got {connections:?}");
    };
    assert!(
        is_recent(connection.connected_at),
        "after 1: worker_connection(W1).connected_at ~now"
    );
    assert_eq!(
        connection.disconnected_at, None,
        "after 1: worker_connection(W1).disconnected_at"
    );
    assert_eq!(
        (case.sb(A).await.status, case.sb(B).await.status),
        (BuildStatus::Queued, BuildStatus::Queued),
        "after 1: sb(A).status and sb(B).status"
    );
    assert_eq!(case.signals(W1), vec![], "after 1: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 1: signals(W2)");
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn c28_a_registration_on_a_second_instance_leaves_rows_of_the_first_claim_open() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    let second = case.second_instance().await;
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
    case.check("step 1").await;
    second.check("step 1").await;

    let open_before = case.open_assignment(&a).await;
    second.connect(W1, w_all(), &[]).await;
    assert_eq!(
        case.open_assignment(&a).await,
        open_before,
        "after 2: open(build:A) unchanged"
    );
    let open_connections = case
        .connections(W1)
        .await
        .into_iter()
        .filter(|c| c.disconnected_at.is_none())
        .count();
    assert_eq!(open_connections, 2, "after 2: open worker_connection(W1)");
    case.check("step 2").await;
    second.check("step 2").await;

    case.report_building(W1, &a).await;
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
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    case.check("step 3").await;
    second.check("step 3").await;

    second.finish().await;
    case.finish().await;
}

fn offered(offer: &Offer) -> Vec<(String, Option<BuildRequirement>)> {
    offer
        .candidates
        .iter()
        .map(|c| (c.job_id.clone(), c.requirement.clone()))
        .collect()
}

fn requirement(architecture: &str, features: &[&str]) -> Option<BuildRequirement> {
    Some(BuildRequirement {
        architecture: architecture.into(),
        required_features: features.iter().map(|f| (*f).into()).collect(),
    })
}

fn taken(assignment: Option<&Assignment>) -> Option<String> {
    assignment.map(|a| a.job_id().to_owned())
}

fn build_spec(assignment: &Assignment) -> &BuildSpec {
    let Job::Build(build) = &assignment.job else {
        panic!("a build job, got {:?}", assignment.job);
    };
    &build.build
}

fn a_before_b<'k>(a: &'k str, b: &'k str) -> [(&'k str, u32, u64, bool); 2] {
    [(a, 0, 0, false), (b, 5, 5_000_000, false)]
}

async fn open_rows(case: &SchedulerCase, keys: &[String]) -> Vec<(String, String)> {
    let mut open = Vec::new();
    for key in keys {
        if let Some(row) = case.open_assignment(key).await {
            open.push((key.clone(), row.worker_id));
        }
    }
    open
}
