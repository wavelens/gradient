/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{evaluation, team_project};
use gradient_fixtures::ids::project_id;
use gradient_fixtures::rows::{team, team_member};
use gradient_test_support::web::{make_test_server, signed_in};
use gradient_types::{EvalCapability, WaitingReason};
use serde_json::Value;

const DEMAND_URL: &str = "/api/v1/teams/platform/workers/demand";

#[tokio::test]
async fn team_member_cannot_read_demand() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Member)]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(DEMAND_URL)
        .add_header("authorization", auth)
        .await;

    res.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_reads_demand() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![team()]])
        .append_query_results([vec![team_member(TeamRole::Admin)]])
        .append_query_results([vec![team_project::Model {
            project: project_id(),
            includes_workers: true,
            ..Default::default()
        }]])
        .append_query_results([vec![evaluation::Model {
            status: EvaluationStatus::Waiting,
            waiting_reason: Some(WaitingReason::eval_workers(EvalCapability::Eval, 0).to_json()),
            ..Default::default()
        }]]);
    let server = make_test_server(db.into_connection());

    let res = server
        .get(DEMAND_URL)
        .add_header("authorization", auth)
        .await;

    res.assert_status_ok();
    let body: Value = res.json();
    assert_eq!(body["message"]["eval"], 1);
    assert_eq!(body["message"]["network"], 0);
    assert_eq!(body["message"]["secrets"], 0);
    assert_eq!(body["message"]["builds"], serde_json::json!([]));
}
