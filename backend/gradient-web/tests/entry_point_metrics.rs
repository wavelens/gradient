/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::entry_point;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{public_project, task_without_cache_signing};
use gradient_test_support::web::{make_test_server, make_token, with_auth};
use gradient_types::SessionId;

#[tokio::test]
async fn entry_point_metrics_returns_404_for_attribute_path_that_was_never_evaluated() {
    let session_id = SessionId::now_v7();
    let db = with_auth(mock_db(), session_id)
        .append_query_results([vec![public_project()]])
        .append_query_results([vec![task_without_cache_signing()]])
        .append_query_results([Vec::<entry_point::Model>::new()])
        .append_query_results([Vec::<entry_point::Model>::new()])
        .into_connection();
    let server = make_test_server(db);

    let res = server
        .get("/api/v1/tasks/test-project/test-task/entry-point-metrics?eval=packages.missing")
        .add_header(
            "Authorization",
            format!("Bearer {}", make_token(session_id)),
        )
        .await;

    res.assert_status_not_found();
    let body: serde_json::Value = res.json();
    assert!(
        body["message"].as_str().unwrap().contains("Entry point"),
        "unexpected message: {body}"
    );
}
