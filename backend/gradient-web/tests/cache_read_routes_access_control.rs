/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::{HeaderMap, StatusCode};
use base64::Engine;
use bytes::Bytes;
use gradient_entity::{cache, cache_user, project_cache};
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{cache_member, cache_view_role, user};
use gradient_test_support::cache_fixture::{FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH};
use gradient_test_support::web::{make_test_server, make_token};
use gradient_types::consts::BASE_CACHE_ROLE_VIEW_ID;
use gradient_types::ids::SessionId;
use sea_orm::MockDatabase;
use serde_json::json;

fn basic_auth_header_for_nix_token(token: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("nix:{token}"));
    format!("Basic {encoded}")
}

fn inactive_private_cache_row() -> cache::Model {
    cache::Model {
        active: false,
        ..gradient_fixtures::rows::cache()
    }
}

fn signed_in(db: MockDatabase) -> (MockDatabase, String) {
    let session_id = SessionId::now_v7();
    let session = gradient_fixtures::rows::live_session(session_id);
    let db = db
        .append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]]);
    (db, basic_auth_header_for_nix_token(&make_token(session_id)))
}

async fn send_get_request_and_return_status_headers_and_body(
    db: MockDatabase,
    path: &str,
    authorization: Option<&str>,
) -> (StatusCode, HeaderMap, Bytes) {
    let server = make_test_server(db.into_connection());
    let mut request = server.get(path);
    if let Some(authorization) = authorization {
        request = request.add_header("authorization", authorization.to_owned());
    }
    let response = request.await;
    let mut headers = response.headers().clone();
    headers.remove("x-request-id");
    (response.status_code(), headers, response.into_bytes())
}

#[tokio::test]
async fn requests_without_credentials_or_with_invalid_credentials_get_same_response_for_missing_disabled_and_active_private_caches_on_nix_cache_info_and_narinfo()
 {
    let invalid = basic_auth_header_for_nix_token("not-a-token");
    for path in [
        format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        format!("/cache/{FIXTURE_CACHE_NAME}/{FIXTURE_PATH_HASH}.narinfo"),
    ] {
        let mut responses = Vec::new();
        for rows in [
            Vec::<cache::Model>::new(),
            vec![inactive_private_cache_row()],
            vec![gradient_fixtures::rows::cache()],
        ] {
            for authorization in [None, Some(invalid.as_str())] {
                let db = mock_db().append_query_results([rows.clone()]);
                responses.push(
                    send_get_request_and_return_status_headers_and_body(db, &path, authorization)
                        .await,
                );
            }
        }
        let (status, _, body) = &responses[0];
        assert_eq!(*status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(
            !responses[0].1.contains_key("date"),
            "{path}: {:?}",
            responses[0].1
        );
        let body: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            body,
            json!({
                "code": "unauthorized",
                "error": true,
                "message": "Authentication required to access this cache",
            }),
            "{path}"
        );
        for response in &responses[1..] {
            assert_eq!(response, &responses[0], "{path}");
        }
    }
}

#[tokio::test]
async fn request_without_credentials_for_missing_cache_returns_401() {
    let db = mock_db().append_query_results([Vec::<cache::Model>::new()]);
    let (status, _, _) = send_get_request_and_return_status_headers_and_body(
        db,
        "/cache/missing-cache/nix-cache-info",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_request_without_credentials_for_disabled_private_cache_gets_401_authentication_required()
 {
    let db = mock_db().append_query_results([vec![inactive_private_cache_row()]]);
    let (status, _, body) = send_get_request_and_return_status_headers_and_body(
        db,
        &format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body["message"], "Authentication required to access this cache",
        "{body}"
    );
}

#[tokio::test]
async fn signed_in_user_without_read_store_permission_gets_unauthorized_for_disabled_private_cache()
{
    let (db, authorization) =
        signed_in(mock_db().append_query_results([vec![inactive_private_cache_row()]]));
    let db = db
        .append_query_results([Vec::<cache_user::Model>::new()])
        .append_query_results([Vec::<project_cache::Model>::new()]);
    let (status, _, body) = send_get_request_and_return_status_headers_and_body(
        db,
        &format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        Some(&authorization),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["missing_permissions"], json!(["readStore"]), "{body}");
}

#[tokio::test]
async fn request_without_credentials_for_disabled_public_cache_gets_400_cache_is_disabled() {
    let disabled_public_cache = cache::Model {
        public: true,
        ..inactive_private_cache_row()
    };
    let db = mock_db().append_query_results([vec![disabled_public_cache]]);
    let (status, _, body) = send_get_request_and_return_status_headers_and_body(
        db,
        &format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["message"], "Cache is disabled", "{body}");
}

#[tokio::test]
async fn test_signed_in_user_gets_not_found_for_missing_cache_name() {
    let (db, authorization) =
        signed_in(mock_db().append_query_results([Vec::<cache::Model>::new()]));
    let (status, _, _) = send_get_request_and_return_status_headers_and_body(
        db,
        "/cache/missing-cache/nix-cache-info",
        Some(&authorization),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn signed_in_user_with_read_store_permission_gets_400_cache_disabled_for_disabled_private_cache()
 {
    let (db, authorization) =
        signed_in(mock_db().append_query_results([vec![inactive_private_cache_row()]]));
    let db = db
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
        .append_query_results([vec![cache_view_role()]]);
    let (status, _, body) = send_get_request_and_return_status_headers_and_body(
        db,
        &format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        Some(&authorization),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["message"], "Cache is disabled", "{body}");
}
