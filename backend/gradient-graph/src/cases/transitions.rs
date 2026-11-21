/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{CacheId, EvaluationId};
use gradient_fixtures::shape::{Shape, build_assignment, shape};
use gradient_wire::types::{BuildFailureKind, BuildMetrics};
use sea_orm::{ActiveModelTrait, IntoActiveModel};

use super::{
    A, A_OUT, A2, A2_OUT, B, B_OUT, C, Case, D, I, I_OUT, P, P_OUT, Q, Q_OUT, R, R_OUT, chain,
    chain_done, diamond, drv, nar, store_path, with_attr, with_output, with_references,
};
use crate::messages::{FailedBuildDispatchReport, OrphanCause, Transition};
use crate::transition::find_worker_project_and_credentials_for_dispatched_job;

fn building(shape: Shape, path: &str) -> Shape {
    shape
        .derivation(path)
        .build_status(BuildStatus::Building)
        .attempt(AttemptOutcome::Running, None)
        .open_assignment()
}

#[tokio::test]
async fn t01_build_started_moves_queued_to_building_with_a_running_attempt() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    let assignment = case.assigned(e1, B, false).await;

    assert!(assignment.is_some(), "step 1: Assigned(E1, B) is accepted");
    case.zero_drift("step 1").await;
    let version = case.gv(e1).await;

    case.build_started(B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Building, true),
        "after 2: sb(B)"
    );
    assert_eq!(
        case.latest_attempt(B).await.outcome,
        AttemptOutcome::Running,
        "after 2: attempt(B, latest)"
    );
    assert!(case.gv(e1).await > version, "after 2: gv(E1) rose");
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 1],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t02_stored_output_does_not_make_build_fetchable_until_completion_report_queues_parent() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain(e1), B)).await;

    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Building, false, true),
        "after 1: sb(B)"
    );
    assert!(
        case.output_stored(B, "out").await,
        "after 1: cached_path(output(B,out).hash) stored"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 1],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    case.build_completed(B).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Completed, true, false),
        "after 2: sb(B)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Queued, 0),
        "after 2: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t19_a_completed_report_without_the_output_uploaded_to_the_store_becomes_a_transient_failure_scheduled_for_retry_that_names_the_store_path_and_leaves_the_referencing_evaluation_not_completed()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    let output = case.output_of(A, "out").await;
    let path = store_path(A_OUT, output.package.as_deref().unwrap_or_default());

    case.build_completed(A).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.attempt, a.wanted),
        (BuildStatus::FailedTransient, 1, true),
        "after 1: sb(A)"
    );
    let attempt = case.latest_attempt(A).await;
    assert_eq!(
        attempt.outcome,
        AttemptOutcome::Failed,
        "after 1: attempt(A, latest)"
    );
    let message = attempt.failure_message.unwrap_or_default();
    assert!(
        message.contains(&path),
        "after 1: attempt(A, latest).failure_message names {path}: {message}"
    );
    assert_ne!(
        case.evaluation_status(e1).await,
        EvaluationStatus::Completed,
        "after 1: evaluation(E1).status"
    );
    case.finish().await;
}

#[tokio::test]
async fn t03_a_transient_failure_stays_a_wanted_builder_and_requeues_only_after_the_backoff() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain(e1), B)).await;

    case.build_failed(B, BuildFailureKind::Transient, &[]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::FailedTransient, true),
        "after 1: sb(B)"
    );
    assert_eq!(
        case.latest_attempt(B).await.outcome,
        AttemptOutcome::Failed,
        "after 1: attempt(B, latest)"
    );
    assert!(case.sb(A).await.wanted, "after 1: sb(A).wanted");
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    let requeued = case.requeue().await;

    assert_eq!(requeued, 0, "after 2: result inside the backoff of B");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTransient,
        "after 2: sb(B)"
    );
    case.zero_drift("step 2").await;

    case.advance_clock(B).await;
    let requeued = case.requeue().await;

    assert_eq!(requeued, 1, "after 3: result past the backoff of B");
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 3: sb(B)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 3: counters(E1)"
    );
    case.zero_drift("step 3").await;
    case.finish().await;
}

#[tokio::test]
async fn t04_a_permanent_failure_fails_the_dependents_and_the_evaluation() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain(e1).evaluation_status(EvaluationStatus::Building),
        B,
    ))
    .await;
    let version = case.gv(e1).await;

    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::FailedPermanent, false),
        "after 1: sb(B)"
    );
    let attempt = case.latest_attempt(B).await;
    assert_eq!(
        (attempt.outcome, attempt.failure_message.is_some()),
        (AttemptOutcome::Failed, true),
        "after 1: attempt(B, latest): {attempt:?}"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.wanted),
        (BuildStatus::DependencyFailed, false),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 2, 0, 0],
        "after 1: counters(E1)"
    );
    assert!(case.gv(e1).await > version, "after 1: gv(E1) rose");
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t05_a_timeout_is_a_terminal_failure_and_spreads_like_a_permanent_failure() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain(e1).evaluation_status(EvaluationStatus::Building),
        B,
    ))
    .await;

    case.build_failed(B, BuildFailureKind::Timeout, &[]).await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::FailedTimeout,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(A)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
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
async fn t06_inputs_unavailable_with_the_object_in_storage_deletes_nothing_and_retries() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    let stored = case
        .find_cached_path(B_OUT)
        .await
        .expect("cached_path(B_OUT) is seeded");
    case.store_object(&stored.nar_object_storage_key).await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert!(
        case.find_cached_path(B_OUT).await.is_some(),
        "after 1: cached_path(B_OUT) present"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Completed, true),
        "after 1: sb(B)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps, a.wanted),
        (BuildStatus::FailedTransient, 0, true),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 1, 0, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t07_inputs_unavailable_with_the_object_gone_drops_the_path_and_resets_the_producer() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert!(
        case.find_cached_path(B_OUT).await.is_none(),
        "after 1: cached_path(B_OUT) gone"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable, b.wanted),
        (BuildStatus::Queued, false, true),
        "after 1: sb(B)"
    );
    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::FailedTransient, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;

    case.advance_clock(A).await;
    case.requeue().await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 2: sb(A)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t15_requeue_thaws_permanently_failed_producer_and_evaluation_stays_building_until_both_complete()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    case.override_sb(B, |build| build.status = BuildStatus::FailedPermanent)
        .await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.wanted),
        (BuildStatus::Queued, true),
        "after 1: requeue thaws B to Queued and wanted"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedTransient,
        "sb(A) is FailedTransient after 1."
    );
    assert!(
        case.counters(e1).await[1] > 0,
        "After 1: active builds still stay in E1's count."
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "E1 remains Building after 1"
    );
    case.zero_drift("step 1").await;

    case.build_started(B).await;
    case.build_output(B, false).await;
    case.commit_nar(with_references(nar(B_OUT, "b"), &[(B_OUT, "b")]))
        .await;
    case.build_completed(B).await;
    case.advance_clock(A).await;
    case.requeue().await;
    case.build_started(A).await;
    case.build_output(A, false).await;
    case.commit_nar(with_references(nar(A_OUT, "a"), &[(B_OUT, "b")]))
        .await;
    case.build_completed(A).await;

    assert_ne!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "E1 left Building after B and A complete."
    );
    case.finish().await;
}

#[tokio::test]
async fn t15b_inputs_unavailable_after_the_evaluation_is_aborted_thaws_the_producer_to_skipped_and_leaves_building()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    case.override_sb(B, |build| build.status = BuildStatus::Aborted)
        .await;

    case.abort_build(e1, A).await;
    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Skipped,
        "After 1: thaw of B against cancelled build A ends in Skipped"
    );
    assert_ne!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Building,
        "After 1: E1 left Building"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t17_late_permanent_worker_failure_after_evaluation_abort_keeps_build_aborted_and_sends_no_consumer_status_event()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .build_status(BuildStatus::FailedTransient),
        B,
    ))
    .await;
    let consumer_shared_build_a_id = case.sb(A).await.id;

    case.set_evaluation_status(e1, EvaluationStatus::Aborted)
        .await;
    case.transition(Transition::AbortEvaluationSharedBuilds {
        evaluation: e1,
        hard: true,
    })
    .await;
    let mut build_status_event_receiver = case.ctx.events.subscribe();
    case.build_failed(B, BuildFailureKind::Permanent, &[]).await;

    let mut statuses_announced_for_consumer_shared_build_a_after_late_failure_report = Vec::new();
    while let Ok(envelope) = build_status_event_receiver.try_recv() {
        if let gradient_types::events::Event::BuildStatusChanged(changed) = &envelope.event
            && changed.derivation_build == consumer_shared_build_a_id
        {
            statuses_announced_for_consumer_shared_build_a_after_late_failure_report
                .push(changed.status);
        }
    }
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Aborted,
        "after 2: B is still Aborted"
    );
    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedTransient,
        "After 2: waiting consumer A still in FailedTransient state"
    );
    assert!(
        statuses_announced_for_consumer_shared_build_a_after_late_failure_report.is_empty(),
        "No status event for A after 2 rounds"
    );
    case.finish().await;
}

#[tokio::test]
async fn t18_late_permanent_failure_report_from_old_dispatch_after_thaw_keeps_build_queued_and_second_evaluation_consumer_status()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(
        chain(e1)
            .evaluation_status(EvaluationStatus::Building)
            .evaluation(e2),
    )
    .await;
    let dispatch_row_id_for_evaluation_before_abort = case
        .assigned(e1, B, false)
        .await
        .expect("Tests allow Assigned(E1, B).");
    case.build_started(B).await;

    case.set_evaluation_status(e1, EvaluationStatus::Aborted)
        .await;
    case.transition(Transition::AbortEvaluationSharedBuilds {
        evaluation: e1,
        hard: true,
    })
    .await;

    let e2_walk_batch = case
        .tasked(
            e2,
            vec![with_attr(with_output(drv(A2, &[B]), A2_OUT), "pkg.a2")],
        )
        .await;
    case.walk(e2_walk_batch).await;
    case.upstream_probed(&[A2]).await;
    assert_eq!(
        case.status(B).await,
        BuildStatus::Queued,
        "After 3: B Queued again for E2"
    );
    let a2_build_status_before_late_failure_report = case.status(A2).await;

    case.transition(Transition::BuildFailed {
        shared_build: case.sb(B).await.id,
        error: String::new(),
        log_banner: String::new(),
        kind: BuildFailureKind::Permanent,
        missing_paths: Vec::new(),
        dispatch_that_reported_failure: Some(FailedBuildDispatchReport {
            dispatched_job: dispatch_row_id_for_evaluation_before_abort,
            substitute: false,
            build_context: serde_json::json!({}),
            metrics: None,
        }),
    })
    .await;

    assert_eq!(
        case.status(B).await,
        BuildStatus::Queued,
        "After 4: B can stay in Queued due to a late failure report from the old dispatch."
    );
    assert_eq!(
        case.status(A2).await,
        a2_build_status_before_late_failure_report,
        "Consumer A2 in E2 is still Created after step 4."
    );
    case.finish().await;
}

#[tokio::test]
async fn t08_the_fourth_inputs_unavailable_fails_the_build_permanently_without_a_self_heal() {
    let e1 = EvaluationId::now_v7();
    let unavailable = Some(AttemptFailureReason::InputsUnavailable);
    let case = Case::start(
        chain_done(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(A)"
    );
    assert!(
        case.find_cached_path(B_OUT).await.is_some(),
        "after 1: cached_path(B_OUT) present, no self-heal"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 0, 1, 0, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn last_allowed_attempt_inputs_unavailable_ends_build_with_final_failed_status_and_keeps_referenced_uploaded_output()
 {
    let (e1, e2, unread_cache) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        CacheId::now_v7(),
    );
    let unavailable = Some(AttemptFailureReason::InputsUnavailable);
    let case = Case::start(
        chain_done(e1)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(A)
            .build_status(BuildStatus::Building)
            .shared_build_option(|b| b.attempt = 2)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Failed, unavailable)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .evaluation(e2)
            .evaluation_status(EvaluationStatus::Completed)
            .into_cache(unread_cache)
            .cached_path(R_OUT, "r")
            .mark_uploaded_by_user(),
    )
    .await;
    assert_eq!(case.sb(A).await.attempt, 2, "seed: sb(A).attempt");
    let upload = case
        .find_cached_path(R_OUT)
        .await
        .expect("cached_path(R_OUT) is seeded");
    case.store_object(&upload.nar_object_storage_key).await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(R_OUT, "r")],
    )
    .await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(A)"
    );
    assert_eq!(
        case.latest_attempt(A).await.reason,
        unavailable,
        "after 1: latest attempt(A).reason"
    );
    assert!(
        case.find_cached_path(R_OUT).await.is_some(),
        "after 1: cached_path(R_OUT) present"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Completed,
        "after 1: sb(B)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

#[tokio::test]
async fn t07b_inputs_unavailable_retires_only_the_content_variant_whose_object_is_gone() {
    use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain_done(e1).evaluation_status(EvaluationStatus::Building),
        A,
    ))
    .await;
    let present_key = format!("{B_OUT}-presentc0ntent");
    case.sql(&format!(
        "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, file_size, \
             nar_size, nar_hash, debug_info_indexed, server_computed_hashes_and_sizes, \
             created_at) \
         SELECT uuidv7(), hash, '{present_key}', package, 'sha256:other', file_size, nar_size, \
             'sha256:othernar', false, true, created_at \
         FROM cached_path WHERE hash = '{B_OUT}';"
    ))
    .await;
    case.store_object(&present_key).await;

    case.build_failed(
        A,
        BuildFailureKind::InputsUnavailable,
        &[store_path(B_OUT, "b")],
    )
    .await;

    let kept: Vec<String> = ECachedPath::find()
        .filter(CCachedPath::Hash.eq(B_OUT))
        .all(&case.db.connection())
        .await
        .expect("the stored contents are read")
        .into_iter()
        .map(|row| row.nar_object_storage_key)
        .collect();
    assert_eq!(
        kept,
        vec![present_key],
        "after 1: cached_path(B_OUT) keeps only the variant with an object"
    );
    let b = case.sb(B).await;
    assert_eq!(
        (b.status, b.fetchable),
        (BuildStatus::Completed, true),
        "after 1: sb(B)"
    );
    case.finish().await;
}

#[tokio::test]
async fn t09_a_lost_worker_closes_only_the_running_attempt_and_requeues_only_building_rows() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        diamond(e1)
            .derivation(D)
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Failed, None)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment(),
    )
    .await;

    case.builds_lost(&[D, C], OrphanCause::WorkerLost).await;

    assert_eq!(
        case.attempt(D, 1).await.outcome,
        AttemptOutcome::Failed,
        "after 1: attempt(D, 1)"
    );
    let closed = case.attempt(D, 2).await;
    assert_eq!(
        (closed.outcome, closed.reason),
        (
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        ),
        "after 1: attempt(D, 2)"
    );
    assert_eq!(
        case.sb(D).await.status,
        BuildStatus::Queued,
        "after 1: sb(D)"
    );
    assert_eq!(
        case.sb(C).await.status,
        BuildStatus::Created,
        "after 1: sb(C)"
    );
    assert_eq!(
        case.counters(e1).await,
        [4, 4, 0, 1, 0],
        "after 1: counters(E1)"
    );
    case.zero_drift("step 1").await;
    let (rows, version) = (case.rows().await, case.gv(e1).await);

    case.builds_lost(&[D], OrphanCause::WorkerLost).await;

    assert_eq!(case.rows().await, rows, "after 2: nothing changes");
    assert_eq!(case.gv(e1).await, version, "after 2: gv(E1)");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t10_a_build_lost_with_its_worker_goes_to_created_when_its_conditions_regressed() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(chain_done(e1), A)).await;

    case.report_missing_nar_of_every_content_variant(B_OUT)
        .await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Building, 1),
        "after 1: sb(A)"
    );
    assert_eq!(
        case.sb(B).await.status,
        BuildStatus::Queued,
        "after 1: sb(B)"
    );
    case.zero_drift("step 1").await;

    case.builds_lost(&[A], OrphanCause::WorkerLost).await;

    let a = case.sb(A).await;
    assert_eq!(
        (a.status, a.blocking_deps),
        (BuildStatus::Created, 1),
        "after 2: sb(A)"
    );
    let closed = case.latest_attempt(A).await;
    assert_eq!(
        (closed.outcome, closed.reason),
        (
            AttemptOutcome::Aborted,
            Some(AttemptFailureReason::WorkerLost)
        ),
        "after 2: attempt(A, latest)"
    );
    assert_eq!(
        case.counters(e1).await,
        [2, 2, 0, 1, 0],
        "after 2: counters(E1)"
    );
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t12_worker_failure_report_after_build_regressed_from_queued_to_created_closes_one_attempt_and_latest_attempt_has_message()
 {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        chain_done(e1)
            .derivation(A)
            .attempt(AttemptOutcome::Failed, None),
    )
    .await;
    let dispatched_job_id = case
        .assigned(e1, A, false)
        .await
        .expect("Assigned(E1, A) is accepted");

    case.override_sb(A, |a| a.status = BuildStatus::Created)
        .await;
    case.build_started(A).await;

    assert_eq!(
        case.sb(A).await.status,
        BuildStatus::Created,
        "after 2: the Building update of sb(A) is dropped"
    );

    let dispatch_worker_failure_message = "the builder of A exited with code 1";
    case.transition(Transition::BuildFailed {
        shared_build: case.sb(A).await.id,
        error: dispatch_worker_failure_message.into(),
        log_banner: String::new(),
        kind: BuildFailureKind::Transient,
        missing_paths: Vec::new(),
        dispatch_that_reported_failure: Some(FailedBuildDispatchReport {
            dispatched_job: dispatched_job_id,
            substitute: false,
            build_context: serde_json::json!({}),
            metrics: None,
        }),
    })
    .await;

    let dispatch_attempt_rows: Vec<_> = case
        .attempts(A)
        .await
        .into_iter()
        .filter(|attempt| attempt.dispatched_job == Some(dispatched_job_id))
        .collect();
    assert_eq!(
        dispatch_attempt_rows
            .iter()
            .map(|attempt| (
                attempt.outcome,
                attempt.failure_message.as_deref(),
                attempt.build_finished_at.is_some()
            ))
            .collect::<Vec<_>>(),
        [(
            AttemptOutcome::Failed,
            Some(dispatch_worker_failure_message),
            true
        )],
        "after 3: one closed attempt of this dispatch with the reported reason"
    );
    assert_eq!(
        case.latest_attempt(A).await.failure_message.as_deref(),
        Some(dispatch_worker_failure_message),
        "after 3: the user reads the reason of this failure"
    );
    case.finish().await;
}

#[tokio::test]
async fn t13_prioritize_sets_the_flag_on_the_build_or_lifts_the_evaluation_and_its_builds() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(chain(e1)).await;

    case.prioritize_build(B).await;

    assert!(case.sb(B).await.prioritized, "after 1: sb(B).prioritized");
    assert!(!case.sb(A).await.prioritized, "after 1: sb(A).prioritized");
    case.zero_drift("step 1").await;

    case.transition(Transition::PrioritizeEvaluation { evaluation: e1 })
        .await;

    assert!(
        case.evaluation(e1).await.prioritized,
        "after 2: evaluation(E1).prioritized"
    );
    let a = case.sb(A).await;
    assert!(!a.prioritized, "after 2: sb(A).prioritized stays unset");
    let lifted =
        gradient_db::scheduling::priority::shared_builds_with_qos(&case.ctx.worker_db, &[a.id])
            .await
            .expect("the priority read runs");
    assert!(lifted.contains(&a.id), "after 2: A is lifted through E1");
    case.zero_drift("step 2").await;
    case.finish().await;
}

#[tokio::test]
async fn t14_requeue_imports_thaws_the_failed_closure_except_reproducible_builder_failures() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(
        shape()
            .evaluation(e1)
            .evaluation_status(EvaluationStatus::Failed)
            .derivation(I)
            .walked()
            .entry_point("pkg.i")
            .job()
            .probed()
            .drv_cached()
            .output(I_OUT)
            .ifd()
            .depends_on(P)
            .depends_on(Q)
            .build_status(BuildStatus::DependencyFailed)
            .derivation(P)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(P_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::NotEnoughResources),
            )
            .derivation(Q)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(Q_OUT)
            .build_status(BuildStatus::DependencyFailed)
            .depends_on(R)
            .derivation(R)
            .walked()
            .job()
            .probed()
            .drv_cached()
            .output(R_OUT)
            .build_status(BuildStatus::FailedPermanent)
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            ),
    )
    .await;

    case.requeue_imports(e1, &[I]).await;

    let p = case.sb(P).await;
    assert_eq!(
        (p.status, p.attempt, p.probed, p.wanted),
        (BuildStatus::Skipped, 0, true, false),
        "after 1: sb(P)"
    );
    assert_eq!(
        case.sb(I).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(I)"
    );
    assert_eq!(
        case.sb(Q).await.status,
        BuildStatus::DependencyFailed,
        "after 1: sb(Q)"
    );
    assert_eq!(
        case.sb(R).await.status,
        BuildStatus::FailedPermanent,
        "after 1: sb(R)"
    );
    assert_eq!(
        case.evaluation(e1).await.status,
        EvaluationStatus::Failed,
        "after 1: evaluation(E1)"
    );
    case.zero_drift("step 1").await;
    case.finish().await;
}

const SECOND_WORKER_ID_FOR_SECOND_DISPATCHED_JOB: &str = "w-second";
const CORE_BUILD_KILL_STEPS: [u32; 6] = [8, 6, 4, 3, 2, 1];

#[tokio::test]
async fn t11_report_sender_from_named_dispatched_job_or_none() {
    let e1 = EvaluationId::now_v7();
    let case = Case::start(building(
        chain(e1).evaluation_status(EvaluationStatus::Building),
        B,
    ))
    .await;
    let project = case.db.project_of(e1).await;
    let mut second_worker_job_row = build_assignment(
        e1,
        project,
        case.db.task_of(e1).await,
        case.sb(B).await.id,
        gradient_types::now(),
    );
    second_worker_job_row.worker_id = SECOND_WORKER_ID_FOR_SECOND_DISPATCHED_JOB.into();
    second_worker_job_row.finished_at = Some(gradient_types::now());
    let other_worker_second_dispatched_job_id = second_worker_job_row
        .into_active_model()
        .insert(&case.db.connection())
        .await
        .expect("the dispatch row is written")
        .id;

    assert_eq!(
        find_worker_project_and_credentials_for_dispatched_job(
            &case.ctx,
            Some(other_worker_second_dispatched_job_id)
        )
        .await
        .ok()
        .flatten()
        .map(|sender| (sender.worker_id, sender.project)),
        Some((
            SECOND_WORKER_ID_FOR_SECOND_DISPATCHED_JOB.to_owned(),
            project
        )),
        "The report identity comes from the dispatched job, not from the latest build attempt."
    );
    assert!(
        find_worker_project_and_credentials_for_dispatched_job(&case.ctx, None)
            .await
            .ok()
            .flatten()
            .is_none(),
        "Reports with no dispatched job return no sender. No worker and no project."
    );
    case.finish().await;
}

#[tokio::test]
async fn two_projects_alternate_dispatch_of_one_shared_build_each_killed_out_of_memory_at_its_next_lower_core_step_fails_permanently_after_eleven_dispatches_and_is_never_requeued()
 {
    let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
    let case = Case::start(chain(e1).evaluation(e2)).await;
    let second_alternating_project_id = case.db.project_of(e2).await;
    let alternating_evaluation_and_project_id_pairs = [
        (e1, case.db.project_of(e1).await),
        (e2, second_alternating_project_id),
    ];
    let max_build_attempts = usize::try_from(case.ctx.config.build.max_attempts.get())
        .expect("The maximum attempt count can fit in usize.");
    let mut next_core_step_index = [0_usize; 2];
    let mut dispatches_sent_until_shared_build_ended = 0_usize;

    while case.sb(B).await.status != BuildStatus::FailedPermanent {
        assert!(
            dispatches_sent_until_shared_build_ended
                < alternating_evaluation_and_project_id_pairs.len() * CORE_BUILD_KILL_STEPS.len()
                    + max_build_attempts,
            "After {} dispatches: out-of-memory retry loop does not end",
            dispatches_sent_until_shared_build_ended
        );
        let shared_build_turn_project_index = dispatches_sent_until_shared_build_ended
            % alternating_evaluation_and_project_id_pairs.len();
        let (evaluation, project) =
            alternating_evaluation_and_project_id_pairs[shared_build_turn_project_index];
        let core_count_reported_at_oom_kill = CORE_BUILD_KILL_STEPS[next_core_step_index
            [shared_build_turn_project_index]
            .min(CORE_BUILD_KILL_STEPS.len() - 1)];
        next_core_step_index[shared_build_turn_project_index] += 1;
        let dispatched_job = gradient_entity::dispatched_job::Model {
            finished_at: Some(gradient_types::now()),
            ..build_assignment(
                evaluation,
                project,
                case.db.task_of(evaluation).await,
                case.sb(B).await.id,
                gradient_types::now(),
            )
        }
        .into_active_model()
        .insert(&case.db.connection())
        .await
        .expect("the dispatch row is written")
        .id;
        case.transition(Transition::BuildFailed {
            shared_build: case.sb(B).await.id,
            error: "oom".into(),
            log_banner: String::new(),
            kind: BuildFailureKind::Transient,
            missing_paths: Vec::new(),
            dispatch_that_reported_failure: Some(FailedBuildDispatchReport {
                dispatched_job,
                substitute: false,
                build_context: serde_json::json!({}),
                metrics: Some(BuildMetrics {
                    oom_killed: true,
                    peak_ram_mb: Some(100_000),
                    build_cores: Some(core_count_reported_at_oom_kill),
                    ..Default::default()
                }),
            }),
        })
        .await;
        case.requeue().await;
        dispatches_sent_until_shared_build_ended += 1;
    }

    assert_eq!(
        dispatches_sent_until_shared_build_ended, 11,
        "two projects that alternate on one shared build end it after their core steps, within projects times core steps plus the attempt limit"
    );
    case.finish().await;
}
