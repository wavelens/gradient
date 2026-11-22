/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::{StatusCode, header};
use axum_test::TestServer;
use gradient_entity::{cache, cache_user, project_cache};
use gradient_fixtures::rows::cache_member;
use gradient_fixtures::rows::cache_view_role;
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use gradient_types::consts::BASE_CACHE_ROLE_VIEW_ID;
use sea_orm::MockDatabase;

fn inactive_cache_row() -> cache::Model {
    cache::Model {
        active: false,
        ..gradient_fixtures::rows::cache()
    }
}

async fn open_proto_websocket(db: MockDatabase, auth: String) -> (StatusCode, serde_json::Value) {
    let router = gradient_web::create_router(TestState::new().web_db(db.into_connection()).build())
        .expect("router");
    let server = TestServer::builder().http_transport().build(router);
    let response = server
        .get(&format!(
            "/cache/{}/proto",
            gradient_fixtures::rows::cache().name
        ))
        .add_header(header::AUTHORIZATION, auth)
        .add_header(header::CONNECTION, "upgrade")
        .add_header(header::UPGRADE, "websocket")
        .add_header(header::SEC_WEBSOCKET_VERSION, "13")
        .add_header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
        .await;
    let body = serde_json::from_slice(response.as_bytes()).unwrap_or_default();
    (response.status_code(), body)
}

fn reader_of(cache_row: cache::Model) -> (MockDatabase, String) {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![cache_row]])
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]]);
    (db, auth)
}

fn non_member_of(cache_row: cache::Model) -> (MockDatabase, String) {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![cache_row]])
        .append_query_results([Vec::<cache_user::Model>::new()])
        .append_query_results([Vec::<project_cache::Model>::new()]);
    (db, auth)
}

#[tokio::test(flavor = "multi_thread")]
async fn reader_of_disabled_cache_gets_400_cache_disabled_on_connect() {
    let (db, auth) = reader_of(inactive_cache_row());

    let (status, body) = open_proto_websocket(db, auth).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "Cache is disabled", "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn caller_without_read_access_gets_404_for_disabled_and_active_cache() {
    for cache_row in [inactive_cache_row(), gradient_fixtures::rows::cache()] {
        let (db, auth) = non_member_of(cache_row);

        let (status, body) = open_proto_websocket(db, auth).await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reader_of_active_cache_gets_websocket_upgrade() {
    let (db, auth) = reader_of(gradient_fixtures::rows::cache());

    let (status, body) = open_proto_websocket(db, auth).await;

    assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS, "{body}");
}
