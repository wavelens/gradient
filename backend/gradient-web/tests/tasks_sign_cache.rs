/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::task;
use gradient_fixtures::rows::{admin_role, project, project_member, task};
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::ConcurrencyPolicy;
use gradient_types::consts::BASE_ROLE_ADMIN_ID;
use serde_json::Value;

fn task_with(sign_cache: bool) -> task::Model {
    task::Model {
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::SoftAbort,
        sign_cache,
        ..task()
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
        .append_query_results([vec![admin_role()]])
        .append_query_results([vec![project_member(BASE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![admin_role()]]);

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
