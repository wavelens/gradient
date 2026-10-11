/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::{role, task};
use gradient_fixtures::ids::{project_id, task_id, user_id};
use gradient_fixtures::rows::{project, project_member};
use gradient_fixtures::test_date;
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::ConcurrencyPolicy;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use gradient_types::permissions::admin_mask;
use serde_json::Value;

fn admin_role_row() -> role::Model {
    role::Model {
        id: gradient_types::consts::BASE_ROLE_ADMIN_ID,
        name: "Admin".into(),
        permission: admin_mask(),
        ..Default::default()
    }
}

fn task_with(sign_cache: bool) -> task::Model {
    task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::SoftAbort,
        sign_cache,
        ..Default::default()
    }
}

#[tokio::test]
async fn get_task_includes_sign_cache() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![project()]])
        .append_query_results([vec![task_with(false)]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role_row()]]);

    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/tasks/test-project/test-task")
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(
        body["message"]["sign_cache"], false,
        "GET response must echo task.sign_cache verbatim, got: {body}"
    );
}
