/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::{Method, StatusCode};
use base64::Engine;
use gradient_entity::ids::EvaluationId;
use gradient_entity::project_access;
use gradient_fixtures::ids::{commit_id, project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{
    admin_role, cache, eval_at, project, public_project, queued_evaluation, task, user,
};
use gradient_test_support::web::make_test_server;
use gradient_types::consts::BASE_ROLE_VIEW_ID;
use gradient_types::ids::{BuildJobId, DerivationBuildId, ExecJobId, TaskId};
use sea_orm::{MockDatabase, Value};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use uuid::Uuid;

const TOKEN: &str = "Bearer GRJ_0123456789012345678901234567890123456789012345678901234567890123";

fn resolved_token(
    allows_project_credentials_and_privileged_actions: bool,
) -> BTreeMap<&'static str, Value> {
    BTreeMap::from([
        ("job", Value::from(Uuid::from_u128(0x7a))),
        ("attempt", Value::from(Uuid::from_u128(0xa1))),
        ("project", Value::from(project_id().into_inner())),
        ("task", Value::from(task_id().into_inner())),
        ("project_name", Value::from("infra")),
        ("task_name", Value::from("master")),
        ("acting_user", Value::from(user_id().into_inner())),
        (
            "allows_project_credentials_and_privileged_actions",
            Value::from(allows_project_credentials_and_privileged_actions),
        ),
    ])
}

fn with_job_token(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![resolved_token(true)]])
}

fn append_resolved_job_token_without_project_credentials(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![resolved_token(false)]])
}

const JOB_TOKEN_PROJECT_CREDENTIAL_ROUTES: [&str; 2] = [
    "/api/v1/tasks/infra/master/evaluate",
    "/api/v1/tasks/infra/master/pull-requests",
];

#[tokio::test]
async fn test_job_token_without_project_credentials_gets_403_on_evaluate_and_pull_requests() {
    for route in JOB_TOKEN_PROJECT_CREDENTIAL_ROUTES {
        let db = append_resolved_job_token_without_project_credentials(mock_db())
            .append_query_results([vec![user()]]);
        let server = make_test_server(db.into_connection());

        let res = server
            .post(route)
            .add_header("Authorization", TOKEN)
            .json(&serde_json::json!({ "commit": "0123456789abcdef0123456789abcdef01234567" }))
            .await;

        assert_eq!(res.status_code(), StatusCode::FORBIDDEN, "{route}");
        assert_eq!(
            res.json::<Json>()["message"],
            "job tokens without project credentials for evaluations reach only build requests and the evaluations that those build requests started",
            "{route}"
        );
    }
}

#[tokio::test]
async fn job_token_without_project_credentials_reaches_build_request_routes() {
    let db = append_resolved_job_token_without_project_credentials(mock_db())
        .append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/build-requests/manifest")
        .add_header("Authorization", TOKEN)
        .json(&serde_json::json!({}))
        .await;

    assert_ne!(res.status_code(), StatusCode::UNAUTHORIZED);
    assert_ne!(res.status_code(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn job_token_for_evaluation_with_project_credentials_reaches_evaluate_and_pull_requests_routes()
 {
    for route in JOB_TOKEN_PROJECT_CREDENTIAL_ROUTES {
        let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
        let server = make_test_server(db.into_connection());

        let res = server
            .post(route)
            .add_header("Authorization", TOKEN)
            .json(&serde_json::json!({ "commit": "0123456789abcdef0123456789abcdef01234567" }))
            .await;

        assert_ne!(res.status_code(), StatusCode::UNAUTHORIZED, "{route}");
        assert_ne!(res.status_code(), StatusCode::FORBIDDEN, "{route}");
    }
}

const JOB_TOKEN_ALLOWED_METHOD_ROUTE_PAIRS_FOR_OWN_EVALUATION: [(Method, &str); 3] = [
    (Method::POST, "builds"),
    (Method::GET, ""),
    (Method::GET, "artefacts"),
];

async fn send_evaluation_request_with_job_token_for_parent_exec_job(
    method: Method,
    route: &str,
    parent_exec_job: Option<ExecJobId>,
) -> (StatusCode, String) {
    let evaluation = EvaluationId::new(Uuid::from_u128(0xe1));
    let evaluation_row = gradient_entity::evaluation::Model {
        parent_exec_job,
        allows_project_credentials_and_privileged_actions: false,
        ..eval_at(evaluation, 0)
    };
    let db = append_resolved_job_token_without_project_credentials(mock_db());
    let db = if method == Method::POST {
        db.append_query_results([vec![evaluation_row.clone()]])
            .append_query_results([vec![user()]])
    } else {
        db.append_query_results([vec![user()]])
            .append_query_results([vec![evaluation_row.clone()]])
    };
    let access = vec![project_access::Model {
        project: project_id(),
        user: user_id(),
        role: admin_role().id,
        ..Default::default()
    }];
    let db = db
        .append_query_results([vec![evaluation_row]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
        .append_query_results([access.clone()])
        .append_query_results([vec![admin_role()]]);
    let db = match route {
        "builds" => db
            .append_query_results([access])
            .append_query_results([vec![admin_role()]]),
        "" => (0..4).fold(db, |db, _| {
            db.append_query_results([Vec::<BTreeMap<String, Value>>::new()])
        }),
        _ => db.append_query_results([Vec::<BTreeMap<String, Value>>::new()]),
    };
    let server = make_test_server(db.into_connection());
    let path = format!("/api/v1/evals/{evaluation}/{route}");
    let path = path.trim_end_matches('/');
    let request = if method == Method::POST {
        server.post(path).json(&serde_json::json!({}))
    } else {
        server.get(path)
    };
    let response = request.add_header("Authorization", TOKEN).await;
    (response.status_code(), response.text())
}

#[tokio::test]
async fn job_token_without_project_credentials_creates_build_and_reads_its_evaluation_and_artefacts()
 {
    let own_job = Some(ExecJobId::new(Uuid::from_u128(0x7a)));
    for (method, route) in JOB_TOKEN_ALLOWED_METHOD_ROUTE_PAIRS_FOR_OWN_EVALUATION {
        let (status, body) = send_evaluation_request_with_job_token_for_parent_exec_job(
            method.clone(),
            route,
            own_job,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{method} {route}: {body}");
    }
}

#[tokio::test]
async fn job_token_without_project_credentials_gets_forbidden_on_other_evaluations_and_builds() {
    let other_job = Some(ExecJobId::new(Uuid::from_u128(0x7b)));
    for parent_exec_job in [None, other_job] {
        for (method, route) in JOB_TOKEN_ALLOWED_METHOD_ROUTE_PAIRS_FOR_OWN_EVALUATION {
            let (status, body) = send_evaluation_request_with_job_token_for_parent_exec_job(
                method.clone(),
                route,
                parent_exec_job,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {route} parent_exec_job={parent_exec_job:?}: {body}"
            );
        }
    }
}

fn assert_refused_by_scope(status: StatusCode, body: &Json) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        body["message"],
        "job tokens only reach their own project and task"
    );
}

#[tokio::test]
async fn a_job_token_reads_its_own_task() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/tasks/infra/master")
        .add_header("Authorization", TOKEN)
        .await;

    assert_ne!(res.status_code(), StatusCode::UNAUTHORIZED);
    assert_ne!(res.status_code(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_job_token_is_refused_on_account_routes() {
    let server = make_test_server(with_job_token(mock_db()).into_connection());

    let res = server
        .get("/api/v1/user/keys")
        .add_header("Authorization", TOKEN)
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn a_job_token_is_refused_on_exec_job_terminals() {
    for terminal in ["shell", "console", "forward?port=22"] {
        let server = make_test_server(with_job_token(mock_db()).into_connection());

        let res = server
            .get(&format!(
                "/api/v1/exec/00000000-0000-0000-0000-000000000000/{terminal}"
            ))
            .add_header("Authorization", TOKEN)
            .await;

        assert_refused_by_scope(res.status_code(), &res.json::<Json>());
    }
}

#[tokio::test]
async fn a_job_token_on_an_optional_route_outside_its_scope_is_refused() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get("/api/v1/projects/other")
        .add_header("Authorization", TOKEN)
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn a_revoked_job_token_is_unauthorized() {
    let db = mock_db().append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post("/api/v1/tasks/infra/master/evaluate")
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_unauthorized();
    assert_eq!(
        res.json::<Json>()["message"],
        "Your session has ended or the API key is no longer valid. Sign in again."
    );
}

#[tokio::test]
async fn entry_point_downloads_refuses_query_exec_job_token_without_project_credentials_with_403() {
    let db = append_resolved_job_token_without_project_credentials(mock_db())
        .append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());
    let token = TOKEN.trim_start_matches("Bearer ");

    let res = server
        .get(&format!(
            "/api/v1/tasks/infra/master/entry-point-downloads?eval=hello&filename=out&token={token}"
        ))
        .await;

    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Json>()["message"],
        "job tokens without project credentials for evaluations reach only build requests and the evaluations that those build requests started"
    );
}

fn mock_database_with_private_cache_job_token_and_project_access_for_project_credentials_and_privileged_actions(
    allows_project_credentials_and_privileged_actions: bool,
) -> MockDatabase {
    mock_db()
        .append_query_results([vec![cache()]])
        .append_query_results([vec![resolved_token(
            allows_project_credentials_and_privileged_actions,
        )]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![BTreeMap::from([(
            "project",
            Value::from(project_id().into_inner()),
        )])]])
        .append_query_results([vec![project_access::Model {
            project: project_id(),
            user: user_id(),
            role: BASE_ROLE_VIEW_ID,
            team: None,
        }]])
}

async fn get_nix_cache_info_status_using_job_token_password(db: MockDatabase) -> StatusCode {
    let password = TOKEN.trim_start_matches("Bearer ");
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("nix:{password}"));
    make_test_server(db.into_connection())
        .get("/cache/test-cache/nix-cache-info")
        .add_header("authorization", format!("Basic {encoded}"))
        .await
        .status_code()
}

#[tokio::test]
async fn exec_job_token_without_credentials_flag_is_rejected_on_private_cache_route() {
    let db = mock_database_with_private_cache_job_token_and_project_access_for_project_credentials_and_privileged_actions(false);
    assert_eq!(
        get_nix_cache_info_status_using_job_token_password(db).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn exec_job_token_with_project_credentials_and_privileged_actions_reads_private_cache_through_subscription()
 {
    let db = mock_database_with_private_cache_job_token_and_project_access_for_project_credentials_and_privileged_actions(true);
    assert_eq!(
        get_nix_cache_info_status_using_job_token_password(db).await,
        StatusCode::OK
    );
}

fn make_evaluation_for_other_task(
    parent_exec_job: Option<ExecJobId>,
) -> (
    gradient_entity::evaluation::Model,
    gradient_entity::task::Model,
) {
    let other_task = gradient_entity::task::Model {
        id: TaskId::now_v7(),
        name: "nightly".into(),
        ..task()
    };
    let evaluation = gradient_entity::evaluation::Model {
        parent_exec_job,
        ..queued_evaluation(other_task.id, commit_id())
    };
    (evaluation, other_task)
}

#[tokio::test]
async fn job_token_read_evaluation_of_other_task_by_id_returns_not_found() {
    let (evaluation, other_task) = make_evaluation_for_other_task(None);
    let db = with_job_token(mock_db())
        .append_query_results([vec![user()]])
        .append_query_results([vec![evaluation.clone()]])
        .append_query_results([vec![other_task]])
        .append_query_results([vec![public_project()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("/api/v1/evals/{}", evaluation.id))
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_not_found();
}

fn add_build_job_for_other_task_evaluation_in_public_project_to_mock_database(
    db: MockDatabase,
) -> (MockDatabase, BuildJobId) {
    let (evaluation, other_task) = make_evaluation_for_other_task(None);
    let build_job = gradient_entity::build_job::Model {
        id: BuildJobId::now_v7(),
        evaluation: evaluation.id,
        derivation_build: DerivationBuildId::now_v7(),
        ..Default::default()
    };
    let db = db
        .append_query_results([vec![build_job.clone()]])
        .append_query_results([vec![gradient_entity::derivation_build::Model {
            id: build_job.derivation_build,
            ..Default::default()
        }]])
        .append_query_results([vec![evaluation]])
        .append_query_results([vec![other_task]])
        .append_query_results([vec![public_project()]]);
    (db, build_job.id)
}

#[tokio::test]
async fn job_token_read_build_by_id_of_other_task_in_public_project_returns_not_found() {
    let (db, build_id) = add_build_job_for_other_task_evaluation_in_public_project_to_mock_database(
        with_job_token(mock_db()).append_query_results([vec![user()]]),
    );
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("/api/v1/builds/{build_id}"))
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn job_token_streaming_build_log_of_other_task_in_public_project_returns_not_found() {
    let (db, build_id) = add_build_job_for_other_task_evaluation_in_public_project_to_mock_database(
        with_job_token(mock_db()).append_query_results([vec![user()]]),
    );
    let server = make_test_server(db.into_connection());

    let res = server
        .post(&format!("/api/v1/builds/{build_id}/log"))
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn job_token_streaming_evaluation_logs_of_another_task_in_public_project_returns_not_found() {
    let (evaluation, other_task) = make_evaluation_for_other_task(None);
    let db = with_job_token(mock_db())
        .append_query_results([vec![user()]])
        .append_query_results([vec![evaluation.clone()]])
        .append_query_results([vec![other_task]])
        .append_query_results([vec![public_project()]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .post(&format!("/api/v1/evals/{}/builds", evaluation.id))
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_not_found();
}

#[tokio::test]
async fn job_token_reads_evaluation_of_other_task_requested_by_its_exec_job() {
    let token_job = ExecJobId::new(Uuid::from_u128(0x7a));
    let (evaluation, other_task) = make_evaluation_for_other_task(Some(token_job));
    let db = with_job_token(mock_db())
        .append_query_results([vec![user()]])
        .append_query_results([vec![evaluation.clone()]])
        .append_query_results([vec![other_task]])
        .append_query_results([vec![public_project()]])
        .append_query_results([Vec::<gradient_entity::evaluation_message::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&format!("/api/v1/evals/{}/messages", evaluation.id))
        .add_header("Authorization", TOKEN)
        .await;

    res.assert_status_ok();
}

#[tokio::test]
async fn job_token_is_rejected_when_listing_project_tasks() {
    for path in ["/api/v1/tasks/infra", "/api/v1/tasks/infra/available"] {
        let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
        let server = make_test_server(db.into_connection());

        let res = server.get(path).add_header("Authorization", TOKEN).await;

        assert_refused_by_scope(res.status_code(), &res.json::<Json>());
    }
}

#[tokio::test]
async fn job_token_in_query_parameter_is_refused_on_another_task_badge_route() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());
    let raw_token = TOKEN.trim_start_matches("Bearer ");

    let res = server
        .get(&format!(
            "/api/v1/tasks/infra/nightly/badge?token={raw_token}"
        ))
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn job_token_in_query_parameter_is_refused_on_another_task_entry_point_download() {
    let db = with_job_token(mock_db()).append_query_results([vec![user()]]);
    let server = make_test_server(db.into_connection());
    let raw_token = TOKEN.trim_start_matches("Bearer ");

    let res = server
        .get(&format!(
            "/api/v1/tasks/infra/nightly/entry-point-downloads?eval=hello&filename=out&token={raw_token}"
        ))
        .await;

    assert_refused_by_scope(res.status_code(), &res.json::<Json>());
}

#[tokio::test]
async fn request_with_token_in_header_and_query_returns_bad_request() {
    let server = make_test_server(mock_db().into_connection());
    let raw_token = TOKEN.trim_start_matches("Bearer ");

    for path in [
        "/api/v1/tasks/infra/master/badge",
        "/api/v1/tasks/infra/master/entry-point-downloads?eval=hello&filename=out",
    ] {
        let separator = if path.contains('?') { '&' } else { '?' };
        let res = server
            .get(&format!("{path}{separator}token={raw_token}"))
            .add_header("Authorization", TOKEN)
            .await;

        res.assert_status_bad_request();
    }
}
