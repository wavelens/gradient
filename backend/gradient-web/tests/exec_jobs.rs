/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::exec_job_attempt::AttemptEndReason;
use gradient_entity::ids::{
    ClusterJobId, DerivationId, DispatchedJobId, EntryPointId, EvaluationId, ExecJobAttemptId,
    ExecJobId, TaskId,
};
use gradient_entity::{
    entry_point, exec_job, exec_job_attempt, exec_job_output, exec_job_phase, project_access, role,
};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{
    eval_at, project, public_project, superuser, task, user, view_role, write_role,
};
use gradient_fixtures::test_date;
use gradient_test_support::log_storage::ReadErrorLogStorage;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{
    make_test_server, mock_db_and_auth_header_for_api_key_owned_by_user, signed_in_as,
};
use gradient_types::exec::{DeviceRequest, ExecSpec, GuestSize};
use gradient_types::permissions::{Permission, mask_from};
use gradient_types::waiting_reason::{ExecUnmet, WaitingReason};
use sea_orm::MockDatabase;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
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
        guest: GuestSize::default_request(),
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

fn finished_attempt_with_metrics() -> exec_job_attempt::Model {
    exec_job_attempt::Model {
        program_started_at: Some(test_date()),
        ended_at: Some(test_date() + chrono::Duration::seconds(30)),
        end_reason: Some(AttemptEndReason::Completed),
        ..lost_attempt()
    }
}

fn with_job(db: MockDatabase, status: ExecJobStatus) -> MockDatabase {
    with_job_and_attempt(db, status, lost_attempt())
}

fn with_job_and_attempt(
    db: MockDatabase,
    status: ExecJobStatus,
    attempt: exec_job_attempt::Model,
) -> MockDatabase {
    append_exec_job_of_task_with_status_to_mock_database(db, status, task(), attempt)
}

fn append_exec_job_of_task_with_status_to_mock_database(
    db: MockDatabase,
    status: ExecJobStatus,
    task: gradient_entity::task::Model,
    attempt: exec_job_attempt::Model,
) -> MockDatabase {
    db.append_query_results([vec![job(status)]])
        .append_query_results([vec![entry_point::Model {
            eval: "apps.x86_64-linux.train".into(),
            ..Default::default()
        }]])
        .append_query_results([vec![gradient_entity::evaluation::Model {
            task: Some(task.id),
            ..eval_at(evaluation_id(), 0)
        }]])
        .append_query_results([vec![task.clone()]])
        .append_query_results([vec![attempt]])
        .append_query_results([vec![exec_job_phase::Model {
            attempt: attempt_id(),
            name: "epoch-3".into(),
            created_at: test_date(),
            ..Default::default()
        }]])
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([vec![task]])
}

fn without_worker_names(db: MockDatabase) -> MockDatabase {
    let empty = Vec::<BTreeMap<String, sea_orm::Value>>::new;
    db.append_query_results([empty(), empty(), empty()])
}

fn without_timeline_or_cluster(db: MockDatabase) -> MockDatabase {
    let empty = Vec::<BTreeMap<String, sea_orm::Value>>::new;
    without_worker_names(db).append_query_results([empty(), empty()])
}

fn id_pair(first: (&str, Uuid), second: (&str, Uuid)) -> BTreeMap<String, sea_orm::Value> {
    BTreeMap::from([first, second].map(|(key, id)| (key.to_owned(), sea_orm::Value::from(id))))
}

fn in_cluster(db: MockDatabase, cluster: ClusterJobId) -> MockDatabase {
    let attempt = Uuid::from_u128(0xca);
    // A tuple query reads a mock row by position, so the keys sort in column order.
    without_worker_names(db)
        .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()])
        .append_query_results([vec![id_pair(
            ("assignment", Uuid::from_u128(0xd5)),
            ("cluster_attempt", attempt),
        )]])
        .append_query_results([vec![id_pair(
            ("attempt", attempt),
            ("cluster_job", cluster.into_inner()),
        )]])
}

#[tokio::test]
async fn an_exec_job_shows_its_wait_reason_and_attempts() {
    let db = with_job(mock_db(), ExecJobStatus::Queued)
        .append_query_results([vec![public_project()]])
        .append_query_results([Vec::<exec_job_output::Model>::new()]);
    let db = without_timeline_or_cluster(db);

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
    let db = without_timeline_or_cluster(db);

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
async fn a_signed_in_non_member_gets_403_not_404_when_aborting_an_exec_job_of_a_public_project() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![public_project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .post(&format!("/api/v1/exec/{}/abort", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
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
async fn a_superuser_api_key_with_only_read_permission_cannot_open_a_shell_in_an_exec_job_of_a_project_the_key_owner_is_not_a_member_of()
 {
    use sha2::{Digest, Sha256};
    let raw = "r".repeat(64);
    let key = gradient_entity::api::Model {
        id: gradient_entity::ids::ApiId::now_v7(),
        owned_by: superuser().id,
        name: "read-only".into(),
        key: Sha256::digest(raw.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        last_used_at: test_date(),
        created_at: test_date(),
        permission: Permission::ViewProject.bit() | Permission::ReadSuperuser.bit(),
        ..Default::default()
    };
    let db = mock_db()
        .append_query_results([vec![key.clone()]])
        .append_exec_results([gradient_fixtures::mock::exec_ok()])
        .append_query_results([vec![key]])
        .append_query_results([vec![superuser()]]);
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/shell", job_id()))
        .add_header("authorization", format!("Bearer GRAD{raw}"))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND, "{}", res.text());
}

#[tokio::test]
async fn a_member_without_task_write_cannot_open_a_shell() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_access::Model {
            team: None,
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
async fn a_member_without_trigger_permission_cannot_control_an_exec_job() {
    for (delete, action) in [
        (false, "abort"),
        (false, "run"),
        (false, "keep-vm-running"),
        (true, "keep-vm-running"),
    ] {
        let (db, auth) = signed_in_as(user());
        let db = with_job(db, ExecJobStatus::Running)
            .append_query_results([vec![project()]])
            .append_query_results([vec![project_access::Model {
                team: None,
                project: project().id,
                user: user().id,
                role: view_role().id,
            }]])
            .append_query_results([vec![view_role()]]);
        let server = make_test_server(db.into_connection());
        let url = format!("/api/v1/exec/{}/{action}", job_id());

        let res = if delete {
            server.delete(&url)
        } else {
            server.post(&url)
        }
        .add_header("authorization", auth)
        .await;

        assert_eq!(
            res.status_code(),
            StatusCode::FORBIDDEN,
            "delete={delete} {action}"
        );
    }
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

#[tokio::test]
async fn role_with_trigger_evaluation_but_without_view_project_gets_404_on_private_project_exec_job_endpoints()
 {
    let trigger_evaluation_role_without_view_project = role::Model {
        permission: mask_from(&[Permission::TriggerEvaluation]),
        ..view_role()
    };
    for (method, path) in [("GET", "shell"), ("POST", "run")] {
        let (db, auth) = signed_in_as(user());
        let db = with_job(db, ExecJobStatus::Running)
            .append_query_results([vec![project()]])
            .append_query_results([vec![project_access::Model {
                team: None,
                project: project().id,
                user: user().id,
                role: trigger_evaluation_role_without_view_project.id,
            }]])
            .append_query_results([vec![trigger_evaluation_role_without_view_project.clone()]]);
        let server = make_test_server(db.into_connection());
        let url = format!("/api/v1/exec/{}/{path}", job_id());

        let res = if method == "GET" {
            server.get(&url)
        } else {
            server.post(&url)
        }
        .add_header("authorization", auth)
        .await;

        assert_eq!(res.status_code(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn job_token_reading_exec_job_of_another_task_in_public_project_gets_404() {
    let other_task = gradient_entity::task::Model {
        id: TaskId::now_v7(),
        name: "nightly".into(),
        ..task()
    };
    let db = mock_db()
        .append_query_results([vec![BTreeMap::from([
            ("job", sea_orm::Value::from(Uuid::from_u128(0x7b))),
            ("attempt", sea_orm::Value::from(Uuid::from_u128(0xa2))),
            ("project", sea_orm::Value::from(project_id().into_inner())),
            ("task", sea_orm::Value::from(task_id().into_inner())),
            ("project_name", sea_orm::Value::from("infra")),
            ("task_name", sea_orm::Value::from("master")),
            ("acting_user", sea_orm::Value::from(user_id().into_inner())),
            (
                "allows_project_credentials_and_privileged_actions",
                sea_orm::Value::from(true),
            ),
        ])]])
        .append_query_results([vec![user()]]);
    let db = append_exec_job_of_task_with_status_to_mock_database(
        db,
        ExecJobStatus::Running,
        other_task,
        lost_attempt(),
    )
    .append_query_results([vec![public_project()]])
    .append_query_results([Vec::<exec_job_output::Model>::new()])
    .append_query_results([Vec::<BTreeMap<String, sea_orm::Value>>::new()]);
    let db = without_timeline_or_cluster(db);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", job_id()))
        .add_header(
            "authorization",
            "Bearer GRJ_0123456789012345678901234567890123456789012345678901234567890123",
        )
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_superuser_without_a_role_cannot_open_terminals_or_start_jobs_in_a_foreign_project() {
    for (method, path) in [
        ("GET", "shell"),
        ("GET", "console"),
        ("GET", "forward?port=22"),
        ("POST", "run"),
        ("POST", "keep-vm-running"),
    ] {
        let (db, auth) = signed_in_as(superuser());
        let db = with_job(db, ExecJobStatus::Running)
            .append_query_results([vec![project()]])
            .append_query_results([Vec::<project_access::Model>::new()]);
        let server = make_test_server(db.into_connection());
        let url = format!("/api/v1/exec/{}/{path}", job_id());

        let res = if method == "GET" {
            server.get(&url)
        } else {
            server.post(&url)
        }
        .add_header("authorization", auth)
        .await;

        assert_eq!(res.status_code(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn an_instance_admin_api_key_with_view_project_read_superuser_and_write_superuser_but_no_project_role_gets_not_found_on_shell_console_and_port_forward_of_an_exec_job_in_a_foreign_private_project()
 {
    for path in TERMINAL_PATHS {
        let (db, auth) = mock_db_and_auth_header_for_api_key_owned_by_user(
            superuser(),
            mask_from(&[
                Permission::ViewProject,
                Permission::ReadSuperuser,
                Permission::WriteSuperuser,
            ]),
            None,
        );
        let db = with_job(db, ExecJobStatus::Running)
            .append_query_results([vec![project()]])
            .append_query_results([Vec::<project_access::Model>::new()]);

        assert_eq!(
            get_exec_job_terminal_endpoint_status_code_with_authorization_header(db, auth, path)
                .await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_narrowed_api_key_of_a_superuser_cannot_abort_an_exec_job_of_a_foreign_project() {
    let (db, auth) = mock_db_and_auth_header_for_api_key_owned_by_user(
        superuser(),
        mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]),
        None,
    );
    let db = with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .post(&format!("/api/v1/exec/{}/abort", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
}

const TERMINAL_PATHS: [&str; 3] = ["shell", "console", "forward?port=22"];
const ACCESS_GRANTED_BUT_NOT_WEBSOCKET_UPGRADE_BAD_REQUEST_STATUS: StatusCode =
    StatusCode::BAD_REQUEST;

fn add_running_exec_job_project_and_member_with_role_to_mock_database(
    db: MockDatabase,
    member_role: role::Model,
) -> MockDatabase {
    with_job(db, ExecJobStatus::Running)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_access::Model {
            team: None,
            project: project().id,
            user: user().id,
            role: member_role.id,
        }]])
        .append_query_results([vec![member_role]])
}

async fn get_exec_job_terminal_endpoint_status_code_with_authorization_header(
    db: MockDatabase,
    auth: String,
    path: &str,
) -> StatusCode {
    make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/{path}", job_id()))
        .add_header("authorization", auth)
        .await
        .status_code()
}

#[tokio::test]
async fn api_key_with_only_view_project_and_trigger_evaluation_gets_forbidden_on_all_exec_job_terminal_endpoints_with_or_without_project_limit()
 {
    for pinned_project in [Some(project().id), None] {
        for path in TERMINAL_PATHS {
            let (db, auth) = mock_db_and_auth_header_for_api_key_owned_by_user(
                user(),
                mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]),
                pinned_project,
            );
            let db = add_running_exec_job_project_and_member_with_role_to_mock_database(
                db,
                write_role(),
            );

            assert_eq!(
                get_exec_job_terminal_endpoint_status_code_with_authorization_header(
                    db, auth, path
                )
                .await,
                StatusCode::FORBIDDEN,
                "{path} pinned to {pinned_project:?}"
            );
        }
    }
}

#[tokio::test]
async fn signed_in_member_role_with_trigger_evaluation_without_open_job_terminal_gets_forbidden_on_all_exec_job_terminal_endpoints()
 {
    let trigger_role = role::Model {
        permission: mask_from(&[Permission::ViewProject, Permission::TriggerEvaluation]),
        ..view_role()
    };
    for path in TERMINAL_PATHS {
        let (db, auth) = signed_in_as(user());
        let db = add_running_exec_job_project_and_member_with_role_to_mock_database(
            db,
            trigger_role.clone(),
        );

        assert_eq!(
            get_exec_job_terminal_endpoint_status_code_with_authorization_header(db, auth, path)
                .await,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}

#[tokio::test]
async fn signed_in_write_role_member_passes_permission_check_of_every_exec_job_terminal() {
    for path in TERMINAL_PATHS {
        let (db, auth) = signed_in_as(user());
        let db =
            add_running_exec_job_project_and_member_with_role_to_mock_database(db, write_role());

        assert_eq!(
            get_exec_job_terminal_endpoint_status_code_with_authorization_header(db, auth, path)
                .await,
            ACCESS_GRANTED_BUT_NOT_WEBSOCKET_UPGRADE_BAD_REQUEST_STATUS,
            "{path}"
        );
    }
}

#[tokio::test]
async fn api_key_with_view_project_and_open_job_terminal_without_trigger_evaluation_passes_exec_job_shell_access_check()
 {
    let (db, auth) = mock_db_and_auth_header_for_api_key_owned_by_user(
        user(),
        mask_from(&[Permission::ViewProject, Permission::OpenJobTerminal]),
        None,
    );
    let db = add_running_exec_job_project_and_member_with_role_to_mock_database(db, write_role());

    assert_eq!(
        get_exec_job_terminal_endpoint_status_code_with_authorization_header(db, auth, "shell")
            .await,
        ACCESS_GRANTED_BUT_NOT_WEBSOCKET_UPGRADE_BAD_REQUEST_STATUS
    );
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
    let db = without_timeline_or_cluster(
        with_two_nodes(mock_db()).append_query_results([Vec::<exec_job_output::Model>::new()]),
    );

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
    let db = without_timeline_or_cluster(
        with_two_nodes(mock_db()).append_query_results([Vec::<exec_job_output::Model>::new()]),
    );

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", node_id(1)))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["id"], node_id(0).to_string());
}

#[tokio::test]
async fn a_two_node_job_names_its_cluster_job() {
    let cluster = ClusterJobId::new(Uuid::from_u128(0xc1));
    let db = in_cluster(
        with_two_nodes(mock_db()).append_query_results([Vec::<exec_job_output::Model>::new()]),
        cluster,
    );

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}", node_id(0)))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["cluster_job"], cluster.to_string());
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
async fn a_log_storage_failure_is_a_server_error_not_an_empty_log() {
    let res = TestState::new()
        .web_db(with_two_nodes(mock_db()).into_connection())
        .log_storage(Arc::new(ReadErrorLogStorage))
        .server()
        .get(&format!("/api/v1/exec/{}/log?offset=12", node_id(0)))
        .await;

    assert_eq!(res.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
}

const BYTES_PER_MEBIBYTE: i64 = 1024 * 1024;

fn build_exec_attempt_usage_bucket_row(
    elapsed_ms: i64,
    cpu_avg: f32,
    cpu_max: f32,
) -> BTreeMap<String, sea_orm::Value> {
    BTreeMap::from(
        [
            ("elapsed_ms", sea_orm::Value::from(elapsed_ms)),
            ("cpu_avg", sea_orm::Value::from(cpu_avg)),
            ("cpu_max", sea_orm::Value::from(cpu_max)),
            (
                "ram_avg_bytes",
                sea_orm::Value::from(300 * BYTES_PER_MEBIBYTE),
            ),
            (
                "ram_max_bytes",
                sea_orm::Value::from(400 * BYTES_PER_MEBIBYTE),
            ),
            (
                "disk_max_bytes",
                sea_orm::Value::from(1024 * BYTES_PER_MEBIBYTE),
            ),
            ("whole_attempt_cpu_avg", sea_orm::Value::from(1.25_f32)),
            ("whole_attempt_cpu_max", sea_orm::Value::from(1.75_f32)),
            (
                "whole_attempt_ram_max_bytes",
                sea_orm::Value::from(412 * BYTES_PER_MEBIBYTE),
            ),
            (
                "whole_attempt_disk_max_bytes",
                sea_orm::Value::from(1100 * BYTES_PER_MEBIBYTE),
            ),
            (
                "ram_total_bytes",
                sea_orm::Value::from(480 * BYTES_PER_MEBIBYTE),
            ),
            (
                "disk_total_bytes",
                sea_orm::Value::from(1900 * BYTES_PER_MEBIBYTE),
            ),
        ]
        .map(|(key, value)| (key.to_owned(), value)),
    )
}

#[tokio::test]
async fn metrics_show_the_requested_guest_size_and_points() {
    let db = with_job_and_attempt(
        mock_db(),
        ExecJobStatus::Completed,
        finished_attempt_with_metrics(),
    )
    .append_query_results([vec![public_project()]])
    .append_query_results([vec![
        build_exec_attempt_usage_bucket_row(0, 0.5, 1.0),
        build_exec_attempt_usage_bucket_row(10_000, 1.5, 1.75),
    ]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/metrics", job_id()))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let metrics = &body["message"];
    assert_eq!(metrics["attempt"], 1);
    assert_eq!(metrics["attempt_open"], false);
    assert_eq!(metrics["requested_cpus"], 2);
    assert_eq!(metrics["requested_memory_bytes"], 500 * BYTES_PER_MEBIBYTE);
    assert_eq!(metrics["requested_disk_bytes"], 2048 * BYTES_PER_MEBIBYTE);
    assert_eq!(metrics["usable_memory_bytes"], 480 * BYTES_PER_MEBIBYTE);
    assert_eq!(metrics["usable_disk_bytes"], 1900 * BYTES_PER_MEBIBYTE);
    assert_eq!(metrics["cpu_avg"], 1.25);
    assert_eq!(metrics["cpu_max"], 1.75);
    assert_eq!(metrics["ram_max_bytes"], 412 * BYTES_PER_MEBIBYTE);
    assert_eq!(metrics["disk_max_bytes"], 1100 * BYTES_PER_MEBIBYTE);
    let points = metrics["points"].as_array().expect("points");
    assert_eq!(points.len(), 2);
    assert_eq!(points[1]["elapsed_ms"], 10_000);
    assert_eq!(points[1]["cpu_avg"], 1.5);
    assert_eq!(points[1]["cpu_max"], 1.75);
    assert_eq!(points[1]["ram_avg_bytes"], 300 * BYTES_PER_MEBIBYTE);
    assert_eq!(points[1]["ram_max_bytes"], 400 * BYTES_PER_MEBIBYTE);
    assert_eq!(points[1]["disk_max_bytes"], 1024 * BYTES_PER_MEBIBYTE);
}

#[tokio::test]
async fn metrics_of_an_unknown_attempt_are_not_found() {
    let db =
        with_job(mock_db(), ExecJobStatus::Failed).append_query_results([vec![public_project()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/metrics?attempt=7", job_id()))
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);
    let body: Value = res.json();
    assert_eq!(body["message"], "Exec job attempt not found");
}

#[tokio::test]
async fn metrics_of_a_private_project_need_view_access() {
    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Failed)
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/metrics", job_id()))
        .add_header("authorization", auth)
        .await;

    assert_eq!(res.status_code(), StatusCode::NOT_FOUND);

    let (db, auth) = signed_in_as(user());
    let db = with_job(db, ExecJobStatus::Failed)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_access::Model {
            team: None,
            project: project().id,
            user: user().id,
            role: view_role().id,
        }]])
        .append_query_results([vec![view_role()]]);

    let res = make_test_server(db.into_connection())
        .get(&format!("/api/v1/exec/{}/metrics?attempt=1", job_id()))
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["points"], Value::Array(Vec::new()));
    assert_eq!(body["message"]["cpu_max"], 0.0);
    assert_eq!(body["message"]["usable_memory_bytes"], Value::Null);
}
