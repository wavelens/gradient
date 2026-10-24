/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::EvaluationId;
use gradient_entity::project_access;
use gradient_fixtures::rows::{eval_at, project, superuser, task, user};
use gradient_test_support::web::{make_test_server, signed_in_as};
use serde_json::json;
use uuid::Uuid;

fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

async fn abort_status_of_private_project_evaluation(
    caller: gradient_entity::user::Model,
) -> StatusCode {
    let (db, auth) = signed_in_as(caller);
    let db = db
        .append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
        .append_query_results([Vec::<project_access::Model>::new()]);

    make_test_server(db.into_connection())
        .post(&format!("/api/v1/evals/{}", evaluation_id()))
        .add_header("authorization", auth)
        .json(&json!({ "method": "abort" }))
        .await
        .status_code()
}

#[tokio::test]
async fn a_superuser_aborts_an_evaluation_outside_their_projects() {
    assert_eq!(
        abort_status_of_private_project_evaluation(superuser()).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_non_member_cannot_abort_an_evaluation_of_a_private_project() {
    assert_eq!(
        abort_status_of_private_project_evaluation(user()).await,
        StatusCode::NOT_FOUND
    );
}
