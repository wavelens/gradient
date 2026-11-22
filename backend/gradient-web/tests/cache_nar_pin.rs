/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::{cache, cached_path};
use gradient_fixtures::rows::{cache, cache_admin_role, cache_member};
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, public_cache_empty_nars,
};
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use gradient_web::create_router;
use std::sync::Arc;

const PIN_UNPIN_AND_DELETE_NAR_ROUTES: [&str; 3] = ["pin", "unpin", "delete"];

async fn send_nar_request_for_unknown_hash_with_mock_database_and_get_response(
    cache_row: cache::Model,
    route: &str,
) -> (StatusCode, serde_json::Value) {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![cache_admin_role()]])
        .append_query_results([Vec::<cached_path::Model>::new()]);
    let server = TestState::new().web_db(db.into_connection()).server();
    let nar_path = format!("/api/v1/caches/{}/nars/{}", cache().name, "a".repeat(32));
    let request = match route {
        "pin" => server.post(&format!("{nar_path}/pin")),
        "unpin" => server.delete(&format!("{nar_path}/pin")),
        _ => server.delete(&nar_path),
    };
    let response = request.add_header("authorization", auth).await;
    let body = serde_json::from_slice(response.as_bytes()).unwrap_or_default();
    (response.status_code(), body)
}

#[tokio::test]
async fn cache_member_pin_unpin_delete_nar_get_same_response_when_cache_inactive() {
    let mut active_answers = Vec::new();
    let mut inactive_answers = Vec::new();
    for route in PIN_UNPIN_AND_DELETE_NAR_ROUTES {
        let active =
            send_nar_request_for_unknown_hash_with_mock_database_and_get_response(cache(), route)
                .await;
        let inactive = send_nar_request_for_unknown_hash_with_mock_database_and_get_response(
            cache::Model {
                active: false,
                ..cache()
            },
            route,
        )
        .await;
        active_answers.push((route, active));
        inactive_answers.push((route, inactive));
    }

    assert!(
        active_answers
            .iter()
            .all(|(_, (status, _))| *status == StatusCode::NOT_FOUND),
        "{active_answers:?}"
    );
    assert_eq!(inactive_answers, active_answers);
}

#[tokio::test]
async fn pin_unauthenticated_returns_403() {
    let state = public_cache_empty_nars().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .post(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}/pin?include_referenced_paths=true"
        ))
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unpin_unauthenticated_returns_403() {
    let state = public_cache_empty_nars().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));

    let resp = server
        .delete(&format!(
            "/api/v1/caches/{FIXTURE_CACHE_NAME}/nars/{FIXTURE_PATH_HASH}/pin"
        ))
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
}
