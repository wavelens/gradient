/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use serde_json::json;

#[tokio::test]
async fn a_project_patch_may_resend_its_own_name() {
    let session_id = SessionId::now_v7();
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![project()]])
        .append_exec_results([exec_ok(), exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/projects/test-project")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "name": project().name, "display_name": "Renamed" }))
        .await;

    res.assert_status_ok();
}

#[tokio::test]
async fn a_task_patch_may_resend_its_own_name() {
    let session_id = SessionId::now_v7();
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![project()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![task()]])
        .append_exec_results([exec_ok(), exec_ok()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .patch("/api/v1/tasks/test-project/test-task")
        .add_header(
            "authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .json(&json!({ "name": task().name, "display_name": "Renamed" }))
        .await;

    res.assert_status_ok();
}
