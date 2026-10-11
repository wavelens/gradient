/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::unwrap_used,
    reason = "test scaffolding: a fixture helper that cannot build its value should fail the test loudly"
)]

use gradient_entity::ids::*;
use gradient_fixtures::ids::commit_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{eval_at, project, project_member, public_project, task, user};
use gradient_test_support::web::{make_test_server, signed_in, signed_in_as};
use gradient_types::consts::BASE_ROLE_VIEW_ID;
use serde_json::Value;
use uuid::Uuid;

fn commit_url() -> String {
    format!("/api/v1/commits/{}", commit_id())
}

fn other_project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-0000000000a0").unwrap())
}

fn eval_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000050").unwrap())
}

fn commit_row() -> gradient_entity::commit::Model {
    gradient_entity::commit::Model {
        id: commit_id(),
        message: "feat: something".into(),
        hash: vec![0xab; 20],
        author_name: "Tester".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn anon_can_read_commit_in_public_project() {
    let db = mock_db()
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![public_project()]]);
    let server = make_test_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["id"], commit_id().to_string());
}

#[tokio::test]
async fn anon_cannot_read_commit_in_private_project() {
    let db = mock_db()
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]]);
    let server = make_test_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn member_can_read_commit_in_private_project() {
    let (db, auth) = signed_in();

    let db = db
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&commit_url())
        .add_header("authorization", auth)
        .await;
    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["error"], false);
    assert_eq!(body["message"]["id"], commit_id().to_string());
}

#[tokio::test]
async fn deactivated_member_cannot_read_commit_in_private_project() {
    let deactivated = gradient_entity::user::Model {
        active: false,
        ..user()
    };

    let (db, auth) = signed_in_as(deactivated);
    let db = db
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![task()]])
        .append_query_results([vec![project()]])
        .append_query_results([vec![project_member(BASE_ROLE_VIEW_ID)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&commit_url())
        .add_header("authorization", auth)
        .await;
    res.assert_status_not_found();
}

#[tokio::test]
async fn non_member_cannot_read_commit() {
    let (db, auth) = signed_in();

    let foreign_task = gradient_entity::task::Model {
        project: other_project_id(),
        ..task()
    };
    let foreign_project = gradient_entity::project::Model {
        id: other_project_id(),
        ..project()
    };

    let db = db
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![eval_at(eval_id(), 0)]])
        .append_query_results([vec![foreign_task]])
        .append_query_results([vec![foreign_project]])
        .append_query_results([Vec::<gradient_entity::project_user::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(&commit_url())
        .add_header("authorization", auth)
        .await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn commit_referenced_only_via_orphan_eval_returns_404() {
    let direct_eval = gradient_entity::evaluation::Model {
        task: None,
        ..eval_at(eval_id(), 0)
    };

    let db = mock_db()
        .append_query_results([vec![commit_row()]])
        .append_query_results([vec![direct_eval]]);
    let server = make_test_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn nonexistent_commit_returns_404() {
    let db = mock_db().append_query_results([Vec::<gradient_entity::commit::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}

#[tokio::test]
async fn commit_without_evaluation_returns_404() {
    let db = mock_db()
        .append_query_results([vec![commit_row()]])
        .append_query_results([Vec::<gradient_entity::evaluation::Model>::new()]);
    let server = make_test_server(db.into_connection());

    let res = server.get(&commit_url()).await;
    res.assert_status_not_found();
    let body: Value = res.json();
    assert_eq!(body["error"], true);
}
