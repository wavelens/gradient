/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::ids::{
    DerivationId, DispatchedJobId, EntryPointId, EvaluationId, ExecJobAttemptId, ExecJobId,
};
use gradient_entity::{entry_point, exec_job, exec_job_attempt, exec_job_phase, project_access};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, project, public_project, superuser, task, user};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::exec::{DeviceRequest, ExecSpec};
use gradient_types::waiting_reason::{ExecUnmet, WaitingReason};
use sea_orm::MockDatabase;
use serde_json::Value;
use uuid::Uuid;

fn job_id() -> ExecJobId {
    ExecJobId::new(Uuid::from_u128(0x7a))
}

fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

fn attempt_id() -> ExecJobAttemptId {
    ExecJobAttemptId::new(Uuid::from_u128(0xa1))
}

fn job(status: ExecJobStatus) -> exec_job::Model {
    let spec = ExecSpec {
        nodes: 1,
        labels: vec!["lab-a".into()],
        devices: vec![DeviceRequest {
            name: "gpu:h100".into(),
            count: 8,
        }],
        ..ExecSpec::default()
    };
    let waiting = WaitingReason::ExecWorkers {
        unmet: ExecUnmet::default(),
        connected_workers: 2,
        matching_workers: 1,
    };
    exec_job::Model {
        id: job_id(),
        evaluation: evaluation_id(),
        entry_point: EntryPointId::new(Uuid::from_u128(0xe9)),
        program_derivation: DerivationId::new(Uuid::from_u128(0xd1)),
        spec: serde_json::to_value(spec).expect("spec json"),
        status,
        waiting_reason: Some(waiting.to_json()),
        created_at: test_date(),
        ..Default::default()
    }
}

fn lost_attempt() -> exec_job_attempt::Model {
    exec_job_attempt::Model {
        id: attempt_id(),
        exec_job: job_id(),
        assignment_id: DispatchedJobId::new(Uuid::from_u128(0xd5)),
        worker: "w1".into(),
        number: 1,
        started_at: test_date(),
        ended_at: Some(test_date()),
        end_reason: Some(AttemptEndReason::Lost),
        message: None,
    }
}

fn with_job(db: MockDatabase, status: ExecJobStatus) -> MockDatabase {
    db.append_query_results([vec![job(status)]])
        .append_query_results([vec![entry_point::Model {
            eval: "apps.x86_64-linux.train".into(),
            ..Default::default()
        }]])
        .append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![lost_attempt()]])
        .append_query_results([vec![exec_job_phase::Model {
            attempt: attempt_id(),
            name: "epoch-3".into(),
            created_at: test_date(),
            ..Default::default()
        }]])
        .append_query_results([vec![task()]])
}

#[tokio::test]
async fn an_exec_job_shows_its_wait_reason_and_attempts() {
    let db =
        with_job(mock_db(), ExecJobStatus::Queued).append_query_results([vec![public_project()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}", job_id()))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let job = &body["message"];
    assert_eq!(job["status"], "queued");
    assert_eq!(job["entry_point"], "apps.x86_64-linux.train");
    assert_eq!(job["waiting_reason"]["kind"], "exec_workers");
    assert_eq!(job["waiting_reason"]["matching_workers"], 1);
    assert_eq!(job["labels"][0], "lab-a");
    assert_eq!(job["devices"][0]["count"], 8);
    assert_eq!(job["attempts"][0]["number"], 1);
    assert_eq!(job["attempts"][0]["end_reason"], "lost");
    assert_eq!(job["phases"][0]["attempt"], 1);
    assert_eq!(job["worker"], Value::Null, "a queued job runs nowhere");
}

#[tokio::test]
async fn a_started_exec_job_names_the_worker_of_its_attempt() {
    let db =
        with_job(mock_db(), ExecJobStatus::Running).append_query_results([vec![public_project()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}", job_id()))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["worker"], "w1");
}

#[tokio::test]
async fn an_outsider_cannot_read_a_private_exec_job() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec-jobs/{}", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_finished_exec_job_cannot_be_aborted() {
    let (db, auth) = signed_in_as(superuser());
    let db = with_job(db, ExecJobStatus::Completed).append_query_results([vec![project()]]);

    let res = make_test_server(db.into_connection())
        .post(&format!("/api/v1/exec-jobs/{}/abort", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn get_worker_answers_404_for_an_unknown_worker() {
    let (db, auth) = signed_in_as(superuser());

    let res = make_test_server(db.into_connection())
        .get("/api/v1/admin/workers/nope")
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}
