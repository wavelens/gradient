/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::ids::EvaluationId;
use gradient_fixtures::shape::shape;
use gradient_pool::score::RamNeed;
use gradient_wire::types::{BuildFailureKind, BuildMetrics, Job, JobKind, JobPhase};

use super::{A, A_OUT, B, B_OUT, CaseShapes, SchedulerCase, failure, span, w_build};
use crate::jobs::Assignment;

const W1: &str = "w1";
const W2: &str = "w2";
const OUT_OF_MEMORY_AT_ONE_CORE: &str = "ran out of memory with 1 core";

async fn start(seed: gradient_fixtures::shape::Shape) -> SchedulerCase {
    SchedulerCase::start_with(seed, |c| c.build.retry_backoff_secs = 0).await
}

async fn dispatch(case: &SchedulerCase, worker: &str) -> Option<Assignment> {
    case.admit_startable_moves().await;
    case.offers_full(worker).await;
    case.score_all(worker).await;
    let taken = case.request_job(worker, JobKind::Build).await?;
    case.report_building(worker, taken.job_id()).await;
    Some(taken)
}

async fn kill(case: &SchedulerCase, worker: &str, taken: &Assignment, cores: Option<u32>) {
    let metrics = BuildMetrics {
        oom_killed: true,
        peak_ram_mb: Some(100_000),
        build_cores: cores,
        ..Default::default()
    };
    case.fail(
        worker,
        taken.job_id(),
        taken.assignment_id(),
        failure(BuildFailureKind::Transient)
            .error("oom")
            .spans(vec![span(JobPhase::Build, 0, 1)])
            .elapsed_ms(1)
            .metrics(metrics),
    )
    .await;
    case.requeue_transient_retries().await;
}

fn sent_cores(taken: &Assignment) -> Option<u32> {
    match &taken.job {
        Job::Build(build) => build.build.build_cores,
        Job::Flake(_) => None,
    }
}

fn reserved_mb(taken: &Assignment) -> Option<u64> {
    match taken.ram_need {
        RamNeed::Predicted(mb) => Some(mb),
        RamNeed::Negligible | RamNeed::Unmeasured => None,
    }
}

async fn last_failure_message(case: &SchedulerCase, path: &str) -> Option<String> {
    case.attempts(path)
        .await
        .into_iter()
        .rev()
        .find_map(|attempt| attempt.failure_message)
}

#[tokio::test]
async fn e01_an_out_of_memory_kill_at_64_cores_retries_at_43_without_spending_an_attempt() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    let first = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    assert_eq!(sent_cores(&first), Some(64), "after 1: cores sent");
    let attempt = case.sb(A).await.attempt;
    case.check("step 1").await;

    kill(&case, W1, &first, Some(64)).await;
    assert_eq!(case.sb(A).await.attempt, attempt, "after 2: attempt");
    case.check("step 2").await;

    let second = dispatch(&case, W1)
        .await
        .expect("after 3: W1 takes A again");
    assert_eq!(sent_cores(&second), Some(43), "after 3: cores sent");
    assert_eq!(case.sb(A).await.attempt, attempt, "after 3: attempt");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn e02_an_out_of_memory_kill_at_one_core_fails_the_build() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(1).ram_total_mb(8_000), &[])
        .await;

    let taken = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    assert_eq!(sent_cores(&taken), Some(1), "after 1: cores sent");
    kill(&case, W1, &taken, Some(1)).await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 2: sb(A).status"
    );
    assert_eq!(
        last_failure_message(&case, A).await.as_deref(),
        Some(OUT_OF_MEMORY_AT_ONE_CORE),
        "after 2: failure message"
    );
    assert!(
        dispatch(&case, W1).await.is_none(),
        "after 3: nothing to take"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn e03_a_build_killed_at_every_step_ends_after_twelve_dispatches() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(1_024_000), &[])
        .await;

    let mut sent = Vec::new();
    while let Some(taken) = dispatch(&case, W1).await {
        assert!(
            sent.len() < 20,
            "after {}: the retries never end",
            sent.len()
        );
        let cores = sent_cores(&taken);
        sent.push(cores.unwrap_or(0));
        kill(&case, W1, &taken, cores).await;
    }

    assert_eq!(
        sent,
        vec![64, 43, 29, 20, 14, 10, 7, 5, 4, 3, 2, 1],
        "after 1: cores sent per dispatch"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedPermanent, 0),
        "after 1: sb(A) status and attempt"
    );
    assert_eq!(
        last_failure_message(&case, A).await.as_deref(),
        Some(OUT_OF_MEMORY_AT_ONE_CORE),
        "after 1: failure message"
    );
    assert_eq!(
        case.derivation_metrics(A).await.len(),
        12,
        "after 1: one metric row per kill"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn e04_a_kill_recorded_through_one_instance_caps_the_dispatch_on_another() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;
    second
        .connect(W2, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    let first = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    kill(&case, W1, &first, Some(64)).await;
    case.check("step 1").await;

    second.resync_startable_set().await;
    let taken = dispatch(&second, W2).await.expect("after 2: W2 takes A");
    assert_eq!(sent_cores(&taken), Some(43), "after 2: cores sent by B");
    second.check("step 2").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn e05_a_build_already_pending_on_another_instance_follows_a_later_kill() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    let second = case.second_instance().await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;
    second
        .connect(W2, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;
    second.resync_startable_set().await;

    let first = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    kill(&case, W1, &first, Some(64)).await;
    case.check("step 1").await;

    second.build_assign_pass(false, true).await;
    let taken = dispatch(&second, W2).await.expect("after 2: W2 takes A");
    assert_eq!(sent_cores(&taken), Some(43), "after 2: cores sent by B");
    second.check("step 2").await;

    second.finish().await;
    case.finish().await;
}

#[tokio::test]
async fn e06_the_steps_start_at_the_configured_core_limit() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(
        W1,
        w_build().cpus(64).core_limit(8).ram_total_mb(256_000),
        &[],
    )
    .await;

    let first = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    assert_eq!(sent_cores(&first), Some(8), "after 1: cores sent");
    kill(&case, W1, &first, Some(8)).await;

    let second = dispatch(&case, W1)
        .await
        .expect("after 2: W1 takes A again");
    assert_eq!(sent_cores(&second), Some(6), "after 2: cores sent");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn e07_the_cap_follows_the_fewest_cores_a_build_was_killed_with() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W2, w_build().cpus(16).ram_total_mb(256_000), &[])
        .await;

    let first = dispatch(&case, W2).await.expect("after 1: W2 takes A");
    assert_eq!(sent_cores(&first), Some(16), "after 1: cores sent");
    kill(&case, W2, &first, Some(16)).await;

    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;
    let second = dispatch(&case, W1).await.expect("after 2: W1 takes A");
    assert_eq!(sent_cores(&second), Some(11), "after 2: cores sent");
    kill(&case, W1, &second, Some(11)).await;

    let third = dispatch(&case, W1)
        .await
        .expect("after 3: W1 takes A again");
    assert_eq!(sent_cores(&third), Some(8), "after 3: cores sent");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn e08_a_kill_without_a_core_count_spends_the_attempt_budget() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    for step in 1..=3 {
        let taken = dispatch(&case, W1)
            .await
            .unwrap_or_else(|| panic!("after {step}: W1 takes A"));
        assert_eq!(sent_cores(&taken), Some(64), "after {step}: cores sent");
        kill(&case, W1, &taken, None).await;
    }

    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedPermanent, 2),
        "after 3: sb(A) status and attempt"
    );
    assert!(
        dispatch(&case, W1).await.is_none(),
        "after 4: nothing to take"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn e09_a_kill_with_fewer_cores_is_free_with_the_budget_almost_spent() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    for step in 1..=2 {
        let taken = dispatch(&case, W1)
            .await
            .unwrap_or_else(|| panic!("after {step}: W1 takes A"));
        case.fail(
            W1,
            taken.job_id(),
            taken.assignment_id(),
            failure(BuildFailureKind::Transient).error("network"),
        )
        .await;
        case.requeue_transient_retries().await;
    }
    assert_eq!(case.sb(A).await.attempt, 2, "after 2: attempt");

    let third = dispatch(&case, W1).await.expect("after 3: W1 takes A");
    kill(&case, W1, &third, Some(64)).await;
    let shared = case.sb(A).await;
    assert_eq!(shared.attempt, 2, "after 3: sb(A).attempt");
    assert_ne!(
        shared.status,
        BuildStatus::FailedPermanent,
        "after 3: sb(A).status"
    );

    let fourth = dispatch(&case, W1).await.expect("after 4: W1 takes A");
    assert_eq!(sent_cores(&fourth), Some(43), "after 4: cores sent");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn e10_a_worker_ignoring_the_sent_cores_ends_on_the_attempt_budget() {
    let case = start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    let mut dispatches = 0;
    while let Some(taken) = dispatch(&case, W1).await {
        dispatches += 1;
        assert!(dispatches <= 4, "after {dispatches}: the retries never end");
        kill(&case, W1, &taken, Some(64)).await;
    }

    assert_eq!(
        dispatches, 4,
        "after 1: one free retry, then three attempts"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(A).status"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn e11_a_row_without_a_build_time_from_a_lost_worker_caps_nothing() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .derivation_metric(|m| {
            m.build_cores = Some(64);
            m.peak_ram_mb = Some(30_000);
        });
    let case = start(seed).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(256_000), &[])
        .await;

    let taken = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    assert_eq!(sent_cores(&taken), Some(64), "after 1: cores sent");
    assert!(
        reserved_mb(&taken).is_some_and(|mb| mb >= 30_000),
        "after 1: the lower bound holds at 64 cores: {:?}",
        taken.ram_need
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn e12_a_build_too_large_at_full_cores_starts_with_fewer() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for (cores, peak) in [(64, 200_000), (16, 50_000)] {
        for _ in 0..3 {
            seed = seed.derivation_metric(move |m| {
                m.build_cores = Some(cores);
                m.peak_ram_mb = Some(peak);
                m.build_time_ms = Some(60_000);
            });
        }
    }
    let case = start(seed).await;
    case.connect(W1, w_build().cpus(64).ram_total_mb(60_000), &[])
        .await;
    case.refit_build_history().await;

    let taken = dispatch(&case, W1).await.expect("after 1: W1 takes A");
    let cores = sent_cores(&taken).expect("after 1: cores sent");
    assert!(cores < 64, "after 1: cores sent {cores}");
    assert!(
        reserved_mb(&taken).is_some_and(|mb| mb <= 60_000),
        "after 1: the reservation fits the worker: {:?}",
        taken.ram_need
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn e13_a_kill_of_another_build_of_the_package_holds_without_a_fit() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .pname("leaky")
        .startable(B, B_OUT)
        .pname("leaky")
        .derivation_metric(|m| {
            m.oom_killed = true;
            m.build_cores = Some(16);
            m.peak_ram_mb = Some(20_000);
        });
    let case = start(seed).await;
    case.connect(W1, w_build().cpus(16).slots(2).ram_total_mb(256_000), &[])
        .await;
    let a = case.key(A).await;

    let mut taken = Vec::new();
    while let Some(next) = dispatch(&case, W1).await {
        taken.push(next);
    }
    let on_a = taken
        .iter()
        .find(|t| t.job_id() == a)
        .expect("after 1: W1 takes A");
    assert_eq!(sent_cores(on_a), Some(16), "after 1: cores sent to A");
    assert!(
        reserved_mb(on_a).is_some_and(|mb| mb >= 22_000),
        "after 1: the kill of B holds for A: {:?}",
        on_a.ram_need
    );
    case.check("step 1").await;

    case.finish().await;
}
