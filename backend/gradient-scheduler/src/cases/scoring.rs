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
use gradient_fixtures::shape::{Shape, shape};
use gradient_pool::score::{HistoryPrediction, ScoreBreakdown, Windowed};
use gradient_wire::types::{BuildFailureKind, BuildMetrics, FlakeStep, JobKind, JobPhase};

use super::{
    A, A_OUT, ARM, B, CaseShapes, SchedulerCase, SlotKind, X86, ago, failure, is_recent, span,
    w_all,
};

const W1: &str = "w1";
const W2: &str = "w2";
const DAY: i64 = 86_400;

const F: &str = "ffffffffffffffffffffffffffffffff-foo-1.0.drv";
const G: &str = "gggggggggggggggggggggggggggggggg-foo-1.1.drv";
const F_OUT: &str = "f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0";
const F_DEV: &str = "f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1";
const G_OUT: &str = "g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0";
const G_DEV: &str = "g1g1g1g1g1g1g1g1g1g1g1g1g1g1g1g1";

#[tokio::test]
async fn m01_predict_takes_the_newest_20_rows_of_the_name_and_architecture() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for i in 1..=25 {
        seed = seed.derivation_metric(move |m| {
            m.pname = Some("hello".into());
            m.peak_ram_mb = Some(100 + i);
            m.build_time_ms = Some(1000);
            m.created_at = ago(60 * i);
        });
    }
    for (pname, architecture) in [
        ("hello", ARM),
        ("hello", ARM),
        ("hello", ARM),
        ("other", X86),
        ("other", X86),
    ] {
        seed = seed.derivation_metric(move |m| {
            m.pname = Some(pname.into());
            m.architecture = architecture.into();
            m.peak_ram_mb = Some(9999);
            m.created_at = ago(30);
        });
    }
    let case = SchedulerCase::start(seed).await;

    let prediction = case.predict("hello", X86).await;
    assert_eq!(
        (
            prediction.samples,
            prediction.predicted_peak_ram_mb,
            prediction.build_time_ms,
        ),
        (20, Some(119), Some(1000)),
        "after 1: samples, peak_ram_p95, build_time_mean"
    );

    case.finish().await;
}

#[tokio::test]
async fn m02_rows_older_than_the_retention_are_outside_the_window() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for (peak, age) in [(100, DAY), (9000, 91 * DAY)] {
        for _ in 0..5 {
            seed = seed.derivation_metric(move |m| {
                m.pname = Some("hello".into());
                m.peak_ram_mb = Some(peak);
                m.created_at = ago(age);
            });
        }
    }
    let case = SchedulerCase::start(seed).await;
    case.retention().await;

    let prediction = case.predict("hello", X86).await;
    assert_eq!(
        (prediction.samples, prediction.predicted_peak_ram_mb),
        (5, Some(100)),
        "after 1: samples, peak_ram_p95"
    );

    case.finish().await;
}

#[tokio::test]
async fn m03_only_old_rows_give_no_prediction() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for _ in 0..3 {
        seed = seed.derivation_metric(|m| {
            m.pname = Some("hello".into());
            m.peak_ram_mb = Some(100);
            m.build_time_ms = Some(1000);
            m.created_at = ago(100 * DAY);
        });
    }
    let case = SchedulerCase::start(seed).await;
    case.retention().await;

    assert_eq!(
        case.predict("hello", X86).await,
        HistoryPrediction::default(),
        "after 1: samples 0, every value None"
    );

    case.finish().await;
}

#[tokio::test]
async fn m04_a_derivation_without_pname_matches_by_name_and_sums_its_output_sizes() {
    let case = SchedulerCase::start(
        shape()
            .one(EvaluationId::now_v7(), F, F_OUT)
            .named_output(F_DEV, "dev")
            .output_option(F_OUT, |o| o.nar_size = Some(10))
            .output_option(F_DEV, |o| o.nar_size = Some(20))
            .derivation_metric(|m| m.created_at = ago(60))
            .startable(G, G_OUT)
            .named_output(G_DEV, "dev")
            .output_option(G_OUT, |o| o.nar_size = Some(30))
            .derivation_metric(|m| m.created_at = ago(60)),
    )
    .await;

    let foo_1_0 = case.predict("foo-1.0", X86).await;
    assert_eq!(
        (foo_1_0.samples, foo_1_0.output_nar_size),
        (1, Some(30)),
        "after 1: samples, output_nar_size_mean"
    );

    let foo_1_1 = case.predict("foo-1.1", X86).await;
    assert_eq!(
        (foo_1_1.samples, foo_1_1.output_nar_size),
        (1, Some(30)),
        "after 2: samples, output_nar_size_mean"
    );

    case.finish().await;
}

#[tokio::test]
async fn m05_p95_oom_rate_contention_and_a_measured_zero() {
    let mut seed = shape().one(EvaluationId::now_v7(), A, A_OUT);
    for i in 1..=20 {
        let (build_time_ms, concurrent_builds) = if i <= 10 { (1000, 0) } else { (2000, 25) };
        seed = seed.derivation_metric(move |m| {
            m.pname = Some("p".into());
            m.peak_ram_mb = Some(i);
            m.oom_killed = i <= 2;
            m.build_time_ms = Some(build_time_ms);
            m.concurrent_builds = Some(concurrent_builds);
            m.cpu_time_ms = Some(0);
            m.created_at = ago(60 * i);
        });
    }
    let case = SchedulerCase::start(seed).await;

    let prediction = case.predict("p", X86).await;
    assert!(
        (prediction.oom_rate - 0.1).abs() < 1e-6,
        "after 1: oom_rate == 0.1, got {}",
        prediction.oom_rate
    );
    assert_eq!(
        (
            prediction.predicted_peak_ram_mb,
            prediction.build_time_ms,
            prediction.uncontended_build_time_ms,
            prediction.avg_disk_bytes,
            prediction.avg_cpu_time_ms,
        ),
        (Some(19), Some(1500), Some(1000), None, Some(0)),
        "after 1: peak_ram_p95, build_time_mean, build_time_uncontended_mean, disk_bytes_mean, avg_cpu_time_ms"
    );

    case.finish().await;
}

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
async fn m06_an_instance_without_rows_has_empty_windows_and_zero_completed() {
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
    for (age, rss) in (0..=20)
        .map(|i| (60 * (i + 1), 100 + i))
        .chain([(25 * 3600, 9000)])
    {
        seed = seed.evaluation_metric(move |m| {
            m.peak_rss_mb = rss;
            m.created_at = before(age);
        });
    }
    for (age, fetch_ms) in [(3600, 900), (1800, 1100)] {
        seed = seed
            .eval_assignment()
            .dispatched_at(before(age))
            .closed(DispatchedJobOutcome::Completed, before(age - 600))
            .worker_elapsed_ms(1500)
            .phase(JobPhase::Fetch.as_i16(), 0, fetch_ms);
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
        (own.predicted_peak_ram_mb, own.build_time_ms),
        (Some(119), Some(1000)),
        "after 1: result[T1].peak_rss_p95, fetch_mean"
    );
    let other = history.for_job(t2, &steps);
    assert_eq!(
        (
            other.predicted_peak_ram_mb,
            other.uncontended_build_time_ms,
            other.from_fleet_mean,
        ),
        (None, Some(1000), true),
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
    case.offers_full(W1).await;
    case.score_all(W1).await;
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
async fn m09_an_unscored_build_waits_for_a_score_or_four_timer_ticks() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all(), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.check("step 1").await;

    let unscored = case.request_job(W1, JobKind::Build).await;
    assert!(unscored.is_none(), "after 2: result == nothing");
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert!(
        case.idle_slots()
            .await
            .contains(&(W1.into(), SlotKind::Build)),
        "after 2: idle_slots contains (W1, Build)"
    );
    case.check("step 2").await;

    for _ in 0..4 {
        case.build_assign_pass(true, false).await;
    }
    case.check("step 3").await;

    let taken = case.request_job(W1, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 4: result.job_id"
    );
    let row = case
        .open_assignment(&a)
        .await
        .expect("after 4: open(build:A)");
    assert_eq!(
        breakdown(&row.score_breakdown).vetoes,
        Vec::<String>::new(),
        "after 4: open(build:A).score_breakdown.vetoes"
    );
    assert_eq!(
        row.missing_nar_size, None,
        "after 4: open(build:A).missing_nar_size"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn m10_equal_scores_tie_to_the_smaller_job_id_and_the_row_keeps_the_score() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all(), &[]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    let smaller = a.clone().min(b.clone());

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.scores(W1, &[(&a, 2, 2048, false), (&b, 2, 2048, false)])
        .await;
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
        (Some(2), Some(2048), Some(0)),
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
            m.created_at = ago(60 * i);
        });
    }
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all().ram_total_mb(8000), &[]).await;
    case.connect(W2, w_all().ram_total_mb(8000), &[]).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    case.offers_full(W1).await;
    case.offers_full(W2).await;
    case.scores(W1, &[(&a, 0, 0, true)]).await;
    case.scores(W2, &[(&a, 5, 50_000, false)]).await;
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
