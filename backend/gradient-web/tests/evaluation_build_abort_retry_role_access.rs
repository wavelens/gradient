/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::ids::*;
use gradient_entity::{project_access, role};
use gradient_fixtures::ids::{project_id, task_id};
use gradient_fixtures::rows::{eval_at, project, task, user, view_role, write_role};
use gradient_test_support::web::{make_test_server, signed_in_as};
use sea_orm::MockDatabase;
use serde_json::json;
use uuid::Uuid;

fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::from_u128(0xe1))
}

fn build_job_id() -> BuildJobId {
    BuildJobId::new(Uuid::from_u128(0xb1))
}

fn shared_build_id() -> DerivationBuildId {
    DerivationBuildId::new(Uuid::from_u128(0xa1))
}

fn with_membership(db: MockDatabase, role: &role::Model, lookups: usize) -> MockDatabase {
    (0..lookups).fold(db, |db, _| {
        db.append_query_results([vec![project_access::Model {
            project: project_id(),
            user: user().id,
            role: role.id,
        }]])
        .append_query_results([vec![role.clone()]])
    })
}

fn with_evaluation(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![eval_at(evaluation_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
}

fn with_build(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![gradient_entity::build_job::Model {
        id: build_job_id(),
        evaluation: evaluation_id(),
        derivation_build: shared_build_id(),
        ..Default::default()
    }]])
    .append_query_results([vec![gradient_entity::derivation_build::Model {
        id: shared_build_id(),
        ..Default::default()
    }]])
    .append_query_results([vec![eval_at(evaluation_id(), 0)]])
    .append_query_results([vec![gradient_entity::task::Model {
        id: task_id(),
        project: project_id(),
        ..Default::default()
    }]])
    .append_query_results([vec![project()]])
}

#[tokio::test]
async fn a_view_member_cannot_abort_or_retry_evaluations_or_builds() {
    let evaluation = format!("/api/v1/evals/{}", evaluation_id());
    let build = format!("/api/v1/builds/{}", build_job_id());
    for (url, is_build, body) in [
        (
            evaluation.clone(),
            false,
            Some(json!({ "method": "abort" })),
        ),
        (format!("{evaluation}/retry"), false, None),
        (format!("{build}/abort"), true, None),
        (format!("{build}/retry"), true, None),
    ] {
        let (db, auth) = signed_in_as(user());
        let db = if is_build {
            with_build(db)
        } else {
            with_evaluation(db)
        };
        let db = with_membership(db, &view_role(), 2);

        let request = make_test_server(db.into_connection())
            .post(&url)
            .add_header("authorization", auth);
        let res = match body {
            Some(body) => request.json(&body).await,
            None => request.await,
        };

        assert_eq!(res.status_code(), StatusCode::FORBIDDEN, "POST {url}");
    }
}

#[tokio::test]
async fn a_write_member_aborts_an_evaluation() {
    let (db, auth) = signed_in_as(user());
    let db = with_membership(with_evaluation(db), &write_role(), 2);

    let res = make_test_server(db.into_connection())
        .post(&format!("/api/v1/evals/{}", evaluation_id()))
        .add_header("authorization", auth)
        .json(&json!({ "method": "abort" }))
        .await;

    res.assert_status_ok();
}
