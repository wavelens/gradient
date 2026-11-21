/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::dispatched_job::DispatchedJobOutcome;
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{CacheId, DispatchedJobId, EvaluationId};
use gradient_entity::pending_delivery::PendingDeliveryKind;
use gradient_entity::store_path_content_producer::{
    CacheUploadChannel, StoredContentVariantProducerKind,
};
use gradient_entity::{
    RotatedHashMapping, StorePathMapper, derivation, derivation_build, derivation_feature,
    derivation_output,
};
use gradient_fixtures::shape::{Shape, shape};
use gradient_graph::read_derivation_from_trusted_nar::load_verified_trusted_derivation_record;
use gradient_graph::{NarCommit, StoredContentProducer};
use gradient_test_support::cache_fixture::zstd_compressed_nar_of_bytes;
use gradient_test_support::derivation_file_from_reported_facts::DerivationFileFromReportedFacts;
use gradient_types::{EvalCapability, WaitingReason};
use gradient_wire::types::{
    BuildFailureKind, BuildMetrics, BuildOutput, BuildSpecKind, BumpedInputWire, DerivationOutput,
    DiscoveredDerivation, EvalMessageLevel, EvalStatsReport, FlakeSource, FlakeStep, Job, JobKind,
    JobPhase, JobPhaseSpan,
};
use gradient_wire::worker_store_path_bloom_filter::decode_store_path_hash_into_two_u64_words;
use sea_orm::{ActiveEnum, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

use super::{
    A, A_OUT, ARM, B, B_OUT, C, C_OUT, CaseShapes, D, D_OUT, S_OUT, SchedulerCase, X, X_OUT, X86,
    ago, attr_of, build_output, computed_out_output_path_hash, drv, eval_key, failure, is_recent,
    jobs_available_signal_for_one_kind, nar, narinfo, output_nar, span,
    test_derivation_store_file_name, w_all, w_eval, w_network_eval, with_attr, with_output,
    with_references,
};
use crate::jobs::PendingJob;
use crate::pending_job_input_store_paths::PendingJobInputStorePath;

const W1: &str = "w1";
const W2: &str = "w2";
const SOURCE_HASH: &str = "1b9p07z77phvv2hf6gm9f28syp39f1ag";
const SOURCE: &str = "/nix/store/1b9p07z77phvv2hf6gm9f28syp39f1ag-source";

#[tokio::test]
async fn r01_the_building_report_moves_queued_to_building_and_opens_the_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;

    claim(&case, W1).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(started(&case, A).await, vec![], "after 1: att(A) == []");
    case.signals(W1);
    case.check("step 1").await;

    case.report_building(W1, &a).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::Building, 0),
        "after 2: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        started(&case, A).await,
        vec![(AttemptOutcome::Running, true)],
        "after 2: att(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 0, 1],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    let before = (case.sb(A).await, case.attempts(A).await);
    case.report_building(W1, &a).await;
    assert_eq!(
        (case.sb(A).await, case.attempts(A).await),
        before,
        "after 3: sb(A), att(A) unchanged"
    );
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r02_a_completion_without_a_building_report_leaves_no_second_running_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    case.connect(W2, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert_eq!(started(&case, A).await, vec![], "after 2: att(A) == []");
    case.check("step 2").await;

    case.resync_startable_set().await;
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    let taken = case.request_job(W2, JobKind::Build).await;
    assert!(taken.is_none(), "after 4: result == nothing");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r03_a_failure_after_a_completion_and_a_second_completion_change_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a1, vec![span(JobPhase::Build, 0, 5)], 5)
        .await;
    let closed = vec![(a1, Some(DispatchedJobOutcome::Completed), Some(5))];
    assert_eq!(elapsed(&case, &a).await, closed, "after 2: dj(build:A)");
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
    let phases = case.phases(a1).await.len();
    case.check("step 2").await;

    let late = failure(BuildFailureKind::Permanent)
        .error("late")
        .spans(vec![span(JobPhase::Build, 0, 6)])
        .elapsed_ms(6);
    case.fail(W1, &a, a1, late).await;
    assert_eq!(elapsed(&case, &a).await, closed, "after 3: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(AttemptOutcome::Built, None)],
        "after 3: att(A)"
    );
    case.check("step 3").await;

    case.complete(W1, &a, a1, vec![span(JobPhase::Build, 0, 7)], 7)
        .await;
    assert_eq!(elapsed(&case, &a).await, closed, "after 4: dj(build:A)");
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(AttemptOutcome::Built, None)],
        "after 4: att(A)"
    );
    assert_eq!(
        case.phases(a1).await.len(),
        phases,
        "after 4: dispatched_job_phase(a1) rows as after 2"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn second_build_output_report_in_same_dispatch_is_ignored_and_first_report_values_stay() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all().ram_total_mb(8_000)).await;
    let a = case.key(A).await;
    let metrics = BuildMetrics {
        peak_ram_mb: Some(100),
        cpu_time_ms: Some(2000),
        build_time_ms: Some(3000),
        concurrent_builds: Some(1),
        ..Default::default()
    };

    claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    let outputs = vec![build_output(A, A_OUT)];
    case.report_build_output(W1, &a, outputs.clone(), false, Some(metrics.clone()))
        .await;
    let pname = case.db.derivation(A).await.history_name().to_owned();
    let rows: Vec<_> = case
        .derivation_metrics(A)
        .await
        .into_iter()
        .map(|m| {
            (
                m.peak_ram_mb,
                m.cpu_time_ms,
                m.build_time_ms,
                m.worker_id,
                m.architecture,
                m.pname,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![(
            Some(100),
            Some(2000),
            Some(3000),
            W1.to_owned(),
            super::X86.to_owned(),
            Some(pname),
        )],
        "after 2: derivation_metric(A)"
    );
    case.check("step 2").await;

    let second = vec![BuildOutput {
        nar_size: Some(999),
        ..build_output(A, A_OUT)
    }];
    case.report_build_output(W1, &a, second, true, Some(metrics))
        .await;
    assert_eq!(
        (
            case.derivation_metrics(A).await.len(),
            case.output_of(A, "out").await.nar_size,
            case.sb(A).await.substituted,
        ),
        (1, Some(5), false),
        "after 3: derivation_metric(A) rows, nar_size and substituted of the first report"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn build_output_for_other_projects_build_id_is_ignored() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT).one(e2, B, B_OUT)).await;
    let (p1, p2) = (case.db.project_of(e1).await, case.db.project_of(e2).await);
    case.connect_for_projects(W1, w_all(), &[p1]).await;
    case.connect_for_projects(W2, w_all(), &[p2]).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    case.admit_startable_moves().await;
    take(&case, W1, &a).await;
    let b1 = take(&case, W2, &b).await;
    case.report_building(W1, &a).await;
    case.report_building(W2, &b).await;
    let nar_size_before = case.output_of(B, "out").await.nar_size;
    case.check("step 1").await;

    let forged = BuildOutput {
        nar_size: Some(999),
        ..build_output(B, B_OUT)
    };
    case.scheduler
        .handle_build_output(W1, &b, vec![forged], None, true)
        .await
        .expect("the report of W1 is handled");
    assert_eq!(
        (
            case.output_of(B, "out").await.nar_size,
            case.sb(B).await.substituted,
        ),
        (nar_size_before, false),
        "after 2: output(B) and sb(B) are unchanged by W1"
    );
    case.check("step 2").await;

    case.report_build_output(W2, &b, vec![build_output(B, B_OUT)], false, None)
        .await;
    case.commit_nar(NarCommit {
        producer: case.make_worker_producer_for_evaluation_project(e2).await,
        ..output_nar(B, B_OUT)
    })
    .await;
    case.complete(W2, &b, b1, Vec::new(), 700).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.output_of(B, "out").await.nar_size,
        ),
        (BuildStatus::Completed, Some(5)),
        "after 3: sb(B) completes with the output of W2"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r05_a_report_for_a_reaped_job_leaves_the_closed_row_and_the_graph_alone() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    case.reap_overdue_aborts(Duration::ZERO).await;
    let reaped = vec![(a1, Some(DispatchedJobOutcome::Abandoned))];
    assert_eq!(
        case.assignment_outcomes(&a).await,
        reaped,
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 2: sb(A).status"
    );
    assert_eq!(case.active(&a).await, None, "after 2: active(build:A)");
    case.signals(W1);
    case.check("step 2").await;

    let rows = case.assignments(&a).await;
    let attempts = case.attempts(A).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(case.assignments(&a).await, rows, "after 3: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 3: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 3: att(A)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Aborted,
        "after 3: evaluation(E1).status"
    );
    case.check("step 3").await;

    case.report_building(W1, &a).await;
    assert_eq!(case.assignments(&a).await, rows, "after 4: dj(build:A)");
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Aborted,
        "after 4: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 4: att(A)");
    assert_eq!(case.signals(W1), vec![], "after 4: signals(W1)");
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r06_a_report_with_the_id_of_a_rejected_claim_is_dropped_once_another_worker_runs_it() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    case.connect(W2, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.reject(W1, &a).await;
    case.check("step 1").await;

    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:A")
        .assignment_id();
    case.report_building(W2, &a).await;
    case.check("step 2").await;

    case.complete(W1, &a, a1, Vec::new(), 5).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 3: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 3: sb(A).status"
    );
    assert_eq!(
        case.active(&a).await.as_deref(),
        Some(W2),
        "after 3: active(build:A)"
    );
    case.check("step 3").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W2, &a, a2, Vec::new(), 9).await;
    assert_eq!(
        elapsed(&case, &a).await.get(1).cloned(),
        Some((a2, Some(DispatchedJobOutcome::Completed), Some(9))),
        "after 4: dj(build:A)[1]"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 4: sb(A).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r07_a_failure_with_an_old_id_of_a_lost_worker_does_not_fail_the_build_of_another() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().one(e1, A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    case.connect(W2, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.disconnect(W1).await;
    case.check("step 2").await;

    let a2 = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 3: W2 gets build:A")
        .assignment_id();
    case.report_building(W2, &a).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 3: sb(A).status"
    );
    let attempts: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.reason, t.dispatched_job))
        .collect();
    assert!(
        matches!(
            attempts.as_slice(),
            [
                (AttemptOutcome::Aborted, Some(AttemptFailureReason::WorkerLost), _),
                (AttemptOutcome::Running, None, Some(running)),
            ] if *running == a2
        ),
        "after 3: att(A) == [{{Aborted, WorkerLost}}, {{Running, a2}}], got {attempts:?}"
    );
    case.check("step 3").await;

    let attempts = case.attempts(A).await;
    case.connect(W1, w_all()).await;
    let old = failure(BuildFailureKind::Permanent)
        .error("old")
        .elapsed_ms(1);
    case.fail(W1, &a, a1, old).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned)), (a2, None)],
        "after 4: dj(build:A)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Building,
        "after 4: sb(A).status"
    );
    assert_eq!(case.attempts(A).await, attempts, "after 4: att(A)");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 4: evaluation(E1).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r08_a_transient_failure_retries_with_backoff_then_fails_permanently() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with(shape().one(e1, A, A_OUT), |c| {
        c.build.retry_backoff_secs = 1;
    })
    .await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.fail(W1, &a, a1, transient("flaky")).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Failed],
        "after 2: att(A)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 1),
        "after 2: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    case.check("step 2").await;

    retry_after(&case, 1.2).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 3: sb(A).status"
    );
    assert!(case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    let a2 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.fail(W1, &a, a2, transient("")).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 2),
        "after 4: sb(A).status, sb(A).attempt"
    );
    case.check("step 4").await;

    retry_after(&case, 2.5).await;
    case.check("step 5").await;

    let a3 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.fail(W1, &a, a3, transient("")).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 6: sb(A).status"
    );
    let failed: Vec<_> = case
        .attempt_outcomes_with_reasons(A)
        .await
        .into_iter()
        .map(|(outcome, _)| outcome)
        .collect();
    assert_eq!(
        failed,
        vec![AttemptOutcome::Failed; 3],
        "after 6: att(A) has 3 Failed rows"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 6: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 1, 0, 0],
        "after 6: counters(E1)"
    );
    assert!(!case.pending(&a).await, "after 6: pending(build:A)");
    case.check("step 6").await;

    case.finish().await;
}

#[tokio::test]
async fn build_completed_without_uploaded_output_retries_twice_then_fails_permanently_with_output_path_in_message_and_evaluation_fails()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with(shape().one(e1, A, A_OUT), |c| {
        c.build.retry_backoff_secs = 1;
    })
    .await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;
    let output = case.output_of(A, "out").await;
    let path = format!(
        "/nix/store/{}-{}",
        output.hash.unwrap_or_default(),
        output.package.unwrap_or_default()
    );

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 1),
        "after 1: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    case.check("step 1").await;

    retry_after(&case, 1.2).await;
    let a2 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.complete(W1, &a, a2, Vec::new(), 5).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 2),
        "after 2: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    case.check("step 2").await;

    retry_after(&case, 2.5).await;
    let a3 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.complete(W1, &a, a3, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 3: sb(A).status"
    );
    let attempts = case.attempts(A).await;
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| attempt.outcome)
            .collect::<Vec<_>>(),
        vec![AttemptOutcome::Failed; 3],
        "after 3: att(A) has 3 Failed rows"
    );
    let messages: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.failure_message.unwrap_or_default())
        .collect();
    assert!(
        messages.iter().all(|message| message.contains(&path)),
        "after 3: Each failed attempt of A names {path} in its failure message: {messages:?}"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 3: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 0, 1, 0, 0],
        "after 3: counters(E1)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn stored_copy_deleted_after_upload_skip_before_completion_report_causes_one_transient_retry()
{
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with(shape().one(e1, A, A_OUT), |c| {
        c.build.retry_backoff_secs = 1;
    })
    .await;
    case.connect(W1, w_all()).await;
    let a = case.key(A).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.commit_nar(output_nar(A, A_OUT)).await;
    case.db
        .execute(&format!("DELETE FROM cached_path WHERE hash = '{A_OUT}'"))
        .await;
    case.complete(W1, &a, a1, Vec::new(), 5).await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::FailedTransient, 1),
        "after 1: sb(A).status, sb(A).attempt"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 1: evaluation(E1).status"
    );
    case.check("step 1").await;

    retry_after(&case, 1.2).await;
    let a2 = take(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.commit_nar(output_nar(A, A_OUT)).await;
    case.complete(W1, &a, a2, Vec::new(), 5).await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Completed,
        "after 2: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Failed, AttemptOutcome::Built],
        "after 2: att(A) held one failed attempt from a completion without a stored copy, followed by one built attempt."
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Completed,
        "after 2: evaluation(E1).status"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r09_a_permanent_failure_fails_the_dependents_in_both_evaluations() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
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
        .job();
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all()).await;
    let b = case.key(B).await;

    let a1 = claim(&case, W1).await;
    assert_eq!(
        case.open_assignment(&b).await.map(|row| row.id),
        Some(a1),
        "after 1: W1 runs build:B"
    );
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    let exit = failure(BuildFailureKind::Permanent)
        .error("exit 1")
        .elapsed_ms(8);
    case.fail(W1, &b, a1, exit).await;
    assert_eq!(
        elapsed(&case, &b).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed), Some(8))],
        "after 2: dj(build:B)"
    );
    let attempts = case.attempts(B).await;
    let summary: Vec<_> = attempts
        .iter()
        .map(|t| (t.outcome, t.reason, t.failure_message.as_deref()))
        .collect();
    assert_eq!(
        summary,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::BuilderNonzero),
            Some("exit 1"),
        )],
        "after 2: att(B)"
    );
    let statuses = (
        case.sb(B).await.status,
        case.sb(A).await.status,
        case.sb(C).await.status,
    );
    assert_eq!(
        statuses,
        (
            BuildStatus::FailedPermanent,
            BuildStatus::DependencyFailed,
            BuildStatus::DependencyFailed,
        ),
        "after 2: sb(B), sb(A), sb(C)"
    );
    for (name, e) in [("E1", e1), ("E2", e2)] {
        assert_eq!(
            case.evaluation(e).await.status,
            EvaluationStatus::Failed,
            "after 2: evaluation({name}).status"
        );
        assert_eq!(
            case.counters(e).await,
            [2, 0, 2, 0, 0],
            "after 2: counters({name})"
        );
    }
    let finalize = attempts[0].id.to_string();
    assert!(
        case.deliveries()
            .await
            .iter()
            .any(|d| d.kind == PendingDeliveryKind::LogFinalize && d.key == finalize),
        "after 2: pending_delivery has LogFinalize for att(B)[0]"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r10_a_timeout_failure_is_failed_timeout_and_fails_the_dependent() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().chain(e1)).await;
    case.connect(W1, w_all()).await;
    let b = case.key(B).await;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    let timeout = failure(BuildFailureKind::Timeout).elapsed_ms(14_400_000);
    case.fail(W1, &b, a1, timeout).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTimeout,
        "after 2: sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(B).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::WallClockTimeout)
        )],
        "after 2: att(B)"
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
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r11_a_second_substitute_miss_turns_the_passthrough_into_a_real_build() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .passthrough(e1)
        .depends_on(B)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Skipped);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all()).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    let attempt = case.sb(A).await.attempt;

    case.admit_startable_moves().await;
    let a1 = substitute(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    case.fail(W1, &a, a1, failure(BuildFailureKind::SubstituteUnavailable))
        .await;
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.cache_available, shared.attempt),
        (BuildStatus::Queued, true, attempt),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::SubstituteUnavailable)
        )],
        "after 2: att(A)"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(
        case.pending(&a).await,
        "after 3: pending(build:A) after the admission"
    );
    let a2 = substitute(&case, W1, &a).await;
    case.report_building(W1, &a).await;
    case.check("step 3").await;

    case.fail(W1, &a, a2, failure(BuildFailureKind::SubstituteUnavailable))
        .await;
    let shared = case.sb(A).await;
    assert_eq!(
        (
            shared.status,
            shared.cache_available,
            shared.attempt,
            shared.blocking_deps
        ),
        (BuildStatus::Created, false, 0, 1),
        "after 4: sb(A)"
    );
    assert_eq!(
        case.output_of(A, "out").await.external_url,
        None,
        "after 4: output(A, out).external_url"
    );
    let dependency = case.sb(B).await;
    assert_eq!(
        (dependency.status, dependency.wanted),
        (BuildStatus::Queued, true),
        "after 4: sb(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 4: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 4: counters(E1)"
    );
    case.check("step 4").await;

    case.admit_startable_moves().await;
    assert!(!case.pending(&a).await, "after 5: pending(build:A)");
    assert!(case.pending(&b).await, "after 5: pending(build:B)");
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn r12_inputs_unavailable_removes_the_missing_input_and_retries_after_it_is_back() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .depends_on(B)
        .build_status(BuildStatus::Queued)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Completed)
        .cached_output(B);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all()).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.check("step 1").await;

    let missing = failure(BuildFailureKind::InputsUnavailable)
        .error("404")
        .missing(&[super::store_path(B_OUT, "b")])
        .elapsed_ms(2);
    case.fail(W1, &a, a1, missing).await;
    assert_eq!(
        case.assignment_outcomes(&a).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Failed,
            Some(AttemptFailureReason::InputsUnavailable)
        )],
        "after 2: att(A)"
    );
    assert!(
        case.cached_path(B_OUT).await.is_none(),
        "after 2: cached_path(B_OUT) == none"
    );
    let dependency = case.sb(B).await;
    assert_eq!(
        (dependency.status, dependency.fetchable),
        (BuildStatus::Queued, false),
        "after 2: sb(B)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.blocking_deps),
        (BuildStatus::FailedTransient, 1),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "after 2: evaluation(E1).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert!(case.pending(&b).await, "after 3: pending(build:B)");
    assert!(!case.pending(&a).await, "after 3: pending(build:A)");
    case.check("step 3").await;

    case.finish().await;
}

fn dependent_a_on_completed_b(
    seed: Shape,
    evaluation: EvaluationId,
    build_a_start_status: BuildStatus,
) -> Shape {
    seed.evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(A))
        .job()
        .output(A_OUT)
        .depends_on(B)
        .build_status(build_a_start_status)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Completed)
}

async fn build_handed_to_w1(case: &SchedulerCase) -> Option<String> {
    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    case.request_job(W1, JobKind::Build)
        .await
        .map(|assignment| assignment.job_id().to_owned())
}

fn build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
    evaluation: EvaluationId,
    build_a_start_status: BuildStatus,
) -> Shape {
    dependent_a_on_completed_b(
        shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(B_OUT, "b"),
        evaluation,
        build_a_start_status,
    )
}

fn build_a_depends_on_completed_build_b_with_output_user_uploaded_to_other_projects_cache_not_read_by_a_project(
    evaluation: EvaluationId,
    build_a_start_status: BuildStatus,
    b_out_upload_cache_id: CacheId,
) -> Shape {
    dependent_a_on_completed_b(
        shape()
            .evaluation(EvaluationId::now_v7())
            .into_cache(b_out_upload_cache_id)
            .cached_path(B_OUT, "b")
            .mark_uploaded_by_user(),
        evaluation,
        build_a_start_status,
    )
}

async fn run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(
    case: &SchedulerCase,
) {
    run_dependency_output_publish_round_with_production_page_size(case)
        .await
        .expect("periodic checks for graph consistency run");
    case.graph
        .run_consistency_checks()
        .await
        .expect("periodic checks for graph consistency run");
    case.settled().await;
}

async fn run_dependency_output_publish_round_with_production_page_size(
    case: &SchedulerCase,
) -> anyhow::Result<()> {
    case.graph
        .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
            None,
            gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await
        .failure
        .map_or(Ok(()), Err)
}

async fn binary_cache_holds_signed_copy_of_store_path_hash(
    case: &SchedulerCase,
    hash: &str,
    cache: CacheId,
) -> bool {
    case.db
        .signatures(hash)
        .await
        .iter()
        .any(|row| row.cache == cache && row.signature.is_some())
}

async fn insert_second_stored_copy_of_build_b_output_b_into_cache_and_run_consistency_check(
    case: &SchedulerCase,
    cache: CacheId,
    signed: bool,
) {
    let signature = if signed {
        "decode(repeat('ab', 64), 'hex')"
    } else {
        "NULL"
    };
    let (uploaded_by_user_or_api_key_kind, rest_http_api_upload_channel_value) = (
        StoredContentVariantProducerKind::UploadedByUserOrApiKey.to_value(),
        CacheUploadChannel::RestHttpApiUpload.to_value(),
    );
    case.db
        .execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES (uuidv7(), '{B_OUT}', 'other-copy', 'b', 'sha256:other', 'sha256:other', 5, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache}', {signature}, 0, now() FROM cached_path \
             WHERE nar_object_storage_key = 'other-copy'; \
             INSERT INTO store_path_content_producer (id, cached_path, kind, cache, upload_channel, created_at) \
             SELECT uuidv7(), id, {uploaded_by_user_or_api_key_kind}, '{cache}', {rest_http_api_upload_channel_value}, now() FROM cached_path \
             WHERE nar_object_storage_key = 'other-copy'"
        ))
        .await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(case).await;
}

async fn assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(
    case: &SchedulerCase,
    waiting: EvaluationId,
    after: &str,
) {
    let (a, b) = (case.key(A).await, case.key(B).await);
    assert_eq!(
        build_handed_to_w1(case).await,
        Some(a.clone()),
        "{after}: build:A went to a worker"
    );
    assert_eq!(
        case.assignments(&a).await.len(),
        1,
        "{after}: build:A only ships once."
    );
    assert_eq!(
        (
            case.attempts(B).await.len(),
            case.assignments(&b).await.len()
        ),
        (0, 0),
        "{after}: build:B received no new attempt or substitute job"
    );
    assert!(
        case.messages(waiting).await.is_empty(),
        "{after}: Pending evaluations show no messages"
    );
}

#[tokio::test]
async fn periodic_check_signs_other_project_input_into_keyed_read_write_cache_then_hands_out_dependent_build_once_without_new_work_for_b()
 {
    let (e1, signing_cache_id) = (EvaluationId::now_v7(), CacheId::now_v7());
    let seed = build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
        e1,
        BuildStatus::Queued,
    )
    .into_cache(signing_cache_id)
    .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start(seed).await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "after 1: A signed B_OUT is in the keyed ReadWrite cache for E1's project."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn shared_build_is_handed_to_worker_once_when_periodic_check_signs_worker_built_input_into_second_project_keyed_signing_cache()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (signing_cache_id, second_project_shared_binary_cache_key) =
        (CacheId::now_v7(), CacheId::now_v7());
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Queued)
        .cached_output(B)
        .into_cache(signing_cache_id)
        .keyed(SchedulerCase::signing_key())
        .evaluation(e2)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .entry_point(&attr_of(A))
        .job()
        .into_cache(second_project_shared_binary_cache_key)
        .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start(seed).await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(
            &case,
            B_OUT,
            second_project_shared_binary_cache_key
        )
        .await,
        "after 1: ReadWrite caches keyed to project E2 hold the signed B_OUT."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e2, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_dependent_build_uses_other_project_stored_input_immediately_without_keyed_read_write_cache()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
            e1,
            BuildStatus::Queued,
        ),
    )
    .await;

    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_dependent_build_with_read_write_subscription_and_sign_outputs_off_uses_other_project_stored_input_directly_then_input_has_no_signature_and_outside_cache_reader_returns_not_found()
 {
    let (e1, project_binary_cache_id) = (EvaluationId::now_v7(), CacheId::now_v7());
    let seed = build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
        e1,
        BuildStatus::Queued,
    )
    .into_cache(project_binary_cache_id)
    .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start_then(seed, async |case| {
        case.db
            .execute(&format!(
                "UPDATE project_cache SET sign_outputs = false WHERE cache = '{project_binary_cache_id}'"
            ))
            .await;
        run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(case).await;
    })
    .await;

    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    assert!(
        !case
            .db
            .signatures(B_OUT)
            .await
            .iter()
            .any(|row| row.cache == project_binary_cache_id),
        "No signature line for B_OUT is present in the unsigned cache after 1."
    );
    assert_eq!(
        gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &case.db.connection(),
            project_binary_cache_id,
            &[B_OUT.to_owned()],
        )
        .await
        .expect("the served variant is read")
        .remove(B_OUT),
        None,
        "After 1, the outside reader of an unsigned cache does not find B_OUT."
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_active_read_write_subscription_with_sign_outputs_off_project_user_content_upload_output_is_read_by_dependent_build_worker_and_missing_for_outside_cache_reader()
 {
    let (e1, binary_cache_id_without_output_signing) = (EvaluationId::now_v7(), CacheId::now_v7());
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Created)
        .into_cache(binary_cache_id_without_output_signing)
        .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start_with_unstored_outputs(seed, 1).await;
    case.db
        .execute(&format!(
            "UPDATE project_cache SET sign_outputs = false WHERE cache = '{binary_cache_id_without_output_signing}'"
        ))
        .await;

    case.set_unstored_outputs(0);
    case.commit_nar(NarCommit {
        producer: StoredContentProducer::ProjectUserContentUpload {
            project: case.db.project_of(e1).await,
            user: gradient_fixtures::rows::user().id,
            channel: CacheUploadChannel::SshNixStoreUpload,
        },
        ..output_nar(B, B_OUT)
    })
    .await;

    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    assert!(
        gradient_db::store_path_content_variant_reader_choice::fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash(
            &case.db.connection(),
            case.sb(A).await.id,
            &gradient_db::store_path_content_variant_reader_choice::StorePathInputClosureHashes::default(),
            &[B_OUT.to_owned()],
        )
        .await
        .expect("The worker read for build A can load.")
        .contains_key(B_OUT),
        "The worker for build A can read B_OUT after step 1. The project uploaded B_OUT to the cache with signatures disabled."
    );
    assert_eq!(
        gradient_db::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &case.db.connection(),
            binary_cache_id_without_output_signing,
            &[B_OUT.to_owned()],
        )
        .await
        .expect("An external reader can read this.")
        .remove(B_OUT),
        None,
        "An outside reader of a cache with signatures turned off can't find B_OUT after 1."
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn dependent_build_accepts_signed_copy_from_other_project_read_cache() {
    let (e1, signing_cache_id, b_out_upload_cache_id) =
        (EvaluationId::now_v7(), CacheId::now_v7(), CacheId::now_v7());
    let seed =
        build_a_depends_on_completed_build_b_with_output_user_uploaded_to_other_projects_cache_not_read_by_a_project(
            e1,
            BuildStatus::Queued,
            b_out_upload_cache_id,
        )
        .into_cache(signing_cache_id)
        .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start_then(seed, async |case| {
        insert_second_stored_copy_of_build_b_output_b_into_cache_and_run_consistency_check(
            case,
            signing_cache_id,
            true,
        )
        .await;
    })
    .await;

    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn periodic_check_signs_unsigned_copy_in_keyed_cache_and_dependent_build_is_handed_to_worker_once()
 {
    let (e1, signing_cache_id) = (EvaluationId::now_v7(), CacheId::now_v7());
    let seed = build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
        e1,
        BuildStatus::Queued,
    )
    .into_cache(signing_cache_id)
    .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start_then(seed, async |case| {
        insert_second_stored_copy_of_build_b_output_b_into_cache_and_run_consistency_check(
            case,
            signing_cache_id,
            false,
        )
        .await;
    })
    .await;

    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "after 1: always sign the unsigned copy of B_OUT in the keyed cache."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn build_dispatched_before_publish_signs_other_project_input_into_read_write_cache_with_signing_key()
 {
    let (e1, other_project_input_output_upload_evaluation_id, signing_cache_id) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        CacheId::now_v7(),
    );
    let seed = dependent_a_on_completed_b(
        shape().evaluation(other_project_input_output_upload_evaluation_id),
        e1,
        BuildStatus::Created,
    )
    .into_cache(signing_cache_id)
    .keyed(SchedulerCase::signing_key());
    let case = SchedulerCase::start_with_unstored_outputs(seed, 1).await;

    let _ = tokio::time::timeout(
        Duration::from_millis(10),
        case.ctx.wake.run_signature_sweep_now.notified(),
    )
    .await;

    case.set_unstored_outputs(0);
    case.commit_nar(NarCommit {
        producer: case
            .make_worker_producer_for_evaluation_project(
                other_project_input_output_upload_evaluation_id,
            )
            .await,
        ..output_nar(B, B_OUT)
    })
    .await;
    assert!(
        case.db
            .signatures(B_OUT)
            .await
            .iter()
            .all(|row| row.cache != signing_cache_id),
        "after 1: A commit of B_OUT by a worker of another project does not write a signature row into the keyed ReadWrite cache of project E1"
    );
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            case.ctx.wake.run_signature_sweep_now.notified(),
        )
        .await
        .is_ok(),
        "after 1: the B_OUT commit must trigger the signature sweep within 1 second."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    run_dependency_output_publish_round_with_production_page_size(&case)
        .await
        .expect("the publish pass runs");
    case.settled().await;
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "after 2: one publish pass signs B_OUT into the keyed ReadWrite cache of project E1"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn test_keyed_read_write_cache_added_after_seed_reports_no_counter_drift() {
    let (e1, signing_cache_id) = (EvaluationId::now_v7(), CacheId::now_v7());
    let seed = build_a_depends_on_completed_build_b_with_output_copy_stored_by_other_project(
        e1,
        BuildStatus::Queued,
    )
    .into_cache(signing_cache_id);
    let case = SchedulerCase::start(seed).await;

    case.db
        .execute(&format!(
            "UPDATE cache SET private_key = '{}' WHERE id = '{signing_cache_id}'",
            SchedulerCase::signing_key()
        ))
        .await;
    run_dependency_output_publish_round_with_production_page_size(&case)
        .await
        .expect("the periodic graph consistency check runs");
    let graph_consistency_report = case
        .graph
        .run_consistency_checks()
        .await
        .expect("the periodic graph consistency check runs");
    case.settled().await;
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "after 1: check signs B_OUT into the cache that got a key after seed"
    );
    assert_eq!(
        graph_consistency_report.counter_drift, 0,
        "after 1: The check does not correct the blocking counter."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e1, "after 1").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn signature_sweep_signs_uploaded_input_and_assigns_queued_build_to_worker_once() {
    const MAX_PUBLISH_PASS_COUNT: usize = 1;
    let (e1, e2, b_out_upload_cache_id, signing_cache_id) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        CacheId::now_v7(),
        CacheId::now_v7(),
    );
    let seed =
        build_a_depends_on_completed_build_b_with_output_user_uploaded_to_other_projects_cache_not_read_by_a_project(
            e1,
            BuildStatus::Queued,
            b_out_upload_cache_id,
        )
        .into_cache(b_out_upload_cache_id)
        .into_cache(signing_cache_id)
        .keyed(SchedulerCase::signing_key())
        .evaluation(e2)
        .evaluation_status(EvaluationStatus::Building)
        .derivation(A)
        .entry_point(&attr_of(A))
        .job()
        .into_cache(signing_cache_id);
    let case = SchedulerCase::start(seed).await;
    let shared_build_a = case.sb(A).await;
    assert_eq!(
        (shared_build_a.status, shared_build_a.blocking_deps),
        (BuildStatus::Queued, 0),
        "Seed: Build A, status Queued, no blocking dependencies. None of the caches that project E2 can read hold a signed copy of B_OUT."
    );

    let mut previous_periodic_publish_pass_last_parent_build_derivation_id = None;
    for _ in 0..MAX_PUBLISH_PASS_COUNT {
        previous_periodic_publish_pass_last_parent_build_derivation_id = case
            .graph
            .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
                previous_periodic_publish_pass_last_parent_build_derivation_id,
                gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            )
            .await
            .last_handled_parent_build_id;
        case.settled().await;
    }
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "After the publish pass, signed B_OUT is in cache S of project E1."
    );
    assert_build_a_assigned_once_to_worker_w1_and_build_b_has_no_new_attempt_or_substitute_job_and_waiting_evaluation_has_no_message(&case, e2, "after the publish passes").await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn periodic_signing_pass_signs_other_project_dependency_outputs_into_keyed_signing_binary_cache_for_evaluation_finished_now_but_not_for_evaluation_finished_25_hours_ago()
 {
    let (e1, e2, signing_cache_id) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        CacheId::now_v7(),
    );
    let seed = dependent_a_on_completed_b(
        shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(B_OUT, "b")
            .references(&[(C_OUT, "c")])
            .cached_path(C_OUT, "c")
            .cached_path(D_OUT, "d")
            .cached_path(A_OUT, "a")
            .cached_path(X_OUT, "x"),
        e1,
        BuildStatus::Completed,
    )
    .evaluation_status(EvaluationStatus::Completed)
    .into_cache(signing_cache_id)
    .keyed(SchedulerCase::signing_key())
    .evaluation(e2)
    .evaluation_status(EvaluationStatus::Completed)
    .derivation(X)
    .walked()
    .probed()
    .drv_cached()
    .entry_point(&attr_of(X))
    .job()
    .output(X_OUT)
    .depends_on(D)
    .build_status(BuildStatus::Completed)
    .derivation(D)
    .walked()
    .probed()
    .drv_cached()
    .job()
    .output(D_OUT)
    .build_status(BuildStatus::Completed);
    let case = SchedulerCase::start(seed).await;
    let completed = gradient_db::sql::status::eval_in(&[EvaluationStatus::Completed]);
    case.db
        .execute(&format!(
            "UPDATE evaluation SET status = {completed}, \
                 finished_at = now() AT TIME ZONE 'UTC' \
             WHERE id = '{e1}'; \
             UPDATE evaluation SET status = {completed}, \
                 finished_at = (now() AT TIME ZONE 'UTC') - interval '25 hours', \
                 task = (SELECT task FROM evaluation WHERE id = '{e1}') \
             WHERE id = '{e2}'"
        ))
        .await;

    run_dependency_output_publish_round_with_production_page_size(&case)
        .await
        .expect("the periodic signing pass runs");
    case.settled().await;
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, B_OUT, signing_cache_id).await,
        "after 1: the periodic signing pass can sign B_OUT into S, the keyed cache of the project that finished evaluation a moment ago"
    );
    assert!(
        binary_cache_holds_signed_copy_of_store_path_hash(&case, C_OUT, signing_cache_id).await,
        "After 1: the periodic signing pass can also sign C_OUT into S. C_OUT is the store path of the reference in B_OUT."
    );
    assert!(
        case.db
            .signatures(D_OUT)
            .await
            .iter()
            .all(|row| row.cache != signing_cache_id),
        "after 1: D_OUT from an evaluation that finished 25 hours ago is outside the 24-hour window. There is no row in S."
    );

    case.finish().await;
}

#[tokio::test]
async fn periodic_consistency_check_rebuilds_completed_build_without_stored_output_before_handing_out_dependent()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with_unstored_outputs(
        dependent_a_on_completed_b(shape(), e1, BuildStatus::Created),
        1,
    )
    .await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    assert_ne!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: the check put build:B back in the queue again."
    );
    case.check("step 1").await;

    case.connect(W1, w_all()).await;
    let b1 = claim(&case, W1).await;
    case.commit_nar(output_nar(B, B_OUT)).await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.request_job(W1, JobKind::Build)
            .await
            .map(|assignment| assignment.job_id().to_owned()),
        Some(a),
        "after 3: assign build:A"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn substituted_build_with_lost_stored_copy_is_demoted_and_resubstituted_then_second_loss_within_24_hours_does_not_demote_but_loss_after_window_demotes_again()
 {
    let e1 = EvaluationId::now_v7();
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Created)
        .output_option(B_OUT, |o| {
            o.external_url = Some(format!("https://upstream.invalid/nar/{B_OUT}.nar"));
        })
        .build_status(BuildStatus::Substituted)
        .attempt(AttemptOutcome::Substituted, None);
    let case = SchedulerCase::start_with_unstored_outputs(seed, 1).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    let read_external_url_of_b_out = async || {
        derivation_output::Entity::find()
            .filter(derivation_output::Column::Hash.eq(B_OUT))
            .one(&case.db.connection())
            .await
            .expect("the output row of B_OUT is read")
            .expect("B_OUT has an output row")
            .external_url
    };

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    let status_of_b_after_demotion = case.sb(B).await.status;
    assert!(
        !matches!(
            status_of_b_after_demotion,
            BuildStatus::Completed | BuildStatus::Substituted
        ) && read_external_url_of_b_out().await.is_none(),
        "The check demoted build:B and cleared the upstream URL for build:B after step 1."
    );
    case.check("step 1").await;

    case.upstream_hits(&[(B_OUT, narinfo(&[(B_OUT, "b")]))])
        .await;
    case.upstream_probed(&[B]).await;
    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    let b1 = substitute(&case, W1, &b).await;
    case.report_building(W1, &b).await;
    case.report_build_output(W1, &b, vec![build_output(B, B_OUT)], true, None)
        .await;
    case.commit_nar(output_nar(B, B_OUT)).await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Substituted,
        "after 2: sb(B).status"
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.request_job(W1, JobKind::Build)
            .await
            .map(|assignment| assignment.job_id().to_owned()),
        Some(a),
        "After 3: assign build:A"
    );
    case.check("step 3").await;

    case.db
        .execute(&format!("DELETE FROM cached_path WHERE hash = '{B_OUT}'"))
        .await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(1);
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Substituted,
        "A second loss of the stored copy after 4 leaves build:B as substituted."
    );
    case.check("step 4").await;

    move_build_attempt_times_before_demotion_window(&case, B).await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    assert!(
        !matches!(
            case.sb(B).await.status,
            BuildStatus::Completed | BuildStatus::Substituted
        ),
        "The stored copy can disappear after the 24-hour window. That loss demoted build:B again after 5."
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn completed_unneeded_build_without_output_copy_stays_unbacked_after_periodic_check() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evaluation(e1)
        .evaluation_status(EvaluationStatus::Completed)
        .derivation(B)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(&attr_of(B))
        .job()
        .output(B_OUT)
        .build_status(BuildStatus::Completed);
    let case = SchedulerCase::start_with_unstored_outputs(seed, 1).await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: build:B stays completed"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn demotes_completed_builds_without_stored_output_one_per_pass_when_both_are_needed_by_waiting_dependent()
 {
    let e1 = EvaluationId::now_v7();
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Created)
        .derivation(A)
        .depends_on(C)
        .derivation(C)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(C_OUT)
        .build_status(BuildStatus::Completed);
    let case = SchedulerCase::start_with_unstored_outputs(seed, 2).await;
    let count_completed_builds_b_and_c = async || {
        let current_statuses_of_build_b_and_build_c =
            [case.sb(B).await.status, case.sb(C).await.status];
        current_statuses_of_build_b_and_build_c
            .iter()
            .filter(|status| **status == BuildStatus::Completed)
            .count()
    };

    let derivations_from_bounded_demotion_with_bound_1 =
        gradient_db::caches::demotion::demote_completed_builds_missing_needed_stored_output_copies(
            &case.ctx, 1,
        )
        .await
        .expect("Run demotion with an upper limit");
    case.set_unstored_outputs(1);
    assert_eq!(
        (
            derivations_from_bounded_demotion_with_bound_1.len(),
            count_completed_builds_b_and_c().await
        ),
        (1, 1),
        "after 1: a single pass at limit 1 can demote one of build:B and build:C"
    );
    case.check("step 1").await;

    let derivations_from_bounded_demotion_with_bound_1 =
        gradient_db::caches::demotion::demote_completed_builds_missing_needed_stored_output_copies(
            &case.ctx, 1,
        )
        .await
        .expect("Run demotion with an upper limit");
    case.set_unstored_outputs(0);
    assert_eq!(
        (
            derivations_from_bounded_demotion_with_bound_1.len(),
            count_completed_builds_b_and_c().await
        ),
        (1, 0),
        "after 2: demote the other on the next pass"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn periodic_check_keeps_completed_build_rebuilt_with_upload_while_two_successful_attempts_are_within_24_hour_window_then_requeues_it_when_stored_copy_is_lost_after_completion()
 {
    let e1 = EvaluationId::now_v7();
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Created)
        .attempt(AttemptOutcome::Built, None);
    let case = SchedulerCase::start_with_unstored_outputs(seed, 1).await;
    let b = case.key(B).await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    case.connect(W1, w_all()).await;
    let b1 = claim(&case, W1).await;
    case.report_building(W1, &b).await;
    case.commit_nar(output_nar(B, B_OUT)).await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: build:B completed again once the rebuild uploaded its output"
    );
    case.check("step 1").await;

    case.db
        .execute(&format!("DELETE FROM cached_path WHERE hash = '{B_OUT}'"))
        .await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(1);
    assert_eq!(
        (case.sb(B).await.status, case.attempts(B).await.len()),
        (BuildStatus::Completed, 2),
        "after 2: build:B lost the stored copy after completion. build:B stayed in completed state with 2 successful attempts after the check."
    );
    case.check("step 2").await;

    move_build_attempt_times_before_demotion_window(&case, B).await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    assert_ne!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 3: the check put build:B back in the queue when the 24-hour window ended."
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn move_build_attempt_times_before_demotion_window(case: &SchedulerCase, derivation: &str) {
    let demotion_window_plus_one_hour_seconds =
        gradient_db::caches::demotion::DEMOTION_MIN_TIME_SINCE_LAST_SUCCESSFUL_BUILD.as_secs()
            + 3600;
    let derivation_build = case.sb(derivation).await.id;
    case.db
        .execute(&format!(
            "UPDATE build_attempt \
             SET build_finished_at = build_finished_at \
                     - interval '{demotion_window_plus_one_hour_seconds} seconds', \
                 created_at = created_at - interval '{demotion_window_plus_one_hour_seconds} seconds' \
             WHERE derivation_build = '{derivation_build}'"
        ))
        .await;
}

async fn move_build_attempt_times_inside_demotion_window(case: &SchedulerCase, derivation: &str) {
    let derivation_build = case.sb(derivation).await.id;
    case.db
        .execute(&format!(
            "UPDATE build_attempt \
             SET build_finished_at = now() AT TIME ZONE 'UTC', \
                 created_at = now() AT TIME ZONE 'UTC' \
             WHERE derivation_build = '{derivation_build}'"
        ))
        .await;
}

fn seed_created_build_a_depends_on_completed_build_b_with_copied_output_referencing_completed_build_c_without_stored_copy(
    evaluation: EvaluationId,
) -> Shape {
    dependent_a_on_completed_b(shape(), evaluation, BuildStatus::Queued)
        .attempt(AttemptOutcome::Built, None)
        .attempt(AttemptOutcome::Built, None)
        .depends_on(C)
        .cached_path(B_OUT, "b")
        .references(&[(C_OUT, "c")])
        .derivation(C)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(C_OUT)
        .build_status(BuildStatus::Completed)
}

#[tokio::test]
async fn periodic_consistency_check_keeps_completed_build_with_two_recent_successful_attempts_until_24_hour_window_passes_then_demotes_it_and_its_completed_dependency_without_stored_output_then_worker_rebuilds_dependency_then_build_and_hands_out_dependent()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start_with_unstored_outputs(
        seed_created_build_a_depends_on_completed_build_b_with_copied_output_referencing_completed_build_c_without_stored_copy(e1),
        1,
    )
    .await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);

    move_build_attempt_times_inside_demotion_window(&case, B).await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(B_OUT).await.is_some()
        ),
        (BuildStatus::Completed, true),
        "After 1: two successful attempts within a 24-hour window keep build:B in completed state with the stored copy."
    );
    case.check("step 1").await;

    move_build_attempt_times_before_demotion_window(&case, B).await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status != BuildStatus::Completed,
            case.db.cached_path(B_OUT).await.is_none(),
            case.sb(C).await.status,
            case.sb(A).await.status
        ),
        (true, true, BuildStatus::Completed, BuildStatus::Created),
        "The check after 2 put build:B back in the queue and deleted the stored copy. build:C stayed complete. build:A waited again."
    );
    case.check("step 2").await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    case.set_unstored_outputs(0);
    assert_ne!(
        case.sb(C).await.status,
        BuildStatus::Completed,
        "After 3. The check put `build:C` back in the queue"
    );
    case.check("step 3").await;

    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    let c1 = take(&case, W1, &c).await;
    case.commit_nar(output_nar(C, C_OUT)).await;
    case.complete(W1, &c, c1, Vec::new(), 5).await;
    assert_eq!(
        (
            case.sb(C).await.status,
            case.db.cached_path(C_OUT).await.is_some()
        ),
        (BuildStatus::Completed, true),
        "After 4: build:C completed with a saved copy of its output."
    );
    case.check("step 4").await;

    case.admit_startable_moves().await;
    let b1 = take(&case, W1, &b).await;
    case.commit_nar(with_references(output_nar(B, B_OUT), &[(C_OUT, "c")]))
        .await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db
                .cached_path(B_OUT)
                .await
                .and_then(|stored| stored.references)
                .is_some_and(|references| references.contains(C_OUT))
        ),
        (BuildStatus::Completed, true),
        "After 5: build:B finished with a stored copy. The references in that copy point to the output of build:C."
    );
    case.check("step 5").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.request_job(W1, JobKind::Build)
            .await
            .map(|assignment| assignment.job_id().to_owned()),
        Some(a),
        "after 6: assign build:A"
    );
    case.check("step 6").await;

    case.finish().await;
}

fn make_test_seed_with_queued_dependent_build_status_and_attempt_outcome_and_source_store_path_without_stored_copy(
    evaluation: EvaluationId,
    test_build_b_starting_status: BuildStatus,
    recorded_test_build_b_attempt_outcome: AttemptOutcome,
) -> Shape {
    dependent_a_on_completed_b(shape(), evaluation, BuildStatus::Queued)
        .build_status(test_build_b_starting_status)
        .attempt(recorded_test_build_b_attempt_outcome, None)
        .cached_path(B_OUT, "b")
        .references(&[(S_OUT, "source")])
}

#[tokio::test]
async fn periodic_check_demotes_build_with_missing_source_copy_and_rebuild_uploads_it() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        make_test_seed_with_queued_dependent_build_status_and_attempt_outcome_and_source_store_path_without_stored_copy(
            e1,
            BuildStatus::Completed,
            AttemptOutcome::Built,
        ),
    )
    .await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status != BuildStatus::Completed,
            case.db.cached_path(B_OUT).await.is_none(),
            case.sb(A).await.status
        ),
        (true, true, BuildStatus::Created),
        "The check queued build:B again after step 1 and deleted the stored copy of its output. build:A waited again."
    );
    case.check("step 1").await;

    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    let b1 = take(&case, W1, &b).await;
    case.commit_nar(nar(S_OUT, "source")).await;
    case.commit_nar(with_references(output_nar(B, B_OUT), &[(S_OUT, "source")]))
        .await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(S_OUT).await.is_some(),
            case.db
                .cached_path(B_OUT)
                .await
                .and_then(|stored| stored.references)
                .is_some_and(|references| references.contains(S_OUT))
        ),
        (BuildStatus::Completed, true, true),
        "build:B finished after step 2. Stored copies exist for the source path and the output of build:B. The source path is the target of a reference from the copy of that output."
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.request_job(W1, JobKind::Build)
            .await
            .map(|assignment| assignment.job_id().to_owned()),
        Some(a),
        "Assign build:A after 3"
    );
    case.check("step 3").await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(B_OUT).await.is_some()
        ),
        (BuildStatus::Completed, true),
        "After 4: further checks leave build:B completed with a saved copy."
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn test_upstream_substituted_build_missing_source_store_path_demoted_again_only_after_24_hour_window()
 {
    let e1 = EvaluationId::now_v7();
    let seed = make_test_seed_with_queued_dependent_build_status_and_attempt_outcome_and_source_store_path_without_stored_copy(
        e1,
        BuildStatus::Substituted,
        AttemptOutcome::Substituted,
    )
    .output_option(B_OUT, |o| {
        o.external_url = Some(format!("https://upstream.invalid/nar/{B_OUT}.nar"));
    });
    let case = SchedulerCase::start(seed).await;
    let b = case.key(B).await;
    let build_b_is_not_completed_or_substituted = async || {
        !matches!(
            case.sb(B).await.status,
            BuildStatus::Completed | BuildStatus::Substituted
        )
    };

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert!(
        build_b_is_not_completed_or_substituted().await
            && case.db.cached_path(B_OUT).await.is_none(),
        "The check demoted the substituted `build:B` after 1 and deleted the saved copy of its output."
    );
    case.check("step 1").await;

    case.upstream_hits(&[(B_OUT, narinfo(&[(S_OUT, "source")]))])
        .await;
    case.upstream_probed(&[B]).await;
    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    let b1 = substitute(&case, W1, &b).await;
    case.report_building(W1, &b).await;
    case.report_build_output(W1, &b, vec![build_output(B, B_OUT)], true, None)
        .await;
    case.commit_nar(with_references(output_nar(B, B_OUT), &[(S_OUT, "source")]))
        .await;
    case.complete(W1, &b, b1, Vec::new(), 5).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(B_OUT).await.is_some(),
            case.db.cached_path(S_OUT).await.is_none()
        ),
        (BuildStatus::Substituted, true, true),
        "After 2: build:B gets a substitute again. The copy returns. The source paths that the copy references still have no saved copy."
    );
    case.check("step 2").await;

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(B_OUT).await.is_some()
        ),
        (BuildStatus::Substituted, true),
        "The second check within the 24-hour window after 3 does not demote build:B. build:B does not lose its stored copy."
    );
    case.check("step 3").await;

    move_build_attempt_times_before_demotion_window(&case, B).await;
    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert!(
        build_b_is_not_completed_or_substituted().await,
        "After 4: the 24-hour window ended. The check put build:B back in the queue."
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn periodic_consistency_check_does_not_demote_build_with_output_referencing_pending_producer_and_hands_out_dependent_after_producer_commit()
 {
    let e1 = EvaluationId::now_v7();
    let seed = dependent_a_on_completed_b(shape(), e1, BuildStatus::Created)
        .attempt(AttemptOutcome::Built, None)
        .runtime_depends_on(C)
        .cached_path(B_OUT, "b")
        .references(&[(C_OUT, "c")])
        .derivation(C)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(C_OUT)
        .build_status(BuildStatus::Queued);
    let case = SchedulerCase::start(seed).await;
    let (a, c) = (case.key(A).await, case.key(C).await);

    run_periodic_graph_consistency_check_and_wait_for_scheduler_to_settle(&case).await;
    assert_eq!(
        (
            case.sb(B).await.status,
            case.db.cached_path(B_OUT).await.is_some(),
            case.sb(C).await.status
        ),
        (BuildStatus::Completed, true, BuildStatus::Queued),
        "The check after 1 found build:B in the completed state. build:B kept the saved copy. build:C was still in the queue."
    );
    case.check("step 1").await;

    case.connect(W1, w_all()).await;
    case.admit_startable_moves().await;
    let c1 = take(&case, W1, &c).await;
    case.commit_nar(output_nar(C, C_OUT)).await;
    case.complete(W1, &c, c1, Vec::new(), 5).await;
    assert_eq!(
        (
            case.sb(C).await.status,
            case.db.cached_path(C_OUT).await.is_some()
        ),
        (BuildStatus::Completed, true),
        "After 2: build:C completed with a saved copy of the output."
    );
    case.check("step 2").await;

    case.admit_startable_moves().await;
    assert_eq!(
        case.request_job(W1, JobKind::Build)
            .await
            .map(|assignment| assignment.job_id().to_owned()),
        Some(a),
        "Assign build:A after 3"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn aborted_waiting_evaluation_does_not_hand_out_dependent_build_and_keeps_input_build_completed()
 {
    let (e1, other) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = dependent_a_on_completed_b(
        shape().evaluation(other).cached_path(B_OUT, "b"),
        e1,
        BuildStatus::Queued,
    )
    .evaluation(other)
    .evaluation_status(EvaluationStatus::Completed)
    .derivation(B)
    .job();
    let case = SchedulerCase::start(seed).await;
    let a = case.key(A).await;

    case.admit_startable_moves().await;
    assert!(case.pending(&a).await, "after 1: build:A wait for a worker");
    case.check("step 1").await;

    case.abort_evaluation(e1).await;
    assert_ne!(
        build_handed_to_w1(&case).await,
        Some(a),
        "after 2: build:A does not appear in the distribution after E1 stopped."
    );
    assert_eq!(
        (case.sb(B).await.status, case.job(other, B).await.is_some()),
        (BuildStatus::Completed, true),
        "after 2: build:B still Completed in the other evaluation."
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r13_a_canceled_build_closes_the_row_abandoned_and_requeues_without_an_attempt() {
    let case = SchedulerCase::start(shape().one(EvaluationId::now_v7(), A, A_OUT)).await;
    case.connect(W1, w_all()).await;
    case.connect(W2, w_all()).await;
    let a = case.key(A).await;
    let attempt = case.sb(A).await.attempt;

    let a1 = claim(&case, W1).await;
    case.report_building(W1, &a).await;
    case.signals(W2);
    case.check("step 1").await;

    let canceled = failure(BuildFailureKind::Canceled)
        .error("worker draining")
        .elapsed_ms(3);
    case.fail(W1, &a, a1, canceled).await;
    assert_eq!(
        elapsed(&case, &a).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned), Some(3))],
        "after 2: dj(build:A)"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Aborted],
        "after 2: att(A)"
    );
    let shared = case.sb(A).await;
    assert_eq!(
        (shared.status, shared.attempt),
        (BuildStatus::Queued, attempt),
        "after 2: sb(A)"
    );
    assert!(!case.pending(&a).await, "after 2: pending(build:A)");
    assert_eq!(case.signals(W2), vec![], "after 2: signals(W2)");
    case.check("step 2").await;

    case.admit_startable_moves().await;
    let taken = case.request_job(W2, JobKind::Build).await;
    assert_eq!(
        taken.as_ref().map(|j| j.job_id().to_owned()),
        Some(a.clone()),
        "after 3: result.job_id"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Aborted],
        "after 3: att(A)"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r14_a_canceled_eval_job_puts_the_evaluation_back_to_queued() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingFlake)
        .await;
    case.check("step 0").await;

    case.fail(
        W1,
        &k,
        a1,
        failure(BuildFailureKind::Canceled).elapsed_ms(3),
    )
    .await;
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Abandoned))],
        "after 1: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        evaluation.status,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert!(
        evaluation.eval_flake_started_at.is_some(),
        "after 1: eval_flake_started_at kept"
    );
    assert_eq!(case.active(&k).await, None, "after 1: active(eval:E1)");
    case.signals(W1);
    case.check("step 1").await;

    case.assign_queued_evals().await;
    assert!(case.pending(&k).await, "after 2: pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 2: signals(W1)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn a_corruption_within_the_heal_window_fails_instead_of_healing_again() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(
        shape()
            .evalq(e1)
            .into_cache(CacheId::now_v7())
            .eval_cache("F"),
    )
    .await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    case.fail(W1, &k, a1, corrupt()).await;
    let row = case.eval_cache("F").await.expect("after 1: row F kept");
    assert!(row.purged_at.is_some(), "after 1: row F purged");
    assert_eq!(row.size_bytes, 0, "after 1: row F emptied");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 1: evaluation(E1).status"
    );
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Failed))],
        "after 1: dj(eval:E1)"
    );
    case.check("step 1").await;

    set_purged_at(&case, "F", Duration::from_secs(2 * 3600)).await;
    let a2 = evaluate_again(&case, &k).await;
    case.fail(W1, &k, a2, corrupt()).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Queued,
        "after 2: a purge 2 h ago heals again"
    );
    case.check("step 2").await;

    set_purged_at(&case, "F", Duration::from_secs(60)).await;
    let a3 = evaluate_again(&case, &k).await;
    case.fail(W1, &k, a3, corrupt()).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 3: a purge 1 min ago fails the evaluation"
    );
    assert!(
        has_error(&case, e1).await,
        "after 3: evaluation_message(E1) has an Error"
    );
    assert_eq!(
        case.assignment_outcomes(&k).await.get(2).cloned(),
        Some((a3, Some(DispatchedJobOutcome::Failed))),
        "after 3: dj(eval:E1)[2]"
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn evaluate_again(case: &SchedulerCase, key: &str) -> DispatchedJobId {
    case.assign_queued_evals().await;
    let assignment = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("W1 gets the evaluation again")
        .assignment_id();
    case.advance_evaluation(W1, key, EvaluationStatus::EvaluatingDerivation)
        .await;
    assignment
}

async fn set_purged_at(case: &SchedulerCase, fingerprint: &str, ago: Duration) {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let purged_at = gradient_types::now() - chrono::Duration::from_std(ago).unwrap();
    gradient_entity::eval_cache_store::Entity::update_many()
        .col_expr(
            gradient_entity::eval_cache_store::Column::PurgedAt,
            Expr::value(purged_at),
        )
        .filter(gradient_entity::eval_cache_store::Column::Fingerprint.eq(fingerprint))
        .exec(&case.db.connection())
        .await
        .expect("the purge time writes");
}

#[tokio::test]
async fn r16_status_reports_move_the_evaluation_forward_and_skip_a_backwards_move() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evalq(e1)
        .evaluation_status(EvaluationStatus::Waiting)
        .waiting_reason(WaitingReason::eval_workers(EvalCapability::Eval, 0).to_json());
    let case = SchedulerCase::start(seed).await;
    let k = eval_key(e1);
    case.connect(W1, w_all()).await;
    case.refresh_waiting_state().await;
    case.assign_queued_evals().await;
    case.request_job(W1, JobKind::Flake)
        .await
        .expect("after 0: W1 gets eval:E1");
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.waiting_reason),
        (EvaluationStatus::Queued, None),
        "after 0: evaluation(E1)"
    );
    case.check("step 0").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.fetch_started_at.is_some()),
        (EvaluationStatus::Fetching, true),
        "after 1: {{Fetching, fetch_started_at set}}"
    );
    case.check("step 1").await;

    case.report_status(W1, &k, EvaluationStatus::EvaluatingFlake)
        .await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (
            evaluation.status,
            evaluation.eval_flake_started_at.is_some()
        ),
        (EvaluationStatus::EvaluatingFlake, true),
        "after 2: {{EvaluatingFlake, eval_flake_started_at set}}"
    );
    case.check("step 2").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    assert_eq!(
        case.evaluation(e1).await,
        evaluation,
        "after 3: evaluation(E1) unchanged"
    );
    case.check("step 3").await;

    case.report_status(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.eval_drv_started_at.is_some()),
        (EvaluationStatus::EvaluatingDerivation, true),
        "after 4: {{EvaluatingDerivation, eval_drv_started_at set}}"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r17_a_completed_fetch_job_enqueues_the_cached_follow_up_for_an_eval_worker() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    case.connect(W1, w_network_eval()).await;
    case.connect(W2, w_eval()).await;

    case.assign_queued_evals().await;
    assert!(case.pending(&k).await, "after 1: pending(eval:E1)");
    assert_eq!(
        case.pending_flake_job(&k).await.map(|j| j.steps),
        Some(vec![FlakeStep::FetchFlake]),
        "after 1: steps of the pending job"
    );
    case.check("step 1").await;

    let nothing = case.request_job(W2, JobKind::Flake).await;
    assert!(nothing.is_none(), "after 2: W2 gets nothing");
    let fetch = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 2: W1 gets eval:E1");
    let a1 = fetch.assignment_id();
    let Job::Flake(job) = &fetch.job else {
        panic!("after 2: a flake job, got {:?}", fetch.job);
    };
    assert_eq!(
        job.steps,
        vec![FlakeStep::FetchFlake],
        "after 2: result.job.steps"
    );
    assert!(
        matches!(job.source, FlakeSource::Repository { .. }),
        "after 2: result.job.source is Repository, got {:?}",
        job.source
    );
    let fetched_commit = job.source.commit().map(str::to_owned);
    case.check("step 2").await;

    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    case.report_flake_source(W1, &k, SOURCE).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.flake_source.as_deref()),
        (EvaluationStatus::Fetching, Some(SOURCE)),
        "after 3: evaluation(E1)"
    );
    case.signals(W2);
    case.check("step 3").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 900)], 900)
        .await;
    assert_eq!(
        elapsed(&case, &k).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed), Some(900))],
        "after 4: dj(eval:E1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Fetching,
        "after 4: evaluation(E1).status"
    );
    let follow_up = case.pending_job(&k).await;
    let Some(PendingJob::Eval(follow_up)) = follow_up else {
        panic!("after 4: pending(eval:E1) is an eval job, got {follow_up:?}");
    };
    assert_eq!(
        follow_up.job.steps,
        vec![FlakeStep::EvaluateFlake, FlakeStep::EvaluateDerivations],
        "after 4: steps of the follow-up"
    );
    assert_eq!(
        follow_up.job.source,
        FlakeSource::Cached {
            store_path: SOURCE.to_owned(),
            commit: fetched_commit,
        },
        "after 4: source of the follow-up"
    );
    assert_eq!(
        *follow_up.shared_direct_input_store_paths_with_probe_words_and_nar_sizes,
        [PendingJobInputStorePath {
            store_path_bloom_filter_probe_words: decode_store_path_hash_into_two_u64_words(
                SOURCE_HASH
            ),
            nar_size: 0,
        }],
        "after 4: direct inputs of the follow-up"
    );
    assert_eq!(
        case.evaluation_metric(e1).await.map(|m| m.fetch_ms),
        Some(900),
        "after 4: evaluation_metric(E1).fetch_ms"
    );
    assert_eq!(
        case.signals(W2),
        vec![jobs_available_signal_for_one_kind(JobKind::Flake)],
        "after 4: signals(W2)"
    );
    case.check("step 4").await;

    let a2 = case
        .request_job(W2, JobKind::Flake)
        .await
        .expect("after 5: W2 gets eval:E1")
        .assignment_id();
    let rows: Vec<_> = case
        .assignments(&k)
        .await
        .into_iter()
        .map(|row| (row.id, row.outcome, row.worker_id))
        .collect();
    assert_eq!(
        rows,
        vec![
            (a1, Some(DispatchedJobOutcome::Completed), W1.to_owned()),
            (a2, None, W2.to_owned()),
        ],
        "after 5: dj(eval:E1)"
    );
    case.check("step 5").await;

    case.finish().await;
}

#[tokio::test]
async fn r18_a_completed_fetch_job_without_a_flake_source_fails_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    case.connect(W1, w_network_eval()).await;
    case.connect(W2, w_eval()).await;

    case.assign_queued_evals().await;
    let a1 = case
        .request_job(W1, JobKind::Flake)
        .await
        .expect("after 1: W1 gets eval:E1")
        .assignment_id();
    case.report_status(W1, &k, EvaluationStatus::Fetching).await;
    case.check("step 1").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 100)], 100)
        .await;
    assert_eq!(
        case.assignment_outcomes(&k).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(eval:E1)"
    );
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.finished_at.is_some()),
        (EvaluationStatus::Failed, true),
        "after 2: evaluation(E1)"
    );
    assert!(
        has_error(&case, e1).await,
        "after 2: evaluation_message(E1) has an Error"
    );
    assert!(!case.pending(&k).await, "after 2: pending(eval:E1)");
    case.signals(W1);
    case.signals(W2);
    case.check("step 2").await;

    case.assign_queued_evals().await;
    assert!(!case.pending(&k).await, "after 3: pending(eval:E1)");
    assert_eq!(case.signals(W1), vec![], "after 3: signals(W1)");
    assert_eq!(case.signals(W2), vec![], "after 3: signals(W2)");
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r19_an_eval_completion_moves_to_building_or_straight_to_the_verdict() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let (k1, k2) = (eval_key(e1), eval_key(e2));
    let a1 = case.evalrun(W1, e1).await;
    let a2 = case.evalrun(W2, e2).await;
    case.advance_evaluation(W1, &k1, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.advance_evaluation(W2, &k2, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let found = with_attr(with_output(drv(A, &[]), A_OUT), &attr_of(A));
    let derivation_a_drv_base_store_path = test_derivation_store_file_name(&found);
    case.report_eval_result(W1, &k1, vec![found]).await;
    case.upstream_probed(&[&derivation_a_drv_base_store_path])
        .await;
    assert_eq!(
        case.sb(&derivation_a_drv_base_store_path).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.counters(e1).await,
        [1, 1, 0, 1, 0],
        "after 1: counters(E1)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::EvaluatingDerivation,
        "after 1: evaluation(E1).status"
    );
    case.check("step 1").await;

    case.report_eval_stats(W1, &k1, EvalStatsReport::default())
        .await;
    let spans = vec![
        span(JobPhase::Fetch, 0, 1000),
        span(JobPhase::EvalFlake, 1000, 2000),
        span(JobPhase::EvalDerivations, 2000, 5000),
    ];
    case.complete(W1, &k1, a1, spans, 5000).await;
    let evaluation = case.evaluation(e1).await;
    assert_eq!(
        (evaluation.status, evaluation.building_started_at.is_some()),
        (EvaluationStatus::Building, true),
        "after 2: evaluation(E1)"
    );
    assert_eq!(
        case.evaluation_metric(e1)
            .await
            .map(|m| (m.fetch_ms, m.eval_flake_ms, m.eval_drv_ms)),
        Some((1000, 1000, 3000)),
        "after 2: evaluation_metric(E1) phase totals"
    );
    assert_eq!(
        case.assignment_outcomes(&k1).await,
        vec![(a1, Some(DispatchedJobOutcome::Completed))],
        "after 2: dj(eval:E1)"
    );
    case.check("step 2").await;

    case.complete(W2, &k2, a2, Vec::new(), 100).await;
    let evaluation = case.evaluation(e2).await;
    assert_eq!(
        (evaluation.status, evaluation.finished_at.is_some()),
        (EvaluationStatus::Completed, true),
        "after 3: evaluation(E2)"
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(
    case: &SchedulerCase,
    evaluation: EvaluationId,
) -> String {
    let key = eval_key(evaluation);
    case.evalrun(W1, evaluation).await;
    case.advance_evaluation(W1, &key, EvaluationStatus::EvaluatingDerivation)
        .await;
    key
}

fn build_discovered_derivation_with_one_output(path: &str, out: &str) -> DiscoveredDerivation {
    with_attr(with_output(drv(path, &[]), out), &attr_of(path))
}

async fn assert_derivation_file_not_uploaded_and_no_derivation_row_exists(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    refused: &str,
) {
    assert_eq!(
        case.evaluation(evaluation).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation.status"
    );
    let expected = format!("derivation file {refused} was not uploaded or does not match its path");
    assert!(
        case.messages(evaluation)
            .await
            .iter()
            .any(|m| m.level == MessageLevel::Error && m.message == expected),
        "after 1: messages contain {expected:?}"
    );
    let connection = case.db.connection();
    assert_eq!(
        (
            derivation::Entity::find().count(&connection).await.unwrap(),
            derivation_output::Entity::find()
                .count(&connection)
                .await
                .unwrap(),
            derivation_build::Entity::find()
                .count(&connection)
                .await
                .unwrap(),
        ),
        (0, 0, 0),
        "after 1: derivation, output and build rows"
    );
}

#[tokio::test]
async fn worker_reported_facts_do_not_replace_uploaded_derivation_file_facts_and_build_stays_unsubstituted()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1).cached_path(B_OUT, "b")).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let project = case.db.project_of(e1).await;
    let c = case
        .upload_reported_derivation_as_evaluation_project_worker(
            project,
            &build_discovered_derivation_with_one_output(C, C_OUT),
        )
        .await;
    let a = case
        .upload_reported_derivation_as_evaluation_project_worker(
            project,
            &build_discovered_derivation_with_one_output(A, A_OUT),
        )
        .await;
    let (a_base, c_base) = (
        test_derivation_store_file_name(&build_discovered_derivation_with_one_output(A, A_OUT)),
        test_derivation_store_file_name(&build_discovered_derivation_with_one_output(C, C_OUT)),
    );
    let forged = DiscoveredDerivation {
        architecture: ARM.into(),
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: format!("/nix/store/{B_OUT}-b"),
        }],
        dependencies: vec![c.drv_path.clone()],
        dependency_outputs: vec![vec!["out".into()]],
        required_features: vec!["kvm".into()],
        timeout_secs: Some(5),
        max_silent_secs: Some(7),
        prefer_local_build: true,
        ..a.clone()
    };

    case.scheduler
        .handle_eval_result(&k1, vec![c, forged], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");
    case.upstream_probed(&[&a_base, &c_base]).await;
    assert_eq!(
        case.sb(&a_base).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    let recorded = case.db.derivation(&a_base).await;
    assert_eq!(
        (
            recorded.architecture.as_str(),
            recorded.timeout_secs,
            recorded.max_silent_secs,
            recorded.prefer_local_build,
        ),
        (X86, None, None, false),
        "after 1: derivation(A) facts"
    );
    assert_eq!(
        case.output_of(&a_base, "out").await.hash,
        Some(computed_out_output_path_hash(
            &build_discovered_derivation_with_one_output(A, A_OUT)
        )),
        "after 1: output_of(A).hash"
    );
    let connection = case.db.connection();
    assert_eq!(
        derivation_output::Entity::find()
            .filter(derivation_output::Column::Hash.eq(B_OUT))
            .count(&connection)
            .await
            .unwrap(),
        0,
        "after 1: outputs with hash B_OUT"
    );
    assert!(
        case.db.edge(&a_base, &c_base).await.is_none(),
        "after 1: edge(A, C)"
    );
    assert_eq!(
        derivation_feature::Entity::find()
            .filter(derivation_feature::Column::Derivation.eq(recorded.id))
            .count(&connection)
            .await
            .unwrap(),
        0,
        "after 1: features(A)"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn dispatch_uses_limits_from_verified_drv_file_instead_of_reported_limits() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let limited = DiscoveredDerivation {
        timeout_secs: Some(100),
        max_silent_secs: Some(50),
        ..build_discovered_derivation_with_one_output(A, A_OUT)
    };
    let a_base = test_derivation_store_file_name(&limited);
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    case.walk_evaluation_derivations_and_wait(e2, vec![limited.clone()])
        .await;

    let project = case.db.project_of(e1).await;
    let uploaded = case
        .upload_reported_derivation_as_evaluation_project_worker(project, &limited)
        .await;
    let forged = DiscoveredDerivation {
        timeout_secs: Some(5),
        max_silent_secs: Some(7),
        ..uploaded
    };
    case.scheduler
        .handle_eval_result(&k1, vec![forged], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");
    case.upstream_probed(&[&a_base]).await;

    case.connect(W2, w_all()).await;
    case.admit_startable_moves().await;
    let assignment = case
        .request_job(W2, JobKind::Build)
        .await
        .expect("after 2: W2 gets build:A");
    let Job::Build(build) = &assignment.job else {
        panic!("after 2: a build job, got {:?}", assignment.job);
    };
    assert_eq!(
        (build.build.timeout_secs, build.build.max_silent_secs),
        (Some(100), Some(50)),
        "after 2: dispatched limits of A"
    );

    case.finish().await;
}

#[tokio::test]
async fn evaluation_with_unuploaded_derivation_fails_and_records_no_report_derivation() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let project = case.db.project_of(e1).await;
    let a = case
        .upload_reported_derivation_as_evaluation_project_worker(
            project,
            &build_discovered_derivation_with_one_output(A, A_OUT),
        )
        .await;
    let b_base =
        test_derivation_store_file_name(&build_discovered_derivation_with_one_output(B, B_OUT));
    let never_uploaded = DiscoveredDerivation {
        drv_path: format!("/nix/store/{b_base}"),
        ..build_discovered_derivation_with_one_output(B, B_OUT)
    };

    case.scheduler
        .handle_eval_result(&k1, vec![a, never_uploaded], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");
    assert_derivation_file_not_uploaded_and_no_derivation_row_exists(&case, e1, &b_base).await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn evaluation_fails_and_records_nothing_when_derivation_only_has_untrusted_content_variant() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let other_project = case.db.project_of(e2).await;
    let a = case
        .upload_reported_derivation_as_evaluation_project_worker(
            other_project,
            &build_discovered_derivation_with_one_output(A, A_OUT),
        )
        .await;

    case.scheduler
        .handle_eval_result(&k1, vec![a], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");
    assert_derivation_file_not_uploaded_and_no_derivation_row_exists(
        &case,
        e1,
        &test_derivation_store_file_name(&build_discovered_derivation_with_one_output(A, A_OUT)),
    )
    .await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn test_stored_derivation_text_not_matching_path_hash_fails_and_records_nothing() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let project = case.db.project_of(e1).await;
    let a = case
        .upload_reported_derivation_as_evaluation_project_worker(
            project,
            &build_discovered_derivation_with_one_output(A, A_OUT),
        )
        .await;
    let a_base =
        test_derivation_store_file_name(&build_discovered_derivation_with_one_output(A, A_OUT));
    let stored = case
        .cached_path(a_base.split_once('-').expect("a store base path").0)
        .await
        .expect("the uploaded derivation file is stored");
    let other_text = DerivationFileFromReportedFacts::from_reported_derivation_facts(
        &build_discovered_derivation_with_one_output(B, B_OUT),
    )
    .text;
    case.ctx
        .storage
        .nar_storage
        .put(
            &stored.nar_object_storage_key,
            zstd_compressed_nar_of_bytes(&other_text).await,
        )
        .await
        .expect("the replaced NAR is stored");

    case.scheduler
        .handle_eval_result(&k1, vec![a], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");
    assert_derivation_file_not_uploaded_and_no_derivation_row_exists(&case, e1, &a_base).await;
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn trusted_report_with_another_derivation_output_path_fails_without_derivation_rows() {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(shape().evalq(e1).evalq(e2)).await;
    let honest = build_discovered_derivation_with_one_output(B, B_OUT);
    let b_base = test_derivation_store_file_name(&honest);
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    case.evalrun(W2, e2).await;
    case.advance_evaluation(W2, &eval_key(e2), EvaluationStatus::EvaluatingDerivation)
        .await;
    case.scheduler
        .handle_eval_result(
            &eval_key(e2),
            vec![
                case.upload_reported_derivation_as_evaluation_project_worker(
                    case.db.project_of(e2).await,
                    &honest,
                )
                .await,
            ],
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("the eval result is handled");
    case.upstream_probed(&[&b_base]).await;
    let honest_file = DerivationFileFromReportedFacts::from_reported_derivation_facts(&honest);
    let forged_derivation_file = DerivationFileFromReportedFacts::from_drv_text_and_reported_facts(
        String::from_utf8(honest_file.text.clone())
            .expect("derivation text is valid UTF-8")
            .replacen("(\"name\",", "(\"extra\",\"1\"),(\"name\",", 1)
            .into_bytes(),
        &honest_file.derivation,
    );
    assert_ne!(
        forged_derivation_file.text, honest_file.text,
        "before 1: hand-written derivation text differs"
    );
    let forged_hash =
        gradient_entity::StorePath::parse_stored(&forged_derivation_file.derivation.drv_path)
            .expect("a stored derivation path")
            .hash()
            .to_owned();
    let connection = case.db.connection();
    let honest_derivation_b_and_output_rows_before_forged_report = (
        case.db.derivation(&b_base).await,
        case.output_of(&b_base, "out").await,
        derivation_output::Entity::find()
            .count(&connection)
            .await
            .unwrap(),
    );

    let project = case.db.project_of(e1).await;
    let forged = case
        .upload_derivation_file_as_project_worker_content_variant(project, forged_derivation_file)
        .await;
    case.scheduler
        .handle_eval_result(&k1, vec![forged], Vec::new(), Vec::new())
        .await
        .expect("the eval result is handled");

    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation.status"
    );
    assert_eq!(
        derivation::Entity::find()
            .filter(derivation::Column::Hash.eq(forged_hash))
            .count(&connection)
            .await
            .unwrap(),
        0,
        "after 1: derivation row for fake path"
    );
    assert_eq!(
        (
            case.db.derivation(&b_base).await,
            case.output_of(&b_base, "out").await,
            derivation_output::Entity::find()
                .count(&connection)
                .await
                .unwrap(),
        ),
        honest_derivation_b_and_output_rows_before_forged_report,
        "after 1: rows of B and output row count"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn r20_eval_messages_are_stored_for_an_active_job_and_an_error_fails_the_verdict() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    case.report_eval_message(W1, &k, EvalMessageLevel::Warning, "nix-eval", "deprecated")
        .await;
    let stored: Vec<_> = case
        .messages(e1)
        .await
        .into_iter()
        .map(|m| (m.level, m.source, m.message))
        .collect();
    assert_eq!(
        stored,
        vec![(
            MessageLevel::Warning,
            Some("nix-eval".to_owned()),
            "deprecated".to_owned()
        )],
        "after 1: evaluation_message(E1)"
    );
    case.check("step 1").await;

    let unknown = eval_key(EvaluationId::now_v7());
    case.report_eval_message(W1, &unknown, EvalMessageLevel::Error, "nix-eval", "nothing")
        .await;
    assert_eq!(
        case.messages(e1).await.len(),
        1,
        "after 2: evaluation_message(E1) rows unchanged"
    );
    case.check("step 2").await;

    case.report_eval_message(
        W1,
        &k,
        EvalMessageLevel::Error,
        "nix-eval:pkg.b",
        "attribute failed",
    )
    .await;
    assert_eq!(
        case.messages(e1).await.len(),
        2,
        "after 3: evaluation_message(E1) rows"
    );
    case.check("step 3").await;

    case.complete(W1, &k, a1, Vec::new(), 10).await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 4: evaluation(E1).status"
    );
    case.check("step 4").await;

    case.finish().await;
}

#[tokio::test]
async fn r21_eval_stats_fold_into_the_metric_when_the_evaluation_settles_and_only_the_closing_completion_adds_span_totals()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k = eval_key(e1);
    let a1 = case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let stats = EvalStatsReport {
        peak_rss_mb: 512,
        total_thunks: 1000,
        total_eval_ms: 900,
        ..Default::default()
    };
    case.report_eval_stats(W1, &k, stats).await;
    assert_eq!(
        case.evaluation_metric(e1).await,
        None,
        "after 1: evaluation_metric(E1) waits for the evaluation to settle"
    );
    case.check("step 1").await;

    let spans = vec![
        span(JobPhase::Fetch, 0, 100),
        span(JobPhase::EvalFlake, 100, 200),
        span(JobPhase::EvalDerivations, 200, 900),
    ];
    case.complete(W1, &k, a1, spans, 900).await;
    let metric = case
        .evaluation_metric(e1)
        .await
        .expect("after 2: evaluation_metric(E1)");
    assert_eq!(
        (
            metric.peak_rss_mb,
            metric.fetch_ms,
            metric.eval_flake_ms,
            metric.eval_drv_ms,
            metric.total_eval_ms,
            metric.worker_id.as_str(),
        ),
        (512, 100, 100, 700, 900, W1),
        "after 2: evaluation_metric(E1)"
    );
    let phases: Vec<_> = case
        .phases(a1)
        .await
        .into_iter()
        .map(|p| (p.seq, p.phase, p.start_ms, p.end_ms))
        .collect();
    assert_eq!(
        phases,
        vec![
            (0, JobPhase::Fetch.as_i16(), 0, 100),
            (1, JobPhase::EvalFlake.as_i16(), 100, 200),
            (2, JobPhase::EvalDerivations.as_i16(), 200, 900),
        ],
        "after 2: dispatched_job_phase(a1)"
    );
    case.check("step 2").await;

    case.complete(W1, &k, a1, vec![span(JobPhase::Fetch, 0, 999)], 999)
        .await;
    assert_eq!(
        case.evaluation_metric(e1).await,
        Some(metric),
        "after 3: evaluation_metric(E1) unchanged"
    );
    assert_eq!(
        case.assignments(&k)
            .await
            .first()
            .and_then(|row| row.worker_elapsed_ms),
        Some(900),
        "after 3: dj(eval:E1)[0].worker_elapsed_ms"
    );
    case.check("step 3").await;

    case.finish().await;
}

#[tokio::test]
async fn r22_input_update_reports_fill_the_row_and_an_expansion_creates_evaluations() {
    let e1 = EvaluationId::now_v7();
    let seed = shape()
        .evalq(e1)
        .input_update("c0", "nix flake update", &["nixpkgs*"], true)
        .evaluation_kind(EvaluationKind::InputUpdate);
    let case = SchedulerCase::start(seed).await;
    let k = eval_key(e1);
    case.evalrun(W1, e1).await;
    case.advance_evaluation(W1, &k, EvaluationStatus::EvaluatingDerivation)
        .await;
    case.check("step 0").await;

    let bumped = vec![BumpedInputWire {
        name: "nixpkgs".into(),
        old_rev: Some("a".into()),
        new_rev: "b".into(),
    }];
    case.report_input_update_result(W1, &k, "{...}", bumped)
        .await;
    let update = case
        .input_update(e1)
        .await
        .expect("after 1: evaluation_input_update(E1)");
    assert_eq!(
        (update.candidate_lock.as_deref(), update.bumped_inputs),
        (
            Some("{...}"),
            Some(serde_json::json!([{ "name": "nixpkgs", "old_rev": "a", "new_rev": "b" }]))
        ),
        "after 1: evaluation_input_update(E1)"
    );
    case.check("step 1").await;

    case.report_input_update_expansion(W1, &k, &["nixpkgs", "nixpkgs-unstable"])
        .await;
    let task = case.evaluation(e1).await.task;
    let mut created = Vec::new();
    for evaluation in case.evaluations().await {
        if evaluation.id == e1 || evaluation.task != task {
            continue;
        }
        let update = case
            .input_update(evaluation.id)
            .await
            .expect("after 2: an evaluation_input_update row per new evaluation");
        created.push((
            evaluation.kind,
            evaluation.status,
            evaluation.concurrent,
            update.generator,
            update.target_inputs,
        ));
    }
    created.sort_by_key(|row| row.4.to_string());
    let expected = ["nixpkgs", "nixpkgs-unstable"].map(|input| {
        (
            EvaluationKind::InputUpdate,
            EvaluationStatus::Queued,
            true,
            "flake_lock".to_owned(),
            serde_json::json!([input]),
        )
    });
    assert_eq!(
        created,
        expected.to_vec(),
        "after 2: new evaluations of the task of E1"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r23_the_completion_watchdog_drives_an_evaluation_whose_eval_row_closed_long_ago() {
    let [e1, e2, e3, e4] = [(); 4].map(|_| EvaluationId::now_v7());
    let seed = shape()
        .evalq(e1)
        .evaluation_status(EvaluationStatus::EvaluatingDerivation)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Completed, ago(901))
        .startable(A, A_OUT)
        .evalq(e2)
        .evaluation_status(EvaluationStatus::Fetching)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Completed, ago(901))
        .evalq(e3)
        .evaluation_status(EvaluationStatus::EvaluatingFlake)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901))
        .evalq(e4)
        .evaluation_status(EvaluationStatus::EvaluatingDerivation)
        .updated_at(ago(901))
        .eval_assignment()
        .closed(DispatchedJobOutcome::Failed, ago(901));
    let case = SchedulerCase::start(seed).await;
    for e in [e1, e2, e3, e4] {
        case.backdate_evaluation(e, ago(901)).await;
    }
    let expected = [
        EvaluationStatus::Building,
        EvaluationStatus::Fetching,
        EvaluationStatus::EvaluatingFlake,
        EvaluationStatus::Failed,
    ];

    for step in 1..=2 {
        case.eval_completion_watchdog().await;
        let mut statuses = Vec::new();
        for e in [e1, e2, e3, e4] {
            statuses.push(case.evaluation(e).await.status);
        }
        assert_eq!(
            statuses,
            expected.to_vec(),
            "after {step}: evaluation(E1..E4).status"
        );
        case.check(&format!("step {step}")).await;
    }

    case.finish().await;
}

#[tokio::test]
async fn r24_the_stranded_build_sweep_requeues_an_abandoned_building_build_and_skips_a_tracked_one()
{
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let seed = shape()
        .one(e1, A, A_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901))
        .one(e2, B, B_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .closed(DispatchedJobOutcome::Abandoned, ago(901));
    let case = SchedulerCase::start(seed).await;
    case.backdate_shared_build(A, ago(901)).await;
    case.backdate_shared_build(B, ago(901)).await;
    let (a, b) = (case.key(A).await, case.key(B).await);
    case.connect(W1, w_all()).await;
    case.respawn_core().await;
    let row = case
        .assignments(&b)
        .await
        .first()
        .map(|row| row.id)
        .expect("the seeded row of build:B");
    case.reattach(W1, &[(b.as_str(), row)]).await;
    case.check("step 0").await;

    case.stranded_build_sweep().await;
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Queued,
        "after 1: sb(A).status"
    );
    assert_eq!(
        case.attempt_outcomes_with_reasons(A).await,
        vec![(
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        )],
        "after 1: att(A)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Building,
        "after 1: sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes(B).await,
        vec![AttemptOutcome::Running],
        "after 1: att(B)"
    );
    case.check("step 1").await;

    case.resync_startable_set().await;
    assert!(case.pending(&a).await, "after 2: pending(build:A)");
    assert!(!case.pending(&b).await, "after 2: pending(build:B)");
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r25_the_abandoned_dispatch_sweep_closes_only_old_untracked_rows() {
    let seed = shape()
        .one(EvaluationId::now_v7(), A, A_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .by_worker("gone")
        .dispatched_at(ago(1801))
        .one(EvaluationId::now_v7(), B, B_OUT)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
        .by_worker("gone2")
        .dispatched_at(ago(1700))
        .one(EvaluationId::now_v7(), C, C_OUT);
    let case = SchedulerCase::start(seed).await;
    case.connect(W1, w_all()).await;
    let (a, b, c) = (case.key(A).await, case.key(B).await, case.key(C).await);

    let a3 = claim(&case, W1).await;
    assert_eq!(
        case.open_assignment(&c).await.map(|row| row.id),
        Some(a3),
        "after 1: W1 runs build:C"
    );
    case.report_building(W1, &c).await;
    case.check("step 1").await;

    case.abandoned_dispatch_sweep().await;
    let closed = case.assignments(&a).await;
    assert!(
        matches!(
            closed.as_slice(),
            [row] if row.outcome == Some(DispatchedJobOutcome::Abandoned)
                && row.finished_at.is_some_and(is_recent)
        ),
        "after 2: dj(build:A) == [{{Abandoned, finished_at ~now}}], got {closed:?}"
    );
    let open: Vec<_> = case
        .assignments(&b)
        .await
        .into_iter()
        .map(|row| row.finished_at)
        .collect();
    assert_eq!(open, vec![None], "after 2: dj(build:B) == [{{open}}]");
    assert_eq!(
        case.assignment_outcomes(&c).await,
        vec![(a3, None)],
        "after 2: dj(build:C)"
    );
    assert_eq!(
        (case.sb(A).await.status, case.sb(B).await.status),
        (BuildStatus::Building, BuildStatus::Building),
        "after 2: sb(A).status, sb(B).status"
    );
    assert_eq!(
        case.attempt_outcomes(A).await,
        vec![AttemptOutcome::Running],
        "after 2: att(A)"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn r26_phase_spans_are_stored_with_the_parent_on_completion_and_failure() {
    let case = SchedulerCase::start(shape().two(EvaluationId::now_v7())).await;
    case.connect(W1, w_all().slots(2)).await;
    let (a, b) = (case.key(A).await, case.key(B).await);

    case.admit_startable_moves().await;
    let mut taken = Vec::new();
    for _ in 0..2 {
        let assignment = case
            .request_job(W1, JobKind::Build)
            .await
            .expect("after 1: W1 gets a build");
        taken.push((assignment.job_id().to_owned(), assignment.assignment_id()));
    }
    let id_of = |key: &str| {
        taken
            .iter()
            .find(|(job, _)| job == key)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("after 1: W1 runs {key}, got {taken:?}"))
    };
    let (a1, a2) = (id_of(&a), id_of(&b));
    case.report_building(W1, &a).await;
    case.report_building(W1, &b).await;
    case.check("step 1").await;

    case.commit_nar(output_nar(A, A_OUT)).await;
    let spans = vec![
        JobPhaseSpan {
            paths: 3,
            bytes: 1000,
            ..span(JobPhase::Prefetch, 0, 50)
        },
        span(JobPhase::Build, 50, 500),
        JobPhaseSpan {
            parent: Some(1),
            ..span(JobPhase::Compress, 400, 480)
        },
        span(JobPhase::NarPush, 480, 520),
    ];
    case.complete(W1, &a, a1, spans, 520).await;
    let stored: Vec<_> = case
        .phases(a1)
        .await
        .into_iter()
        .map(|p| (p.seq, p.parent_seq, p.phase, p.paths, p.bytes))
        .collect();
    assert_eq!(
        stored,
        vec![
            (0, None, 8, 3, 1000),
            (1, None, 10, 0, 0),
            (2, Some(1), 11, 0, 0),
            (3, None, 12, 0, 0),
        ],
        "after 2: dispatched_job_phase(a1)"
    );
    case.check("step 2").await;

    let failed = failure(BuildFailureKind::Permanent)
        .spans(vec![
            span(JobPhase::Prefetch, 0, 10),
            span(JobPhase::Build, 10, 30),
        ])
        .elapsed_ms(30);
    case.fail(W1, &b, a2, failed).await;
    assert_eq!(
        case.phases(a2).await.len(),
        2,
        "after 3: dispatched_job_phase(a2) rows"
    );
    assert_eq!(
        elapsed(&case, &b).await.first().cloned(),
        Some((a2, Some(DispatchedJobOutcome::Failed), Some(30))),
        "after 3: dj(build:B)[0]"
    );
    case.check("step 3").await;

    case.finish().await;
}

async fn claim(case: &SchedulerCase, winner: &str) -> DispatchedJobId {
    case.admit_startable_moves().await;
    case.request_job(winner, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("after 1: {winner} gets a build"))
        .assignment_id()
}

async fn take(case: &SchedulerCase, worker: &str, key: &str) -> DispatchedJobId {
    let assignment = case
        .request_job(worker, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("{worker} gets {key} again"));
    assert_eq!(assignment.job_id(), key, "{worker} gets {key} again");
    assignment.assignment_id()
}

async fn substitute(case: &SchedulerCase, worker: &str, key: &str) -> DispatchedJobId {
    let assignment = case
        .request_job(worker, JobKind::Build)
        .await
        .unwrap_or_else(|| panic!("{worker} gets {key}"));
    let Job::Build(job) = &assignment.job else {
        panic!("a build job, got {:?}", assignment.job);
    };
    assert_eq!(
        job.build.kind,
        BuildSpecKind::Substitute,
        "result.job.kind of {key}"
    );
    assignment.assignment_id()
}

async fn retry_after(case: &SchedulerCase, secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
    case.requeue_transient_retries().await;
    case.admit_startable_moves().await;
}

fn transient(error: &str) -> super::Failure {
    failure(BuildFailureKind::Transient)
        .error(error)
        .elapsed_ms(4)
}

fn corrupt() -> super::Failure {
    failure(BuildFailureKind::CorruptEvalCache)
        .error("schema")
        .missing(&["F".into()])
        .elapsed_ms(3)
}

async fn has_error(case: &SchedulerCase, evaluation: EvaluationId) -> bool {
    case.messages(evaluation)
        .await
        .iter()
        .any(|m| m.level == MessageLevel::Error)
}

async fn elapsed(
    case: &SchedulerCase,
    key: &str,
) -> Vec<(DispatchedJobId, Option<DispatchedJobOutcome>, Option<i64>)> {
    case.assignments(key)
        .await
        .into_iter()
        .map(|row| (row.id, row.outcome, row.worker_elapsed_ms))
        .collect()
}

async fn started(case: &SchedulerCase, path: &str) -> Vec<(AttemptOutcome, bool)> {
    case.attempts(path)
        .await
        .into_iter()
        .map(|t| (t.outcome, t.build_started_at.is_some()))
        .collect()
}

const DEP_DRV_FILE_BYTES_WITHOUT_INPUTS: &[u8] =
    include_bytes!("../../../gradient-ssh/testdata/input-addressed-dep.drv");
const REAL_DEP_DERIVATION_FILE_PATH: &str =
    include_str!("../../../gradient-ssh/testdata/input-addressed-dep.drv.path");
const TOP_DERIVATION_FILE_BYTES: &[u8] =
    include_bytes!("../../../gradient-ssh/testdata/input-addressed-top.drv");
const TOP_PATH: &str = include_str!("../../../gradient-ssh/testdata/input-addressed-top.drv.path");
const EXPECTED_STORED_HASH_OF_DEP_DRV_TEXT: &str =
    "drv:sha256:2f147701429b5cb2d91db5743a8edbab066eedeb5f5a7afe7c5be32def86670c";
const EXPECTED_STORED_SHA256_HASH_OF_TOP_DRV_TEXT: &str =
    "drv:sha256:93fde50a3163cc333d4ed43c059ac5b5a580b1dd33a6d88a6ab2d4988b54ddba";

fn store_path_hash_part(path: &str) -> &str {
    path.trim()
        .trim_start_matches("/nix/store/")
        .split_once('-')
        .expect("a store base path")
        .0
}

async fn store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    text: &[u8],
    path: &str,
) -> DiscoveredDerivation {
    let project = case.db.project_of(evaluation).await;
    let file =
        DerivationFileFromReportedFacts::from_nix_written_derivation_file_bytes_and_store_path(
            text, path,
        );
    let mut reported = case
        .upload_derivation_file_as_project_worker_content_variant(project, file)
        .await;
    reported.attr = attr_of(&reported.drv_path);
    reported
}

async fn store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    key: &str,
) -> anyhow::Result<()> {
    let dep = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        case,
        evaluation,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    let top = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        case,
        evaluation,
        TOP_DERIVATION_FILE_BYTES,
        TOP_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(key, vec![dep, top], Vec::new(), Vec::new())
        .await
}

async fn read_stored_hash_modulo_for_derivation_by_store_path_hash(
    case: &SchedulerCase,
    hash: &str,
) -> Option<String> {
    derivation::Entity::find()
        .filter(derivation::Column::Hash.eq(hash))
        .one(&case.db.connection())
        .await
        .unwrap()
        .expect("Derivation row")
        .serialized_derivation_hash_for_dependent_output_paths
}

async fn set_stored_hash_modulo_of_derivation_with_store_path_hash(
    case: &SchedulerCase,
    hash: &str,
    value: Option<&str>,
) {
    derivation::Entity::update_many()
        .col_expr(
            derivation::Column::SerializedDerivationHashForDependentOutputPaths,
            sea_orm::sea_query::Expr::value(value.map(str::to_owned)),
        )
        .filter(derivation::Column::Hash.eq(hash))
        .exec(&case.db.connection())
        .await
        .expect("Expect a change in the stored hash modulo.");
}

#[tokio::test]
async fn test_reporting_real_derivation_files_fills_stored_hash_modulo_for_both_derivation_rows() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;

    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    assert_eq!(
        (
            read_stored_hash_modulo_for_derivation_by_store_path_hash(
                &case,
                store_path_hash_part(REAL_DEP_DERIVATION_FILE_PATH),
            )
            .await,
            read_stored_hash_modulo_for_derivation_by_store_path_hash(
                &case,
                store_path_hash_part(TOP_PATH),
            )
            .await,
        ),
        (
            Some(EXPECTED_STORED_HASH_OF_DEP_DRV_TEXT.to_owned()),
            Some(EXPECTED_STORED_SHA256_HASH_OF_TOP_DRV_TEXT.to_owned()),
        ),
        "After 1: modulo of the saved hash of dep and top"
    );
    case.check("step 1").await;

    case.finish().await;
}

#[tokio::test]
async fn null_hash_modulo_derivation_gets_value_when_reported_and_verified_again() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    let dep = store_path_hash_part(REAL_DEP_DERIVATION_FILE_PATH);
    set_stored_hash_modulo_of_derivation_with_store_path_hash(&case, dep, None).await;
    case.check("step 1").await;

    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    assert_eq!(
        read_stored_hash_modulo_for_derivation_by_store_path_hash(&case, dep).await,
        Some(EXPECTED_STORED_HASH_OF_DEP_DRV_TEXT.to_owned()),
        "Stored hash modulo of the NULL row filled again after 2."
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn test_stored_hash_modulo_mismatch_fails_evaluation_and_keeps_stored_value_unchanged() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    let dep = store_path_hash_part(REAL_DEP_DERIVATION_FILE_PATH);
    let differing = format!("drv:sha256:{}", "0".repeat(64));
    set_stored_hash_modulo_of_derivation_with_store_path_hash(&case, dep, Some(&differing)).await;
    case.check("step 1").await;

    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect_err("The eval result can fail when the modulo of the stored hash differs.");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert_eq!(
        read_stored_hash_modulo_for_derivation_by_store_path_hash(&case, dep).await,
        Some(differing),
        "After 2: different stored hash modulo unchanged"
    );
    case.check("step 2").await;

    case.finish().await;
}

async fn delete_nar_object_for_derivation_file(case: &SchedulerCase, text: &[u8], path: &str) {
    let file =
        DerivationFileFromReportedFacts::from_nix_written_derivation_file_bytes_and_store_path(
            text, path,
        );
    let facts = gradient_storage::compress_nar_bytes_and_verify_stream(
        &file.uncompressed_nar_archive_bytes(),
    );
    case.ctx
        .storage
        .nar_storage
        .delete(
            &facts.object_storage_key_for_store_path_hash_and_compressed_file_sha256(
                store_path_hash_part(path),
            ),
        )
        .await
        .expect("The NAR object stored for the derivation file is no longer present.");
}

#[tokio::test]
async fn dependent_report_in_second_batch_uses_input_hash_from_database_after_derivation_file_deleted()
 {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let dep = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![dep], Vec::new(), Vec::new())
        .await
        .expect("Evaluation results undergo processing.");
    delete_nar_object_for_derivation_file(
        &case,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    case.check("step 1").await;

    let top = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        TOP_DERIVATION_FILE_BYTES,
        TOP_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![top], Vec::new(), Vec::new())
        .await
        .expect("Reports without dep go through.");
    assert_ne!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert_eq!(
        read_stored_hash_modulo_for_derivation_by_store_path_hash(
            &case,
            store_path_hash_part(TOP_PATH),
        )
        .await,
        Some(EXPECTED_STORED_SHA256_HASH_OF_TOP_DRV_TEXT.to_owned()),
        "After 2: modulo of the saved hash of top"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn dependent_derivation_reported_in_batch_1_and_input_in_batch_2_fails_naming_missing_input()
{
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let top = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        TOP_DERIVATION_FILE_BYTES,
        TOP_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![top], Vec::new(), Vec::new())
        .await
        .expect("Evaluation results undergo processing.");
    let dep = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![dep], Vec::new(), Vec::new())
        .await
        .expect("Evaluation results undergo processing.");

    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert!(
        case.messages(e1)
            .await
            .iter()
            .any(|m| m.level == MessageLevel::Error
                && m.message.contains(REAL_DEP_DERIVATION_FILE_PATH.trim())
                && m.message.contains("is an input derivation of")
                && m.message
                    .contains("Upgrade the evaluation worker together with the server.")
                && m.message.contains("Run the evaluation again.")),
        "after 2: messages: {:?}",
        case.messages(e1).await
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn batch_store_database_error_marks_evaluation_failed_and_later_batch_is_not_stored() {
    use sea_orm::ConnectionTrait as _;

    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let dep = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    let connection = case.db.connection();
    connection
        .execute_unprepared(
            "CREATE FUNCTION fail_on_derivation_insert() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'fail_on_derivation_insert'; END $$; \
             CREATE TRIGGER fail_on_derivation_insert BEFORE INSERT ON derivation \
             FOR EACH ROW EXECUTE FUNCTION fail_on_derivation_insert();",
        )
        .await
        .expect("Tests install a trigger to reject inserts into the derivation table.");

    let first_derivation_batch_store_error = case
        .scheduler
        .handle_eval_result(&k1, vec![dep.clone()], Vec::new(), Vec::new())
        .await
        .expect_err("The trigger rejected inserts into the derivation table, so the first batch didn't save.");
    assert!(
        format!("{first_derivation_batch_store_error:#}").contains("fail_on_derivation_insert"),
        "after 1: error: {first_derivation_batch_store_error:#}"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation.status"
    );
    let evaluation_message_text_after_failed_save = case
        .messages(e1)
        .await
        .into_iter()
        .find(|m| m.level == MessageLevel::Error)
        .map(|m| m.message)
        .expect("after 1: an error message on the evaluation");
    assert_eq!(
        evaluation_message_text_after_failed_save,
        "The server failed to save the batch of derivations. Evaluation can't continue: the database or storage rejected the write. Run the evaluation again. The server log records the details.",
        "after 1: message"
    );
    assert!(
        evaluation_message_text_after_failed_save
            .contains("the database or storage rejected the write")
            && !evaluation_message_text_after_failed_save.contains("fail_on_derivation_insert")
            && !evaluation_message_text_after_failed_save.contains("upsert"),
        "after 1: message: {evaluation_message_text_after_failed_save}"
    );

    connection
        .execute_unprepared("DROP TRIGGER fail_on_derivation_insert ON derivation;")
        .await
        .expect("The test dropped the trigger that rejected inserts into the derivation table.");
    case.scheduler
        .handle_eval_result(&k1, vec![dep], Vec::new(), Vec::new())
        .await
        .expect("Later batches after a failed evaluation have no effect and produce no error.");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert_eq!(
        (
            derivation::Entity::find().count(&connection).await.unwrap(),
            derivation_build::Entity::find()
                .count(&connection)
                .await
                .unwrap(),
        ),
        (0, 0),
        "after 2: derivation and build rows"
    );

    case.finish().await;
}

async fn test_graph_writer_crash_between_report_batches_fails_evaluation_and_queues_no_build(
    database_rejects_evaluation_update_while_graph_writer_down: bool,
) {
    use sea_orm::ConnectionTrait as _;

    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let dep = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![dep.clone()], Vec::new(), Vec::new())
        .await
        .expect("Graph writers save the first batch while they run.");

    let connection = case.db.connection();
    if database_rejects_evaluation_update_while_graph_writer_down {
        connection
            .execute_unprepared(
                "CREATE FUNCTION fail_on_evaluation_update() RETURNS trigger LANGUAGE plpgsql AS \
                 $$ BEGIN RAISE EXCEPTION 'fail_on_evaluation_update'; END $$; \
                 CREATE TRIGGER fail_on_evaluation_update BEFORE UPDATE ON evaluation \
                 FOR EACH ROW EXECUTE FUNCTION fail_on_evaluation_update();",
            )
            .await
            .expect("The test can install a trigger to reject updates to the evaluation table.");
    }
    case.kill_graph_writer_actor().await;
    let unsaved_report_batch_eval_result_error = case
        .scheduler
        .handle_eval_result(&k1, vec![dep], Vec::new(), Vec::new())
        .await
        .expect_err("The graph writer is down. The second batch cannot reach disk.");
    case.start_graph_writer_after_kill().await;
    if database_rejects_evaluation_update_while_graph_writer_down {
        assert_ne!(
            case.evaluation(e1).await.status,
            EvaluationStatus::Failed,
            "{unsaved_report_batch_eval_result_error:#}"
        );
        connection
            .execute_unprepared(
                "DROP TRIGGER fail_on_evaluation_update ON evaluation; \
                 DROP FUNCTION fail_on_evaluation_update();",
            )
            .await
            .expect("The test removed the trigger that blocked updates to the evaluation table.");
    }

    case.scheduler
        .handle_job_completed(W1, &k1)
        .await
        .expect("The scheduler can process completion of evaluation jobs.");
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "Evaluation Status After Completion"
    );
    let evaluation_error_message = case
        .messages(e1)
        .await
        .into_iter()
        .find(|m| m.level == MessageLevel::Error)
        .map(|m| m.message)
        .expect("Error Message for Evaluation After Completion");
    assert_eq!(
        evaluation_error_message,
        "The server failed to save the batch of derivations. Evaluation can't continue: the database or storage rejected the write. Run the evaluation again. The server log records the details.",
        "Error Message After Completion"
    );
    assert_eq!(
        derivation_build::Entity::find()
            .filter(derivation_build::Column::Status.eq(BuildStatus::Queued))
            .count(&connection)
            .await
            .unwrap(),
        0,
        "Builds queued after completion"
    );

    case.finish().await;
}

#[tokio::test]
async fn graph_writer_killed_between_report_batches_fails_evaluation_and_queues_no_build() {
    test_graph_writer_crash_between_report_batches_fails_evaluation_and_queues_no_build(false)
        .await;
}

#[tokio::test]
async fn lost_batch_and_failed_evaluation_update_still_fails_evaluation_on_job_completion() {
    test_graph_writer_crash_between_report_batches_fails_evaluation_and_queues_no_build(true).await;
}

#[tokio::test]
async fn second_report_that_omits_dependency_does_not_read_its_deleted_derivation_file() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    delete_nar_object_for_derivation_file(
        &case,
        DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
        REAL_DEP_DERIVATION_FILE_PATH,
    )
    .await;
    case.check("step 1").await;

    let top = store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
        &case,
        e1,
        TOP_DERIVATION_FILE_BYTES,
        TOP_PATH,
    )
    .await;
    case.scheduler
        .handle_eval_result(&k1, vec![top], Vec::new(), Vec::new())
        .await
        .expect("Reports without dep go through.");
    assert_ne!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn test_project_p_reports_top_derivation_after_project_q_input_derivation_nar_deleted_without_evaluation_failure_and_stores_derivation_row()
 {
    let (project_q_evaluation_id, project_p_evaluation_id) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = SchedulerCase::start(
        shape()
            .evalq(project_q_evaluation_id)
            .evalq(project_p_evaluation_id),
    )
    .await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, project_q_evaluation_id).await;
    let first_input_recording_project_id = case.db.project_of(project_q_evaluation_id).await;
    let input_derivation_file_from_reported_facts =
        DerivationFileFromReportedFacts::from_reported_derivation_facts(
            &build_discovered_derivation_with_one_output(B, B_OUT),
        );
    let uploaded_input_derivation_reported_by_project_q_worker = case
        .upload_derivation_file_as_project_worker_content_variant(
            first_input_recording_project_id,
            input_derivation_file_from_reported_facts.clone(),
        )
        .await;
    let uploaded_input_derivation_store_basename =
        uploaded_input_derivation_reported_by_project_q_worker
            .drv_path
            .trim_start_matches("/nix/store/");
    let first_top_derivation_reported_by_project_q = case
        .upload_reported_derivation_as_evaluation_project_worker(
            first_input_recording_project_id,
            &with_attr(
                with_output(drv(A, &[uploaded_input_derivation_store_basename]), A_OUT),
                &attr_of(A),
            ),
        )
        .await;
    case.scheduler
        .handle_eval_result(
            &k1,
            vec![
                uploaded_input_derivation_reported_by_project_q_worker.clone(),
                first_top_derivation_reported_by_project_q,
            ],
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("Evaluation results undergo processing.");
    let project_p_id = case.db.project_of(project_p_evaluation_id).await;
    assert!(
        load_verified_trusted_derivation_record(
            &case.scheduler.db.worker_db,
            &case.ctx.storage.nar_storage,
            Arc::clone(&case.scheduler.db.store_path_mapping),
            project_p_id,
            &uploaded_input_derivation_reported_by_project_q_worker.drv_path,
        )
        .await
        .expect("A trusted lookup can run on files for input derivations.")
        .is_none(),
        "Before 2: Project P does not trust any stored variant of an input derivation file."
    );
    delete_nar_object_for_derivation_file(
        &case,
        &input_derivation_file_from_reported_facts.text,
        &uploaded_input_derivation_reported_by_project_q_worker.drv_path,
    )
    .await;
    case.check("step 1").await;

    let evaluation_job_key = eval_key(project_p_evaluation_id);
    case.evalrun(W2, project_p_evaluation_id).await;
    case.advance_evaluation(
        W2,
        &evaluation_job_key,
        EvaluationStatus::EvaluatingDerivation,
    )
    .await;
    let project_p_only_second_top_derivation_depending_on_shared_input_derivation = case
        .upload_reported_derivation_as_evaluation_project_worker(
            project_p_id,
            &with_attr(
                with_output(drv(C, &[uploaded_input_derivation_store_basename]), C_OUT),
                &attr_of(C),
            ),
        )
        .await;
    case.scheduler
        .handle_eval_result(
            &evaluation_job_key,
            vec![project_p_only_second_top_derivation_depending_on_shared_input_derivation.clone()],
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("Evaluation results undergo processing.");
    assert_ne!(
        case.evaluation(project_p_evaluation_id).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert!(
        read_stored_hash_modulo_for_derivation_by_store_path_hash(
            &case,
            store_path_hash_part(
                &project_p_only_second_top_derivation_depending_on_shared_input_derivation.drv_path,
            ),
        )
        .await
        .is_some(),
        "After 2: stored hash modulo of the second top derivation"
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn known_derivations_query_returns_empty_when_client_store_path_differs_from_stored_path() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    store_dep_and_top_derivations_as_worker_content_variants_and_report_one_eval_result(
        &case, e1, &k1,
    )
    .await
    .expect("Evaluation results undergo processing.");
    let project_q_recorded_input_row_derivation_path = REAL_DEP_DERIVATION_FILE_PATH.trim();
    let stored_input_derivation_path_hash =
        store_path_hash_part(project_q_recorded_input_row_derivation_path).to_owned();
    assert!(
        read_stored_hash_modulo_for_derivation_by_store_path_hash(
            &case,
            &stored_input_derivation_path_hash,
        )
        .await
        .is_some(),
        "After 1: A hash modulo is on the derivation row for the input that Project Q recorded."
    );
    assert_eq!(
        case.graph
            .known_derivations(vec![stored_input_derivation_path_hash.clone()])
            .await
            .expect("A known query can read the derivation rows."),
        vec![project_q_recorded_input_row_derivation_path.to_owned()],
        "The known query can respond to a direct request for the stored path of an input row of project Q."
    );
    case.check("step 1").await;

    let project_store_path_mapper_rotating_hash_part =
        StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
    let client_derivation_path = project_store_path_mapper_rotating_hash_part
        .outbound_text(project_q_recorded_input_row_derivation_path)
        .expect("The rotated mapping converts stored input paths to client paths.");
    let hash_rotated_client_derivation_path = project_store_path_mapper_rotating_hash_part
        .inbound_text_if_exact_store_path(&client_derivation_path)
        .expect("The rotated mapping can convert the client path into the saved path.");
    assert_eq!(
        hash_rotated_client_derivation_path, project_q_recorded_input_row_derivation_path,
        "This mapping can convert a client path into the path where input rows for project Q are stored."
    );
    // Same rule as `filter_stored_paths_equal_to_client_paths` in gradient-proto. This crate can't call that function.
    let stored_paths_matching_client_paths: Vec<String> =
        [(hash_rotated_client_derivation_path, client_derivation_path)]
            .into_iter()
            .filter(|(stored, client)| stored == client)
            .map(|(stored, _)| store_path_hash_part(&stored).to_owned())
            .collect();
    assert!(
        case.graph
            .known_derivations(stored_paths_matching_client_paths)
            .await
            .expect("A known query can read the derivation rows.")
            .is_empty(),
        "After 2: no known queries respond to a client path that the mapping changes"
    );
    case.check("step 2").await;

    case.finish().await;
}

async fn report_input_derivation_with_overwritten_hash_modulo_then_report_top_derivation(
    case: &SchedulerCase,
    evaluation: EvaluationId,
    key: &str,
    input_derivation_hash_modulo_text: &str,
) {
    let uploaded_input_derivation_from_first_evaluation_result =
        store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
            case,
            evaluation,
            DEP_DRV_FILE_BYTES_WITHOUT_INPUTS,
            REAL_DEP_DERIVATION_FILE_PATH,
        )
        .await;
    case.scheduler
        .handle_eval_result(
            key,
            vec![uploaded_input_derivation_from_first_evaluation_result],
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("Evaluation results undergo processing.");
    set_stored_hash_modulo_of_derivation_with_store_path_hash(
        case,
        store_path_hash_part(REAL_DEP_DERIVATION_FILE_PATH),
        Some(input_derivation_hash_modulo_text),
    )
    .await;
    let dependent_top_derivation_in_second_result =
        store_derivation_as_evaluation_worker_content_variant_and_return_reported_derivation(
            case,
            evaluation,
            TOP_DERIVATION_FILE_BYTES,
            TOP_PATH,
        )
        .await;
    case.scheduler
        .handle_eval_result(
            key,
            vec![dependent_top_derivation_in_second_result],
            Vec::new(),
            Vec::new(),
        )
        .await
        .expect("Evaluation results undergo processing.");
}

#[tokio::test]
async fn report_omitting_input_fails_on_output_path_mismatch_when_input_has_wrong_hash_modulo() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;
    let wrong_stored_hash_modulo_text = format!("drv:sha256:{}", "0".repeat(64));

    report_input_derivation_with_overwritten_hash_modulo_then_report_top_derivation(
        &case,
        e1,
        &k1,
        &wrong_stored_hash_modulo_text,
    )
    .await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert!(
        case.messages(e1)
            .await
            .iter()
            .any(|m| m.level == MessageLevel::Error
                && m.message.contains(store_path_hash_part(TOP_PATH))
                && m.message.contains("but its inputs give")),
        "after 2: the messages report a mismatch in the output path of top: {:?}",
        case.messages(e1).await
    );
    case.check("step 2").await;

    case.finish().await;
}

#[tokio::test]
async fn test_report_omitting_input_with_unparsable_hash_modulo_fails_evaluation_and_names_row() {
    let e1 = EvaluationId::now_v7();
    let case = SchedulerCase::start(shape().evalq(e1)).await;
    let k1 = connect_worker_one_assign_evaluation_job_move_evaluation_to_evaluating_derivations_and_return_evaluation_job_key(&case, e1).await;

    report_input_derivation_with_overwritten_hash_modulo_then_report_top_derivation(
        &case,
        e1,
        &k1,
        "drv:sha256:zz",
    )
    .await;
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 2: evaluation.status"
    );
    assert!(
        case.messages(e1)
            .await
            .iter()
            .any(|m| m.level == MessageLevel::Error
                && m.message.contains(REAL_DEP_DERIVATION_FILE_PATH.trim())
                && m.message.contains("won't parse")),
        "after 2: the message can name the input line whose hash modulo did not parse: {:?}",
        case.messages(e1).await
    );
    case.check("step 2").await;

    case.finish().await;
}
