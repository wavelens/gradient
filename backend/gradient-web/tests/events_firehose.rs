/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_fixtures::rows::superuser;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{make_test_server, signed_in, signed_in_as};
use sea_orm::MockDatabase;

#[tokio::test]
async fn a_non_superuser_is_refused_the_firehose() {
    let (db, auth) = signed_in();
    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/metrics/events")
        .add_header("authorization", auth)
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_superuser_without_an_upgrade_is_told_to_upgrade() {
    let (db, auth) = signed_in_as(superuser());
    let server = make_test_server(db.into_connection());
    let res = server
        .get("/api/v1/metrics/events")
        .add_header("authorization", auth)
        .await;
    assert!(res.status_code().is_client_error(), "{}", res.status_code());
    assert_ne!(res.status_code(), axum::http::StatusCode::FORBIDDEN);
}

fn mock_database_and_superuser_session_cookie_header() -> (MockDatabase, String) {
    let (db, auth) = signed_in_as(superuser());
    let token = auth.strip_prefix("Bearer ").expect("bearer header");
    (db, format!("jwt_token={token}"))
}

async fn get_events_firehose_http_status_with_superuser_cookie_serve_url_origin_and_headers(
    serve_url: &str,
    origin: &str,
    extra_headers: &[(&str, &str)],
) -> axum::http::StatusCode {
    let (db, cookie) = mock_database_and_superuser_session_cookie_header();
    let serve_url = serve_url.to_owned();
    let server = TestState::new()
        .web_db(db.into_connection())
        .config(move |config| config.server.serve_url = serve_url)
        .server();
    let mut request = server
        .get("/api/v1/metrics/events")
        .add_header("cookie", cookie)
        .add_header("origin", origin.to_owned());
    for (name, value) in extra_headers {
        request = request.add_header(name.to_string(), value.to_string());
    }
    request.await.status_code()
}

const WEBSOCKET_UPGRADE_REQUEST_HEADERS: [(&str, &str); 4] = [
    ("upgrade", "websocket"),
    ("connection", "upgrade"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
];

#[tokio::test]
async fn cookie_websocket_upgrade_from_foreign_origin_is_refused_with_403() {
    let status =
        get_events_firehose_http_status_with_superuser_cookie_serve_url_origin_and_headers(
            "http://127.0.0.1:3000",
            "https://evil.example",
            &WEBSOCKET_UPGRADE_REQUEST_HEADERS,
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cookie_request_from_foreign_origin_without_upgrade_header_to_websocket_route_is_refused_with_403()
 {
    let status =
        get_events_firehose_http_status_with_superuser_cookie_serve_url_origin_and_headers(
            "http://127.0.0.1:3000",
            "https://evil.example",
            &[],
        )
        .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn basic_authorization_header_with_cookie_does_not_skip_origin_check() {
    let (db, auth) = signed_in();
    let token = auth.strip_prefix("Bearer ").expect("bearer header");
    let res = make_test_server(db.into_connection())
        .get("/api/v1/board/live")
        .add_header("upgrade", "websocket")
        .add_header("connection", "upgrade")
        .add_header("origin", "https://evil.example")
        .add_header("authorization", "Basic dXNlcjpwYXNz")
        .add_header("cookie", format!("jwt_token={token}"))
        .await;
    res.assert_status(axum::http::StatusCode::FORBIDDEN);
}

const UPGRADE_REQUIRED_WITHOUT_CONNECTION_STATUS: axum::http::StatusCode =
    axum::http::StatusCode::UPGRADE_REQUIRED;

#[tokio::test]
async fn cookie_websocket_upgrade_from_public_url_origin_passes_origin_check_for_all_url_spellings()
{
    for serve_url in [
        "http://127.0.0.1:3000",
        "http://127.0.0.1:3000/",
        "https://CI.example.com:443/gradient",
    ] {
        let origin = if serve_url.starts_with("https") {
            "https://ci.example.com"
        } else {
            "http://127.0.0.1:3000"
        };
        let status =
            get_events_firehose_http_status_with_superuser_cookie_serve_url_origin_and_headers(
                serve_url,
                origin,
                &WEBSOCKET_UPGRADE_REQUEST_HEADERS,
            )
            .await;
        assert_eq!(
            status, UPGRADE_REQUIRED_WITHOUT_CONNECTION_STATUS,
            "{serve_url}"
        );
    }
}

async fn send_firehose_upgrade_request_as_superuser(
    origin: Option<&str>,
) -> axum::http::StatusCode {
    let (db, auth) = signed_in_as(superuser());
    let server = make_test_server(db.into_connection());
    let mut request = server
        .get("/api/v1/metrics/events")
        .add_header("authorization", auth);
    for (name, value) in WEBSOCKET_UPGRADE_REQUEST_HEADERS {
        request = request.add_header(name, value);
    }
    if let Some(origin) = origin {
        request = request.add_header("origin", origin.to_owned());
    }
    request.await.status_code()
}

#[tokio::test]
async fn bearer_token_websocket_upgrade_from_foreign_origin_passes_origin_check() {
    let status = send_firehose_upgrade_request_as_superuser(Some("https://evil.example")).await;
    assert_eq!(status, UPGRADE_REQUIRED_WITHOUT_CONNECTION_STATUS);
}

#[tokio::test]
async fn bearer_websocket_upgrade_without_origin_header_passes_origin_check() {
    let status = send_firehose_upgrade_request_as_superuser(None).await;
    assert_eq!(status, UPGRADE_REQUIRED_WITHOUT_CONNECTION_STATUS);
}
