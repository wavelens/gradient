/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Generating a diagnostic report is an authenticated action.
//!
//! `GET /evals/{evaluation}/report` packs the evaluation and every failed
//! build's log into a SQLite file. The route used to sit on the optional-auth
//! router, where `EvalAccessContext::load` waves through any public project, so
//! an anonymous caller could export one. `include_instance` defaults to `true`
//! and costs `ManageWorkers`, which hid the hole behind a 403 until a caller
//! passed `include_instance=false`. The OpenAPI contract has always listed this
//! path under the global `bearerAuth`; these tests hold the code to it.

use gradient_entity::ids::EvaluationId;
use gradient_test_support::web::make_test_server;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;

fn run<F: std::future::Future<Output = ()>>(f: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

fn server() -> axum_test::TestServer {
    make_test_server(MockDatabase::new(DatabaseBackend::Postgres).into_connection())
}

fn report_path(query: &str) -> String {
    format!("/api/v1/evals/{}/report?{query}", EvaluationId::now_v7())
}

/// The middleware refuses before any row is read, so the mock never needs a
/// project: reaching the handler at all would surface as a different status.
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

#[test]
fn anonymous_cannot_generate_a_report() {
    run(async {
        let res = server().get(&report_path("")).await;
        assert_rejected_by_auth(res);
    });
}

/// The bypass that made this reachable: opting out of the instance section
/// skipped the only branch that required a logged-in user.
#[test]
fn anonymous_cannot_dodge_the_gate_by_dropping_instance_context() {
    run(async {
        let res = server()
            .get(&report_path("include_instance=false&include_logs=true"))
            .await;
        assert_rejected_by_auth(res);
    });
}

#[test]
fn a_bearer_token_is_still_required_when_it_is_unusable() {
    run(async {
        let res = server()
            .get(&report_path("include_instance=false"))
            .add_header("authorization", "Bearer not-a-real-jwt")
            .await;
        res.assert_status_unauthorized();
    });
}
