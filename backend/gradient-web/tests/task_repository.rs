/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::incoming_webhook_delivery::{self, DeliveryResult};
use gradient_entity::integration::IntegrationKind;
use gradient_entity::{ids::*, integration, task, task_action, task_trigger};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{admin_role, project, project_member, view_role};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use gradient_types::{SessionId, TriggerType};
use sea_orm::MockDatabase;
use serde_json::{Value, json};
use uuid::Uuid;

const URL: &str = "/api/v1/tasks/test-project/test-task/repository";

fn ssh_task() -> task::Model {
    task::Model {
        repository: "git@gitea.example.com:acme/app.git".into(),
        ..gradient_fixtures::rows::task()
    }
}

fn inbound() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000010").unwrap()),
        project: project_id(),
        name: "gitea-example-com".into(),
        display_name: "gitea.example.com".into(),
        kind: IntegrationKind::Inbound,
        endpoint_url: Some("https://gitea.example.com".into()),
        secret: Some("encrypted".into()),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn push_trigger() -> task_trigger::Model {
    task_trigger::Model {
        id: TaskTriggerId::new(Uuid::parse_str("a0000000-0000-0000-0000-0000000000ff").unwrap()),
        task: task_id(),
        trigger_type: TriggerType::ReporterPush,
        config: json!({ "integration_id": inbound().id.to_string(), "branches": ["main"], "tags": [] }),
        active: true,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn with_task_scope(db: MockDatabase, role: RoleId) -> MockDatabase {
    db.append_query_results([vec![project()]])
        .append_query_results([vec![ssh_task()]])
        .append_query_results([vec![project_member(role)]])
        .append_query_results([vec![role_row(role)]])
}

fn with_mask(db: MockDatabase, role: RoleId) -> MockDatabase {
    db.append_query_results([vec![project_member(role)]])
        .append_query_results([vec![role_row(role)]])
}

fn role_row(role: RoleId) -> gradient_entity::role::Model {
    if role == BASE_ROLE_ADMIN_ID {
        admin_role()
    } else {
        view_role()
    }
}

#[tokio::test]
async fn members_see_the_whole_repository_connection() {
    let session_id = SessionId::now_v7();
    let delivery = incoming_webhook_delivery::Model {
        id: IncomingWebhookDeliveryId::now_v7(),
        integration: inbound().id,
        event: "push".into(),
        status_code: 200,
        result: DeliveryResult::Queued,
        queued: 1,
        created_at: test_date(),
        ..Default::default()
    };
    let db = with_mask(
        with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_ADMIN_ID),
        BASE_ROLE_ADMIN_ID,
    )
    .append_query_results([vec![push_trigger()]])
    .append_query_results([Vec::<task_action::Model>::new()])
    .append_query_results([vec![inbound()]])
    .append_query_results([vec![delivery]]);

    let res = make_test_server(db.into_connection())
        .get(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let repo = &body["message"];
    assert_eq!(repo["uses_ssh"], true);
    assert!(
        repo["deploy_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519")
    );
    assert_eq!(repo["build_on_push"]["branches"], json!(["main"]));
    assert_eq!(repo["last_event"]["result"], "queued");
    assert_eq!(repo["integrations"][0]["serves_repository"], true);
    assert!(
        repo["integrations"][0]["webhook_url"]
            .as_str()
            .unwrap()
            .ends_with("/api/v1/hooks/gitea/test-project/gitea-example-com")
    );
    assert_eq!(repo["integrations"][0]["has_secret"], true);
    assert_eq!(repo["can_edit"], true);
}

#[tokio::test]
async fn view_members_see_no_credential_state_and_cannot_edit() {
    let session_id = SessionId::now_v7();
    let db = with_mask(
        with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_VIEW_ID),
        BASE_ROLE_VIEW_ID,
    )
    .append_query_results([Vec::<task_trigger::Model>::new()])
    .append_query_results([Vec::<task_action::Model>::new()])
    .append_query_results([vec![inbound()]]);

    let res = make_test_server(db.into_connection())
        .get(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["can_edit"], false);
    assert_eq!(body["message"]["can_manage_integrations"], false);
    assert!(body["message"]["integrations"][0]["has_secret"].is_null());
}

#[tokio::test]
async fn view_members_see_that_a_status_report_failed_but_not_the_error() {
    let session_id = SessionId::now_v7();
    let status_action = task_action::Model {
        id: TaskActionId::now_v7(),
        task: task_id(),
        action_type: gradient_types::actions::ActionType::GitHostStatusReport,
        config: json!({ "type": "git_host_status_report", "integration_id": inbound().id }),
        active: true,
        created_by: user_id(),
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    };
    let failed = gradient_entity::task_action_delivery::Model {
        id: TaskActionDeliveryId::now_v7(),
        action_id: status_action.id,
        success: false,
        error_message: Some("POST https://git.internal.example/api: 500".into()),
        delivered_at: test_date(),
        ..Default::default()
    };
    let db = with_mask(
        with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_VIEW_ID),
        BASE_ROLE_VIEW_ID,
    )
    .append_query_results([Vec::<task_trigger::Model>::new()])
    .append_query_results([vec![status_action]])
    .append_query_results([vec![inbound()]])
    .append_query_results([vec![failed]]);

    let res = make_test_server(db.into_connection())
        .get(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    let delivery = &body["message"]["report_status"]["last_delivery"];
    assert_eq!(delivery["success"], false);
    assert!(delivery["error_message"].is_null(), "{delivery}");
}

#[tokio::test]
async fn turning_on_push_builds_needs_a_receiving_integration() {
    let session_id = SessionId::now_v7();
    let db = with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_ADMIN_ID);
    let db = with_mask(db, BASE_ROLE_ADMIN_ID).append_query_results([vec![inbound()]]);

    let res = make_test_server(db.into_connection())
        .put(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "build_on_push": true }))
        .await;

    res.assert_status_bad_request();
}

#[tokio::test]
async fn turning_on_push_builds_creates_the_push_trigger() {
    let session_id = SessionId::now_v7();
    let db = with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_ADMIN_ID);
    let db = with_mask(db, BASE_ROLE_ADMIN_ID)
        .append_query_results([vec![inbound()]])
        .append_query_results([vec![ssh_task()]])
        .append_query_results([Vec::<task_trigger::Model>::new()])
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![push_trigger()]]);
    let db = with_mask(db, BASE_ROLE_ADMIN_ID)
        .append_query_results([vec![push_trigger()]])
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![inbound()]])
        .append_query_results([Vec::<incoming_webhook_delivery::Model>::new()]);
    let conn = db.into_connection();
    let log = conn.clone();

    let res = make_test_server(conn)
        .put(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({
            "inbound_integration": inbound().id,
            "build_on_push": true,
            "push_branches": ["main", " "],
        }))
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["build_on_push"]["active"], true);
    let statements = format!("{:?}", log.into_transaction_log());
    assert!(statements.contains("INSERT INTO"), "{statements}");
    assert!(statements.contains("task_trigger"), "{statements}");
}

#[tokio::test]
async fn turning_off_push_builds_deactivates_every_push_trigger() {
    let session_id = SessionId::now_v7();
    let duplicate = task_trigger::Model {
        id: TaskTriggerId::now_v7(),
        ..push_trigger()
    };
    let inactive = |t: task_trigger::Model| task_trigger::Model { active: false, ..t };
    let db = with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_ADMIN_ID);
    let db = with_mask(db, BASE_ROLE_ADMIN_ID)
        .append_query_results([vec![inbound()]])
        .append_query_results([vec![ssh_task()]])
        .append_query_results([vec![push_trigger(), duplicate.clone()]])
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![inactive(push_trigger())]])
        .append_query_results([vec![inactive(duplicate.clone())]]);
    let db = with_mask(db, BASE_ROLE_ADMIN_ID)
        .append_query_results([vec![inactive(push_trigger()), inactive(duplicate)]])
        .append_query_results([Vec::<task_action::Model>::new()])
        .append_query_results([vec![inbound()]])
        .append_query_results([Vec::<incoming_webhook_delivery::Model>::new()]);
    let conn = db.into_connection();
    let log = conn.clone();

    let res = make_test_server(conn)
        .put(URL)
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "inbound_integration": inbound().id, "build_on_push": false }))
        .await;

    res.assert_status_ok();
    let trigger_updates = gradient_fixtures::mock::raw_statements(log.into_transaction_log())
        .iter()
        .filter(|s| s.sql.starts_with(r#"UPDATE "task_trigger""#))
        .count();
    assert_eq!(trigger_updates, 2);
}

fn outbound_without_token() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("a0000000-0000-0000-0000-000000000020").unwrap()),
        kind: IntegrationKind::Outbound,
        secret: None,
        ..inbound()
    }
}

#[tokio::test]
async fn installing_a_deploy_key_needs_a_git_host_token() {
    let session_id = SessionId::now_v7();
    let db = with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_ADMIN_ID)
        .append_query_results([vec![inbound(), outbound_without_token()]]);

    let res = make_test_server(db.into_connection())
        .post(&format!("{URL}/install-deploy-key"))
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "outbound_integration": outbound_without_token().id }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn installing_a_deploy_key_refuses_a_git_host_in_a_private_network() {
    let session_id = SessionId::now_v7();
    let internal_outbound = integration::Model {
        endpoint_url: Some("http://10.0.0.5:8080".into()),
        access_token: Some("encrypted".into()),
        ..outbound_without_token()
    };
    let internal_task = task::Model {
        repository: "http://10.0.0.5:8080/acme/app.git".into(),
        ..gradient_fixtures::rows::task()
    };
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![internal_task]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![internal_outbound.clone()]]);

    let res = make_test_server(db.into_connection())
        .post(&format!("{URL}/install-deploy-key"))
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "outbound_integration": internal_outbound.id }))
        .await;

    res.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("10.0.0.5"),
        "{body}"
    );
}

#[tokio::test]
async fn view_members_cannot_install_webhooks() {
    let session_id = SessionId::now_v7();
    let db = with_task_scope(with_auth(mock_db(), session_id), BASE_ROLE_VIEW_ID)
        .append_query_results([vec![view_role()]]);

    let res = make_test_server(db.into_connection())
        .post(&format!("{URL}/install-webhook"))
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({
            "inbound_integration": inbound().id,
            "outbound_integration": outbound_without_token().id,
        }))
        .await;

    res.assert_status_forbidden();
}
