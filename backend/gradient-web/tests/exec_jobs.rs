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
use gradient_entity::{
    entry_point, exec_job, exec_job_attempt, exec_job_output, exec_job_phase, project_access,
};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, project, public_project, superuser, task, user, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::exec::{DeviceRequest, ExecSpec};
use gradient_types::waiting_reason::{ExecUnmet, WaitingReason};
use sea_orm::MockDatabase;
use serde_json::Value;
use std::collections::BTreeMap;
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
        ..Default::default()
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
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([vec![task()]])
}

#[tokio::test]
async fn an_exec_job_shows_its_wait_reason_and_attempts() {
    let db = with_job(mock_db(), ExecJobStatus::Queued)
        .append_query_results([vec![public_project()]])
        .append_query_results([Vec::<exec_job_output::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", job_id()))
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
    let db = with_job(mock_db(), ExecJobStatus::Running)
        .append_query_results([vec![public_project()]])
        .append_query_results([Vec::<exec_job_output::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", job_id()))
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
        .get(&format!("/api/v1/exec/{}", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_finished_exec_job_cannot_be_aborted() {
    let (db, auth) = signed_in_as(superuser());
    let db = with_job(db, ExecJobStatus::Completed).append_query_results([vec![project()]]);

    let res = make_test_server(db.into_connection())
        .post(&format!("/api/v1/exec/{}/abort", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_member_without_task_write_cannot_open_a_shell() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_access::Model {
            project: project().id,
            user: user().id,
            role: view_role().id,
        }]])
        .append_query_results([vec![view_role()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/shell", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_outsider_cannot_open_a_console() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/console", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

fn node_id(node: i16) -> ExecJobId {
    ExecJobId::new(Uuid::from_u128(0x7a + node as u128))
}

fn node_row(node: i16) -> exec_job::Model {
    exec_job::Model {
        id: node_id(node),
        node_index: node,
        waiting_reason: None,
        ..job(ExecJobStatus::Running)
    }
}

fn node_attempt(node: i16, worker: &str, address: &str) -> exec_job_attempt::Model {
    exec_job_attempt::Model {
        id: ExecJobAttemptId::new(Uuid::from_u128(0xa1 + node as u128)),
        exec_job: node_id(node),
        worker: worker.into(),
        ended_at: None,
        end_reason: None,
        master_port: Some(29500),
        guest_address: Some(address.into()),
        ..lost_attempt()
    }
}

fn with_two_nodes(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![node_row(0), node_row(1)]])
        .append_query_results([vec![entry_point::Model {
            eval: "apps.x86_64-linux.pair".into(),
            ..Default::default()
        }]])
        .append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![
            node_attempt(0, "w1", "10.233.0.2"),
            node_attempt(1, "w2", "10.234.0.2"),
        ]])
        .append_query_results([Vec::<exec_job_phase::Model>::new()])
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([vec![task()]])
        .append_query_results([vec![public_project()]])
}

#[tokio::test]
async fn a_two_node_job_lists_both_nodes() {
    let db = with_two_nodes(mock_db()).append_query_results([Vec::<exec_job_output::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", node_id(0)))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let nodes = body["message"]["node_jobs"]
        .as_array()
        .expect("node_jobs")
        .clone();
    let view = |key: &str| nodes.iter().map(|n| n[key].clone()).collect::<Vec<_>>();
    assert_eq!(view("node"), vec![0, 1]);
    assert_eq!(view("worker"), vec!["w1", "w2"]);
    assert_eq!(view("guest_address"), vec!["10.233.0.2", "10.234.0.2"]);
    assert_eq!(view("master_port"), vec![29500, 29500]);
    assert_eq!(nodes[1]["attempts"][0]["number"], 1);
}

#[tokio::test]
async fn a_node_row_id_answers_the_same_job() {
    let db = with_two_nodes(mock_db()).append_query_results([Vec::<exec_job_output::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", node_id(1)))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["id"], node_id(0).to_string());
}

#[tokio::test]
async fn the_log_of_an_unknown_node_is_404() {
    let db = with_two_nodes(mock_db());

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/log?node=2", node_id(0)))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    let body: Value = res.json();
    assert_eq!(body["message"], "no node 2 in this job");
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
