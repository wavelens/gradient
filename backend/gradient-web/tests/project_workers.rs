/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::RoleId;
use gradient_entity::{role, worker_registration};
use gradient_fixtures::ids::project_id;
use gradient_fixtures::mock::ran;
use gradient_fixtures::rows::{
    admin_role, project, project_member, user, view_role, worker_registration,
};
use gradient_test_support::web::{make_test_server, signed_in_as};
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use serde_json::json;
use uuid::Uuid;

const WORKERS: &str = "/api/v1/projects/test-project/workers";
const WORKER: &str = "550e8400-e29b-41d4-a716-446655440001";

fn role_without_permissions() -> role::Model {
    role::Model {
        id: RoleId::new(Uuid::from_u128(0x0e)),
        name: "nothing".into(),
        project: Some(project_id()),
        permission: 0,
        managed: false,
    }
}

async fn assert_worker_changes_forbidden(role: role::Model) {
    let worker = format!("{WORKERS}/{WORKER}");
    for (method, url) in [
        ("POST", WORKERS.to_owned()),
        ("POST", format!("{worker}/test")),
        ("PATCH", worker.clone()),
        ("DELETE", worker.clone()),
    ] {
        let (db, auth) = signed_in_as(user());
        let db = db
            .append_query_results([vec![project()]])
            .append_query_results([vec![project_member(role.id)]])
            .append_query_results([vec![role.clone()]]);
        let server = make_test_server(db.into_connection());

        let request = match method {
            "POST" if url == WORKERS => server.post(&url).json(&json!({
                "worker_id": WORKER,
                "display_name": "rogue",
                "enable_secrets": true,
            })),
            "POST" => server.post(&url),
            "PATCH" => server.patch(&url).json(&json!({ "active": false })),
            _ => server.delete(&url),
        };
        let res = request.add_header("authorization", auth).await;

        assert_eq!(
            res.status_code(),
            StatusCode::FORBIDDEN,
            "{} {method} {url}",
            role.name
        );
    }
}

#[tokio::test]
async fn a_member_without_manage_workers_cannot_change_project_workers() {
    assert_worker_changes_forbidden(role_without_permissions()).await;
}

#[tokio::test]
async fn a_view_member_cannot_change_project_workers() {
    assert_worker_changes_forbidden(view_role()).await;
}

#[tokio::test]
async fn a_state_managed_project_worker_cannot_be_deleted() {
    let (db, auth) = signed_in_as(user());
    let conn = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![worker_registration::Model {
            peer_id: project_id(),
            worker_id: WORKER.into(),
            managed: true,
            ..worker_registration(true, true)
        }]])
        .into_connection();
    let server = make_test_server(conn.clone());

    let res = server
        .delete(&format!("{WORKERS}/{WORKER}"))
        .add_header("authorization", auth)
        .await;

    res.assert_status(StatusCode::CONFLICT);
    drop(server);
    assert!(!ran(conn, "DELETE FROM \"worker_registration\""));
}
