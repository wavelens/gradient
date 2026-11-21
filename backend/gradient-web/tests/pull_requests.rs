/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use gradient_entity::ids::{GitHostConnectionId, TaskActionId};
use gradient_entity::{
    git_host_connection, git_host_connection_repository_grant, project, task, task_action,
};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, live_session, project, project_member, task, user};
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{
    make_test_server, make_token, spawn_loopback_http_server_with_connection_counter,
};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::{
    ActionConfig, ActionType, GitHostType, PatchGeneratorKind, PrGranularity, SessionId, VerifyGate,
};
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use uuid::Uuid;

const PATCH: &str = "diff --git a/flake.lock b/flake.lock\n--- a/flake.lock\n+++ b/flake.lock\n@@ -1 +1 @@\n-a\n+b\n";

fn authorized_db(session_id: SessionId) -> MockDatabase {
    authorized_db_with_task(session_id, task())
}

fn authorized_db_with_task(session_id: SessionId, task: task::Model) -> MockDatabase {
    let session = live_session(session_id);
    mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![project::Model {
            id: project_id(),
            name: "test-project".into(),
            display_name: "Test Project".into(),
            public_key: "ssh-ed25519 AAAA test".into(),
            private_key: "encrypted".into(),
            created_by: user_id(),
            created_at: test_date(),
            ..Default::default()
        }]])
        .append_query_results([vec![task]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
}

async fn open(db: MockDatabase, session_id: SessionId, body: Value) -> axum_test::TestResponse {
    make_test_server(db.into_connection())
        .post("/api/v1/tasks/test-project/test-task/pull-requests")
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&body)
        .await
}

#[tokio::test]
async fn a_task_without_an_open_pr_action_answers_409() {
    let session_id = SessionId::now_v7();
    let db = authorized_db(session_id).append_query_results([Vec::<task_action::Model>::new()]);

    let res = open(
        db,
        session_id,
        json!({ "title": "update", "patch": PATCH, "commit": "a".repeat(40) }),
    )
    .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
    assert_eq!(
        res.json::<Value>()["message"],
        "There is no open_pr action on the task to specify a connection to the Git host."
    );
}

#[tokio::test]
async fn task_with_server_disk_repository_answers_409_before_git_fetch() {
    let session_id = SessionId::now_v7();
    let local_task = task::Model {
        repository: "file:owner/repo".into(),
        ..task()
    };
    let open_pr = task_action::Model {
        id: TaskActionId::now_v7(),
        task: task_id(),
        name: "open-pr".into(),
        action_type: ActionType::OpenPr,
        config: serde_json::to_value(ActionConfig::OpenPr {
            git_host_connection_id: GitHostConnectionId::now_v7(),
            generator: PatchGeneratorKind::FlakeLock,
            granularity: PrGranularity::PerRun,
            verify_gate: VerifyGate::Build,
            branch_pattern: "gradient/update".into(),
            title_template: None,
            body_template: None,
            update_existing: true,
        })
        .unwrap(),
        events: json!([]),
        active: true,
        last_fired_at: None,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
    };
    let db = authorized_db_with_task(session_id, local_task).append_query_results([vec![open_pr]]);

    let res = open(
        db,
        session_id,
        json!({ "title": "update", "patch": PATCH, "commit": "a".repeat(40) }),
    )
    .await;

    assert_eq!(res.status_code(), StatusCode::CONFLICT);
    assert_eq!(
        res.json::<Value>()["message"],
        "the repository URL of the task does not match an allowed URL: URLs pointing to local files are not allowed"
    );
}

#[tokio::test]
async fn a_user_without_a_commit_answers_400() {
    let session_id = SessionId::now_v7();

    let res = open(
        authorized_db(session_id),
        session_id,
        json!({ "title": "update", "patch": PATCH }),
    )
    .await;

    res.assert_status_bad_request();
    assert_eq!(
        res.json::<Value>()["message"],
        "commit is required outside an exec job"
    );
}

#[tokio::test]
async fn post_pull_request_with_invalid_base_branch_name_returns_400_before_start_commit_read_or_git_host_request()
 {
    for base in ["../main", "a..b", "fix\nx", "fix\u{7}"] {
        let session_id = SessionId::now_v7();
        let db = authorized_db(session_id).into_connection();

        let res = make_test_server(db.clone())
            .post("/api/v1/tasks/test-project/test-task/pull-requests")
            .add_header(
                "Authorization",
                format!("Bearer {}", make_token(session_id)),
            )
            .json(&json!({ "title": "update", "patch": PATCH, "commit": "a".repeat(40), "base": base }))
            .await;

        res.assert_status_bad_request();
        assert_eq!(
            res.json::<Value>()["message"],
            "Values of `base` follow the git rule for branch names: no `..`, no control characters, no spaces and no `~ ^ : ? * [ \\`.",
            "{base:?}"
        );
        assert_eq!(db.into_transaction_log().len(), 7, "{base:?}");
    }
}

#[tokio::test]
async fn pull_request_via_api_using_instance_owned_git_host_connection_without_repository_grant_gets_403_forbidden_and_sends_nothing_to_git_host()
 {
    let (base_url, git_host_connections) =
        spawn_loopback_http_server_with_connection_counter("SECRET-GIT-HOST-BODY");
    let base_url = base_url.replace("127.0.0.1", "localhost");
    let crypt = std::env::temp_dir().join(format!("gradient-test-crypt-{}", Uuid::now_v7()));
    std::fs::write(&crypt, "this-is-a-32-byte-crypt-key!!!!").unwrap();
    let crypt = crypt.to_string_lossy().into_owned();
    let connection = git_host_connection::Model {
        id: GitHostConnectionId::now_v7(),
        project: None,
        name: "instance-gitea".into(),
        display_name: "Instance Gitea".into(),
        git_host_type: GitHostType::Gitea,
        base_url: Some(base_url.clone()),
        access_token: Some(gradient_sources::encrypt_secret(&crypt, "token").unwrap()),
        reports_status: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    };
    let session_id = SessionId::now_v7();
    let session = live_session(session_id);
    let task_on_loopback_host = task::Model {
        repository: format!("{base_url}/acme/app.git"),
        ..task()
    };
    let open_pr_action = task_action::Model {
        id: TaskActionId::now_v7(),
        task: task_on_loopback_host.id,
        name: "update-inputs".into(),
        action_type: ActionType::OpenPr,
        config: json!({ "type": "open_pr", "git_host_connection_id": connection.id }),
        events: json!([]),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };
    let db = mock_db()
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![task_on_loopback_host]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![open_pr_action]])
        .append_query_results([vec![connection]])
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<git_host_connection_repository_grant::Model>::new()])
        .into_connection();
    let server = TestState::new()
        .web_db(db.clone())
        .worker_db(db)
        .cli(move |cli| {
            cli.secrets.crypt_file = crypt;
            cli.outbound.allowed_non_public_network_targets = vec!["localhost".into()];
        })
        .server();

    let res = server
        .post("/api/v1/tasks/test-project/test-task/pull-requests")
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(
            &json!({ "title": "update", "patch": PATCH, "commit": "a".repeat(40), "base": "main" }),
        )
        .await;

    assert_eq!(res.status_code(), StatusCode::FORBIDDEN);
    assert_eq!(git_host_connections.load(Ordering::SeqCst), 0);
}
