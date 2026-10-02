/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::EvaluationId;
use gradient_test_support::web::make_test_server;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

fn server() -> axum_test::TestServer {
    make_test_server(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
}

fn report_path(query: &str) -> String {
    format!("/api/v1/evals/{}/report?{query}", EvaluationId::now_v7())
}

fn assert_rejected_by_auth(res: axum_test::TestResponse) {
    res.assert_status_forbidden();
    let body: Value = res.json();
    assert_eq!(body["error"], Value::Bool(true));
    assert_eq!(
        body["message"],
        Value::String("Authorization header not found".to_string()),
        "report generation must be refused by the auth middleware, not the handler",
    );
}

#[tokio::test]
async fn anonymous_cannot_generate_a_report() {
    let res = server().get(&report_path("")).await;
    assert_rejected_by_auth(res);
}

/// Opting out of the instance section skipped the only branch requiring a logged-in user.
#[tokio::test]
async fn anonymous_cannot_dodge_the_gate_by_dropping_instance_context() {
    let res = server()
        .get(&report_path("include_instance=false&include_logs=true"))
        .await;
    assert_rejected_by_auth(res);
}

#[tokio::test]
async fn a_bearer_token_is_still_required_when_it_is_unusable() {
    let res = server()
        .get(&report_path("include_instance=false"))
        .add_header("authorization", "Bearer not-a-real-jwt")
        .await;
    res.assert_status_unauthorized();
}
