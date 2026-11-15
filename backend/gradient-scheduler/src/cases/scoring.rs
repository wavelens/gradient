/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use chrono::{Duration, NaiveDateTime, Timelike};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{DispatchedJobId, EvaluationId};
use gradient_fixtures::shape::shape;
use gradient_pool::score::weights::REFERENCE_CORES;
use gradient_pool::score::{ScoreBreakdown, Windowed};
use gradient_wire::types::{BuildFailureKind, BuildMetrics, FlakeStep, JobKind, JobPhase};

use super::{
    A, A_OUT, B, B_OUT, C, C_OUT, CaseShapes, D, D_OUT, N, N_OUT, SchedulerCase, ago, attr_of,
    failure, is_recent, output_nar, span, w_all,
};

const W1: &str = "w1";
const W2: &str = "w2";
const DAY: i64 = 86_400;

#[tokio::test]
async fn m06_instance_windows_take_rows_at_the_boundary_and_wait_from_ready_at() {
    let now = whole_second_now();
    let before = |secs: i64| now - Duration::seconds(secs);
    let mut seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .open_assignment()
        .dispatched_at(before(540))
        .ready_at(before(600))
        .claim_window(1, 10_485_760, 2)
        .closed(DispatchedJobOutcome::Abandoned, before(480))
        .eval_assignment()
        .dispatched_at(before(540))
        .ready_at(before(600))
        .claim_window(9, 9_437_184_000, 9)
        .closed(DispatchedJobOutcome::Abandoned, before(480));
    for (age, peak) in [
        (300, 100),
        (301, 300),
        (3600, 500),
        (DAY - 1, 700),
        (DAY + 1, 9000),
    ] {
        seed = seed.derivation_metric(move |m| {
            m.peak_ram_mb = Some(peak);
            m.created_at = before(age);
        });
    }
    let case = SchedulerCase::start(seed).await;

    let context = case.instance_context(now).await;
    assert_eq!(
        context.peak_ram_mb,
        windowed(100.0, 300.0, 400.0),
        "after 1: peak_ram"
    );
    assert_eq!(
        context.completed,
        windowed(1.0, 3.0, 4.0),
        "after 1: completed"
    );
    assert_eq!(
        (
            context.wait_secs.w1h,
            context.nar_size_mb.w1h,
            context.missing_paths.w1h,
            context.dependency_cnt.w1h,
        ),
        (Some(60.0), Some(10.0), Some(1.0), Some(2.0)),
        "after 1: wait_secs, nar_size_mb, missing_paths, dependency_cnt of w1h"
    );

    case.finish().await;
}

#[tokio::test]
async fn m12_an_instance_without_rows_has_empty_windows_and_zero_completed() {
    let now = whole_second_now();
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;

    let context = case.instance_context(now).await;
    assert_eq!(
        (
            context.peak_ram_mb,
            context.wait_secs,
            context.nar_size_mb,
            context.missing_paths,
            context.dependency_cnt,
        ),
        (
            Windowed::default(),
            Windowed::default(),
            Windowed::default(),
            Windowed::default(),
            Windowed::default(),
        ),
        "after 1: windowed values"
    );
    assert_eq!(
        context.completed,
        windowed(0.0, 0.0, 0.0),
        "after 1: completed"
    );

    case.finish().await;
}

#[tokio::test]
async fn m07_eval_history_takes_the_p95_rss_per_task_and_the_fleet_fetch_mean() {
    let now = whole_second_now();
    let before = |secs: i64| now - Duration::seconds(secs);
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let mut seed = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Completed);
    for (age, fetch_ms) in [(3600, 900), (1800, 1100)] {
        seed = seed
            .eval_assignment()
            .dispatched_at(before(age))
            .closed(DispatchedJobOutcome::Completed, before(age - 600))
            .worker_elapsed_ms(1500)
            .phase(JobPhase::Fetch.as_i16(), 0, fetch_ms);
    }
    for (age, rss) in (0..=20)
        .map(|i| (60 * (i + 1), 100 + i))
        .chain([(25 * 3600, 9000)])
    {
        seed = seed
            .evaluation(EvaluationId::now_v7())
            .same_task_as(e1)
            .evaluation_status(EvaluationStatus::Completed)
            .evaluation_metric(move |m| {
                m.peak_rss_mb = rss;
                m.created_at = before(age);
            });
    }
    let seed = seed
        .evaluation(e2)
        .evaluation_status(EvaluationStatus::Completed);
    let case = SchedulerCase::start(seed).await;
    let (t1, t2) = (case.db.task_of(e1).await, case.db.task_of(e2).await);

    let history = case.eval_history(now).await;
    let steps = [FlakeStep::FetchFlake];
    let own = history.for_job(t1, &steps);
    assert_eq!(
        (
            own.ram_mb(REFERENCE_CORES),
            own.build_ms(REFERENCE_CORES).map(f64::round),
            own.own_samples > 0,
        ),
        (Some(119), Some(1000.0), true),
        "after 1: result[T1].peak_rss_p95, fetch_mean"
    );
    let other = history.for_job(t2, &steps);
    assert_eq!(
        (
            other.ram_mb(REFERENCE_CORES),
            other.build_ms(REFERENCE_CORES).map(f64::round),
            other.own_samples,
        ),
        (None, Some(1000.0), 0),
        "after 1: result[T2].peak_rss_p95, fetch_mean of every run"
    );

    case.finish().await;
}

#[tokio::test]
async fn m08_only_a_real_build_or_an_oom_kill_writes_a_metric_row() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all().slots(2), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let mut assignments: HashMap<String, DispatchedJobId> = HashMap::new();
    for _ in 0..2 {
        let taken = case
            .request_job(W1, JobKind::Build)
            .await
            .expect("step 1: W1 takes a build");
        assignments.insert(taken.job_id().to_owned(), taken.assignment_id());
    }
    for key in [&a, &b] {
        case.report_building(W1, key).await;
    }
    case.check("step 1").await;

    case.fail(
        W1,
        &a,
        assignments[&a],
        failure(BuildFailureKind::Permanent)
            .error("exit 1")
            .spans(vec![span(JobPhase::Build, 0, 1)])
            .elapsed_ms(1),
    )
    .await;
    assert_eq!(
        case.derivation_metrics(A).await,
        vec![],
        "after 2: derivation_metric(A)"
    );
    case.check("step 2").await;

    case.fail(
        W1,
        &b,
        assignments[&b],
        failure(BuildFailureKind::Transient)
            .error("oom")
            .spans(vec![span(JobPhase::Build, 0, 1)])
            .elapsed_ms(1)
            .metrics(BuildMetrics {
                oom_killed: true,
                peak_ram_mb: Some(4000),
                ..Default::default()
            }),
    )
    .await;
    let metrics: Vec<_> = case
        .derivation_metrics(B)
        .await
        .into_iter()
        .map(|m| (m.oom_killed, m.peak_ram_mb))
        .collect();
    assert_eq!(
        metrics,
        vec![(true, Some(4000))],
        "after 3: derivation_metric(B)"
    );
    let first = case
        .attempts(B)
        .await
        .into_iter()
        .next()
        .map(|t| (t.outcome, t.reason));
    assert_eq!(
        first,
        Some((AttemptOutcome::Failed, Some(AttemptFailureReason::Oom))),
        "after 3: att(B)[0]"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTransient,
        "after 3: sb(B).status"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn m09_build_assigned_to_first_requesting_worker_without_score_report() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.check("step 1").await;

    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 2: result.job_id"
    );
    let row = case
        .open_assignment(&a)
        .await
        .expect("after 2: open(build:A)");
    assert_eq!(
        breakdown(&row.score_breakdown).vetoes,
        Vec::<String>::new(),
        "after 2: open(build:A).score_breakdown.vetoes"
    );
    assert_eq!(
        row.missing_nar_size,
        Some(0),
        "after 2: open(build:A).missing_nar_size"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn m10_equal_scores_tie_to_the_smaller_job_id_and_the_row_keeps_the_score() {
    let seed = shape()
        .two(EvaluationId::now_v7())
        .add_missing_cached_build_input(A, D, D_OUT)
        .add_missing_cached_build_input(B, N, N_OUT);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    let smaller = a.clone().min(b.clone());

    case.admit_startable_moves().await;
    case.check("step 1").await;

    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(smaller.clone()),
        "after 2: result.job_id"
    );
    let row = case
        .open_assignment(&smaller)
        .await
        .expect("after 2: open(smaller key)");
    assert_eq!(
        (
            row.missing_count,
            row.missing_nar_size,
            row.dependency_count
        ),
        (Some(1), Some(5), Some(1)),
        "after 2: missing_count, missing_nar_size, dependency_count"
    );
    let ready_at = row.ready_at.expect("after 2: ready_at set");
    assert!(is_recent(ready_at), "after 2: ready_at ~t_ready");
    assert!(row.queued_at <= ready_at, "after 2: queued_at <= ready_at");
    assert_eq!(
        row.score,
        breakdown(&row.score_breakdown).total,
        "after 2: score == score_breakdown.total"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn m11_outputs_present_on_the_worker_lift_the_predicted_resource_penalty() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for i in 1..=20 {
        seed = seed.derivation_metric(move |m| {
            m.peak_ram_mb = Some(60_000);
            m.build_time_ms = Some(60_000);
            m.created_at = ago(60 * i);
        });
    }
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all().ram_total_mb(8000), &[]).await;
    case.connect(W2, w_all().ram_total_mb(8000), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.install_worker_local_store_bloom_filter(W1, &[A_OUT])
        .await;
    case.check("step 1").await;

    let refused = case.request_job(W2, JobKind::Build).await;
    assert!(refused.is_none(), "after 2: result == nothing");
    case.check("step 2").await;

    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 3: result.job_id"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn m13_completed_build_updates_only_the_completing_workers_store_summary() {
    let seed = shape()
        .chain(EvaluationId::now_v7())
        .add_missing_cached_build_input(B, D, D_OUT)
        .derivation(C)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(C))
        .job()
        .output(C_OUT)
        .depends_on(B);
    let case = SchedulerCase::start_with(seed, |c| c.build.retry_backoff_secs = 1).await;
    case.connect(W1, w_all(), &[]).await;
    case.connect(W2, w_all(), &[]).await;
    case.install_worker_local_store_bloom_filter(W1, &[]).await;
    case.install_worker_local_store_bloom_filter(W2, &[]).await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);

    case.admit_startable_moves().await;
    let b1 = case
        .request_job(W1, JobKind::Build)
        .await
        .expect("after 1: W1 gets build:B")
        .assignment_id();
    assert_eq!(
        read_open_assignment_missing_count_and_nar_size_for_job_key(&case, &b).await,
        (Some(1), Some(5)),
        "after 1: open(build:B) misses D on W1"
    );
    case.check("step 1").await;

    case.report_building(W1, &b).await;
    case.commit_nar(output_nar(B, B_OUT)).await;
    case.complete(W1, &b, b1, vec![span(JobPhase::Build, 0, 1000)], 1000)
        .await;
    case.admit_startable_moves().await;
    case.check("step 2").await;

    let assignment = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: W2 gets a dependent of B");
    let first_dependent_job_key_received_by_worker_w2 = assignment.job_id().to_owned();
    let remaining_dependent_build_job_key_for_worker_w1 =
        if first_dependent_job_key_received_by_worker_w2 == a {
            c.clone()
        } else {
            a.clone()
        };
    assert!(
        [&a, &c].contains(&&first_dependent_job_key_received_by_worker_w2),
        "after 3: W2 gets build:A or build:C"
    );
    assert_eq!(
        read_open_assignment_missing_count_and_nar_size_for_job_key(
            &case,
            &first_dependent_job_key_received_by_worker_w2
        )
        .await,
        (Some(1), Some(5)),
        "after 3: W2 still misses B's output"
    );
    case.check("step 3").await;

    case.report_building(W2, &first_dependent_job_key_received_by_worker_w2)
        .await;
    case.fail(
        W2,
        &first_dependent_job_key_received_by_worker_w2,
        assignment.assignment_id(),
        failure(BuildFailureKind::Transient).elapsed_ms(1),
    )
    .await;
    case.check("step 4").await;

    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(remaining_dependent_build_job_key_for_worker_w1.clone()),
        "after 5: W1 gets the other dependent of B"
    );
    assert_eq!(
        read_open_assignment_missing_count_and_nar_size_for_job_key(
            &case,
            &remaining_dependent_build_job_key_for_worker_w1
        )
        .await,
        (Some(0), Some(0)),
        "after 5: W1 holds B's output without a new store summary"
    );
    case.check("step 5").await;

    tokio::time::sleep(std::time::Duration::from_secs_f64(1.2)).await;
    case.requeue_transient_retries().await;
    case.admit_startable_moves().await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(first_dependent_job_key_received_by_worker_w2.clone()),
        "after 6: W2 gets the failed dependent again"
    );
    assert_eq!(
        read_open_assignment_missing_count_and_nar_size_for_job_key(
            &case,
            &first_dependent_job_key_received_by_worker_w2
        )
        .await,
        (Some(1), Some(5)),
        "after 6: the failed build added nothing to W2's store summary"
    );
    case.check("step 6").await;

    case.finish().await;
}

async fn read_open_assignment_missing_count_and_nar_size_for_job_key(
    case: &SchedulerCase,
    key: &str,
) -> (Option<i32>, Option<i64>) {
    let row = case
        .open_assignment(key)
        .await
        .unwrap_or_else(|| panic!("open({key})"));
    (row.missing_count, row.missing_nar_size)
}

fn whole_second_now() -> NaiveDateTime {
    gradient_types::now()
        .with_nanosecond(0)
        .expect("a whole second")
}

fn windowed(w5m: f64, w1h: f64, w24h: f64) -> Windowed {
    Windowed {
        w5m: Some(w5m),
        w1h: Some(w1h),
        w24h: Some(w24h),
    }
}

fn breakdown(stored: &serde_json::Value) -> ScoreBreakdown {
    serde_json::from_value(stored.clone()).expect("a score breakdown")
}
