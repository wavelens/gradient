/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use gradient_entity::{cache, cache_user, project_cache};
use gradient_fixtures::rows::{cache, cache_admin_role, cache_member, cache_view_role};
use gradient_test_support::cache_fixture::{FIXTURE_CACHE_NAME, public_cache_empty_nars};
use gradient_test_support::state::TestState;
use gradient_test_support::web::signed_in;
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_CACHE_ROLE_VIEW_ID};
use gradient_web::create_router;
use sea_orm::{DatabaseConnection, MockDatabase};
use std::sync::Arc;

fn mock_database_and_auth_header_for_signed_in_cache_admin() -> (DatabaseConnection, String) {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![cache()]])
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![cache_admin_role()]]);
    (db.into_connection(), auth)
}

#[tokio::test]
async fn chunk_upload_rejects_too_long_or_forbidden_store_path_name_with_bad_request_and_no_staging_file()
 {
    let hash = "a".repeat(32);
    for segment in [
        format!("{hash}-{}", "n".repeat(212)),
        format!("{hash}-hello~world"),
    ] {
        let upload_server_base_temp_dir = tempfile::TempDir::new().unwrap();
        let base_dir = upload_server_base_temp_dir.path().to_path_buf();
        let (db, auth) = mock_database_and_auth_header_for_signed_in_cache_admin();
        let server = TestState::new()
            .web_db(db)
            .config(move |config| config.server.base_dir = base_dir)
            .server();

        let response = server
            .put(&format!(
                "/api/v1/caches/{}/nars/{segment}/chunk?offset=0",
                cache().name
            ))
            .add_header("authorization", auth)
            .bytes(vec![1u8, 2, 3].into())
            .await;

        response.assert_status(StatusCode::BAD_REQUEST);
        let partial_dir = upload_server_base_temp_dir
            .path()
            .join("nar-upload-partial");
        assert!(
            std::fs::read_dir(&partial_dir)
                .map(|entries| entries.count() == 0)
                .unwrap_or(true),
            "{segment}"
        );
    }
}

#[tokio::test]
async fn chunk_upload_with_longest_valid_store_path_name_succeeds_and_uses_same_partial_file_name()
{
    let hash = "a".repeat(32);
    let longest_valid_chunk_url_path_segment = format!("{hash}-{}", "n".repeat(211));
    let mut partial_upload_staging_file_names = Vec::new();
    for segment in [hash.clone(), longest_valid_chunk_url_path_segment] {
        let upload_server_base_temp_dir = tempfile::TempDir::new().unwrap();
        let base_dir = upload_server_base_temp_dir.path().to_path_buf();
        let (db, auth) = mock_database_and_auth_header_for_signed_in_cache_admin();
        let server = TestState::new()
            .web_db(db)
            .config(move |config| config.server.base_dir = base_dir)
            .server();

        let response = server
            .put(&format!(
                "/api/v1/caches/{}/nars/{segment}/chunk?offset=0",
                cache().name
            ))
            .add_header("authorization", auth)
            .bytes(vec![1u8, 2, 3].into())
            .await;

        response.assert_status_ok();
        let partial_dir = upload_server_base_temp_dir
            .path()
            .join("nar-upload-partial");
        partial_upload_staging_file_names.push(
            std::fs::read_dir(&partial_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| name.ends_with(".partial"))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(partial_upload_staging_file_names[0].len(), 1);
    assert_eq!(
        partial_upload_staging_file_names[0],
        partial_upload_staging_file_names[1]
    );
}

const DIRECT_UPLOAD_ROUTES: [&str; 3] = ["nars", "chunk", "finalize"];

fn inactive_cache_row() -> cache::Model {
    cache::Model {
        active: false,
        ..cache()
    }
}

async fn send_direct_nar_upload_request(
    db: MockDatabase,
    auth: String,
    route: &str,
) -> (StatusCode, serde_json::Value) {
    let server = TestState::new().web_db(db.into_connection()).server();
    let cache_path = format!("/api/v1/caches/{}/nars", cache().name);
    let hash = "a".repeat(32);
    let request = match route {
        "nars" => server.post(&cache_path).multipart(
            MultipartForm::new()
                .add_part("narinfo", Part::text("{}"))
                .add_part("nar", Part::bytes(vec![1u8, 2, 3])),
        ),
        "chunk" => server
            .put(&format!("{cache_path}/{hash}/chunk?offset=0"))
            .bytes(vec![1u8, 2, 3].into()),
        _ => server
            .post(&format!("{cache_path}/{hash}/finalize"))
            .json(&serde_json::json!({
                "store_path": format!("/nix/store/{hash}-hello"),
                "file_hash": format!("sha256:{}", "b".repeat(52)),
                "file_size": 3,
                "nar_size": 3,
                "nar_hash": format!("sha256:{}", "c".repeat(52)),
            })),
    };
    let response = request.add_header("authorization", auth).await;
    let body = serde_json::from_slice(response.as_bytes()).unwrap_or_default();
    (response.status_code(), body)
}

#[tokio::test]
async fn write_store_member_gets_cache_disabled_on_direct_upload_routes_when_cache_off() {
    let mut answers = Vec::new();
    for route in DIRECT_UPLOAD_ROUTES {
        let (db, auth) = signed_in();
        let db = db
            .append_query_results([vec![inactive_cache_row()]])
            .append_query_results([vec![cache_member(BASE_CACHE_ROLE_ADMIN_ID)]])
            .append_query_results([vec![cache_admin_role()]]);

        let (status, body) = send_direct_nar_upload_request(db, auth, route).await;
        answers.push((route, status, body["message"].clone()));
    }

    let expected = DIRECT_UPLOAD_ROUTES
        .map(|route| {
            (
                route,
                StatusCode::BAD_REQUEST,
                serde_json::Value::from("Cache is disabled"),
            )
        })
        .to_vec();
    assert_eq!(answers, expected);
}

#[tokio::test]
async fn disabled_cache_direct_upload_routes_keep_404_for_non_members_and_403_for_members_without_write_store_permission()
 {
    for route in DIRECT_UPLOAD_ROUTES {
        let (db, auth) = signed_in();
        let db = db
            .append_query_results([vec![inactive_cache_row()]])
            .append_query_results([Vec::<cache_user::Model>::new()])
            .append_query_results([Vec::<project_cache::Model>::new()]);
        let (status, body) = send_direct_nar_upload_request(db, auth, route).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}: {body}");

        let (db, auth) = signed_in();
        let db = db
            .append_query_results([vec![inactive_cache_row()]])
            .append_query_results([vec![cache_member(BASE_CACHE_ROLE_VIEW_ID)]])
            .append_query_results([vec![cache_view_role()]]);
        let (status, body) = send_direct_nar_upload_request(db, auth, route).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}: {body}");
    }
}

#[tokio::test]
async fn cache_admin_can_reactivate_disabled_cache() {
    let (db, auth) = signed_in();
    let db = db
        .append_query_results([vec![inactive_cache_row()]])
        .append_query_results([vec![cache_member(BASE_CACHE_ROLE_ADMIN_ID)]])
        .append_query_results([vec![cache_admin_role()]])
        .append_query_results([vec![cache()]]);
    let server = TestState::new().web_db(db.into_connection()).server();

    let response = server
        .post(&format!("/api/v1/caches/{}/active", cache().name))
        .add_header("authorization", auth)
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn upload_unauthenticated_returns_403() {
    let state = public_cache_empty_nars().await;
    let server = TestServer::new(create_router(Arc::clone(&state)).expect("router"));
    let form = MultipartForm::new()
        .add_part("narinfo", Part::text("{}"))
        .add_part("nar", Part::bytes(vec![1u8, 2, 3]));
    let resp = server
        .post(&format!("/api/v1/caches/{FIXTURE_CACHE_NAME}/nars"))
        .multipart(form)
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
}
