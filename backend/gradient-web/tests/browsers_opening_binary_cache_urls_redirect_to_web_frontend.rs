/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use gradient_core::ServerState;
use gradient_test_support::cache_fixture::{
    FIXTURE_CACHE_NAME, FIXTURE_PATH_HASH, private_cache_state, public_cache_state,
    public_cache_with_nar, public_cache_with_narinfo,
};
use gradient_test_support::state::TestState;
use gradient_web::create_router;
use std::pin::Pin;
use std::sync::Arc;

const FIREFOX_PAGE_ACCEPT_HEADER: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES: &str = "*/*";
const TEST_SERVER_FRONTEND_URL: &str = "https://ci.example";
const VALID_NIX_STORE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

type CacheTestFixtureStarterFunction = fn() -> Pin<Box<dyn Future<Output = Arc<ServerState>>>>;

fn build_test_server_with_frontend_url_and_browser_redirect_flag(enabled: bool) -> TestServer {
    TestState::new()
        .config(move |config| {
            config.server.frontend_url = TEST_SERVER_FRONTEND_URL.into();
            config
                .server
                .frontend_redirect_browser_requests_to_frontend_pages = enabled;
        })
        .server()
}

async fn start_test_server_with_frontend_url_and_browser_redirect_setting(
    fixture: impl Future<Output = Arc<ServerState>>,
    enabled: bool,
) -> TestServer {
    let mut state = fixture.await;
    let config = Arc::make_mut(
        &mut Arc::get_mut(&mut state)
            .expect("fixture state has a single owner")
            .config,
    );
    config.server.frontend_url = TEST_SERVER_FRONTEND_URL.into();
    config
        .server
        .frontend_redirect_browser_requests_to_frontend_pages = enabled;
    TestServer::new(create_router(state).expect("router"))
}

async fn send_get_request_with_accept_header(
    server: &TestServer,
    path: &str,
    accept: &str,
) -> TestResponse {
    server.get(path).add_header("accept", accept).await
}

fn assert_302_redirect_to_location_with_no_store_and_vary_accept(
    response: &TestResponse,
    location: &str,
) {
    response.assert_status(StatusCode::FOUND);
    response.assert_header("location", location);
    response.assert_header("cache-control", "no-store");
    response.assert_header("vary", "Accept");
}

fn binary_cache_urls_with_expected_frontend_redirects() -> Vec<(String, String)> {
    let cache_page = format!("{TEST_SERVER_FRONTEND_URL}/caches/test-cache");
    let store_path_page = format!("{cache_page}/nars?hash={VALID_NIX_STORE_HASH}");
    vec![
        ("/cache/test-cache".into(), cache_page.clone()),
        ("/cache/test-cache/".into(), cache_page.clone()),
        (
            "/cache/test-cache/nix-cache-info".into(),
            cache_page.clone(),
        ),
        ("/cache/test-cache/gradient-cache-info".into(), cache_page),
        (
            format!("/cache/test-cache/{VALID_NIX_STORE_HASH}.narinfo"),
            store_path_page.clone(),
        ),
        (
            format!("/cache/test-cache/ls/{VALID_NIX_STORE_HASH}"),
            store_path_page,
        ),
    ]
}

#[tokio::test]
async fn binary_cache_metadata_narinfo_and_ls_urls_redirect_to_web_frontend_pages() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    for (path, location) in binary_cache_urls_with_expected_frontend_redirects() {
        let response =
            send_get_request_with_accept_header(&server, &path, FIREFOX_PAGE_ACCEPT_HEADER).await;
        assert_302_redirect_to_location_with_no_store_and_vary_accept(&response, &location);
    }
}

#[tokio::test]
async fn browser_request_without_credentials_to_private_binary_cache_redirects_to_web_frontend() {
    let server = start_test_server_with_frontend_url_and_browser_redirect_setting(
        private_cache_state(),
        true,
    )
    .await;
    let response = send_get_request_with_accept_header(
        &server,
        &format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
        FIREFOX_PAGE_ACCEPT_HEADER,
    )
    .await;
    assert_302_redirect_to_location_with_no_store_and_vary_accept(
        &response,
        &format!("{TEST_SERVER_FRONTEND_URL}/caches/{FIXTURE_CACHE_NAME}"),
    );
}

#[tokio::test]
async fn non_browser_unauthenticated_request_to_private_cache_gets_same_refusal_with_redirect_on_and_off()
 {
    let path = format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info");
    let mut responses = Vec::new();
    for enabled in [true, false] {
        let server = start_test_server_with_frontend_url_and_browser_redirect_setting(
            private_cache_state(),
            enabled,
        )
        .await;
        responses.push(
            send_get_request_with_accept_header(
                &server,
                &path,
                ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES,
            )
            .await,
        );
    }
    let [with_redirect, without_redirect] = responses.as_slice() else {
        unreachable!()
    };
    with_redirect.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(with_redirect.status_code(), without_redirect.status_code());
    assert_eq!(with_redirect.text(), without_redirect.text());
    assert_eq!(
        with_redirect.maybe_header("www-authenticate"),
        without_redirect.maybe_header("www-authenticate")
    );
    with_redirect.assert_header("vary", "Accept");
}

#[tokio::test]
async fn nix_and_curl_requests_get_redirect_disabled_status_body_and_location() {
    let enabled = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    let disabled = build_test_server_with_frontend_url_and_browser_redirect_flag(false);
    for (path, _) in binary_cache_urls_with_expected_frontend_redirects() {
        let with_redirect = send_get_request_with_accept_header(
            &enabled,
            &path,
            ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES,
        )
        .await;
        let without_redirect = send_get_request_with_accept_header(
            &disabled,
            &path,
            ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES,
        )
        .await;
        assert_eq!(
            with_redirect.status_code(),
            without_redirect.status_code(),
            "{path}"
        );
        assert_eq!(with_redirect.text(), without_redirect.text(), "{path}");
        assert_eq!(
            with_redirect.maybe_header("location"),
            without_redirect.maybe_header("location"),
            "{path}"
        );
        with_redirect.assert_header("vary", "Accept");
    }
}

#[tokio::test]
async fn nix_opening_cache_root_redirects_to_nix_cache_info() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    let response = send_get_request_with_accept_header(
        &server,
        "/cache/test-cache",
        ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES,
    )
    .await;
    response.assert_status(StatusCode::FOUND);
    response.assert_header("location", "/cache/test-cache/nix-cache-info");
}

#[tokio::test]
async fn html_page_request_without_redirect_setting_matches_curl_response_and_has_no_vary_header() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(false);
    for (path, _) in binary_cache_urls_with_expected_frontend_redirects() {
        let from_browser =
            send_get_request_with_accept_header(&server, &path, FIREFOX_PAGE_ACCEPT_HEADER).await;
        let from_curl = send_get_request_with_accept_header(
            &server,
            &path,
            ACCEPT_HEADER_VALUE_ALL_MEDIA_TYPES,
        )
        .await;
        assert_eq!(
            from_browser.status_code(),
            from_curl.status_code(),
            "{path}"
        );
        assert_eq!(from_browser.text(), from_curl.text(), "{path}");
        assert_eq!(
            from_browser.maybe_header("location"),
            from_curl.maybe_header("location"),
            "{path}"
        );
        assert!(from_browser.maybe_header("vary").is_none(), "{path}");
    }
}

#[tokio::test]
async fn browser_requests_for_nar_downloads_files_build_logs_and_api_v1_are_not_redirected() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    for path in [
        format!("/cache/test-cache/nar/{VALID_NIX_STORE_HASH}.nar"),
        format!("/cache/test-cache/serve/{VALID_NIX_STORE_HASH}/bin/hello"),
        format!("/cache/test-cache/log/{VALID_NIX_STORE_HASH}-hello.drv"),
        "/api/v1/caches/test-cache".to_string(),
    ] {
        let response =
            send_get_request_with_accept_header(&server, &path, FIREFOX_PAGE_ACCEPT_HEADER).await;
        assert_ne!(response.status_code(), StatusCode::FOUND, "{path}");
        assert!(response.maybe_header("location").is_none(), "{path}");
    }
}

#[tokio::test]
async fn test_binary_cache_url_encodes_hostile_name_before_redirect() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    for (path, location) in [
        (
            "/cache/%2F%2Fevil.example/nix-cache-info",
            "https://ci.example/caches/%2F%2Fevil.example",
        ),
        (
            "/cache/x%0D%0ASet-Cookie:%20a=b/nix-cache-info",
            "https://ci.example/caches/x%0D%0ASet-Cookie%3A%20a%3Db",
        ),
        (
            "/cache/@evil.example/ls/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "https://ci.example/caches/%40evil.example/nars?hash=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ] {
        let response =
            send_get_request_with_accept_header(&server, path, FIREFOX_PAGE_ACCEPT_HEADER).await;
        assert_302_redirect_to_location_with_no_store_and_vary_accept(&response, location);
        assert!(response.maybe_header("set-cookie").is_none(), "{path}");
    }
}

#[tokio::test]
async fn narinfo_and_ls_urls_with_invalid_store_hash_are_not_redirected() {
    let server = build_test_server_with_frontend_url_and_browser_redirect_flag(true);
    for path in [
        "/cache/test-cache/%2F%2Fevil.example.narinfo",
        "/cache/test-cache/not-a-store-hash.narinfo",
        "/cache/test-cache/ls/%2F%2Fevil.example",
    ] {
        let response =
            send_get_request_with_accept_header(&server, path, FIREFOX_PAGE_ACCEPT_HEADER).await;
        assert_ne!(response.status_code(), StatusCode::FOUND, "{path}");
        assert!(response.maybe_header("location").is_none(), "{path}");
        response.assert_header("vary", "Accept");
    }
}

#[tokio::test]
async fn non_browser_requests_ignore_web_frontend_redirect_setting() {
    let fixtures: [(String, CacheTestFixtureStarterFunction); 3] = [
        (
            format!("/cache/{FIXTURE_CACHE_NAME}/nix-cache-info"),
            || Box::pin(public_cache_state()),
        ),
        (
            format!("/cache/{FIXTURE_CACHE_NAME}/{FIXTURE_PATH_HASH}.narinfo"),
            || Box::pin(public_cache_with_narinfo()),
        ),
        (
            format!("/cache/{FIXTURE_CACHE_NAME}/ls/{FIXTURE_PATH_HASH}"),
            || Box::pin(public_cache_with_nar()),
        ),
    ];
    for (path, fixture) in fixtures {
        let mut responses = Vec::new();
        for enabled in [true, false] {
            let server = start_test_server_with_frontend_url_and_browser_redirect_setting(
                fixture(),
                enabled,
            )
            .await;
            responses.push(server.get(&path).await);
        }
        let [with_redirect, without_redirect] = responses.as_slice() else {
            unreachable!()
        };
        with_redirect.assert_status_ok();
        assert_eq!(
            with_redirect.status_code(),
            without_redirect.status_code(),
            "{path}"
        );
        assert_eq!(
            with_redirect.as_bytes(),
            without_redirect.as_bytes(),
            "{path}"
        );
        let [mut headers_with_redirect, mut headers_without_redirect] =
            [with_redirect, without_redirect].map(|response| response.headers().clone());
        for headers in [&mut headers_with_redirect, &mut headers_without_redirect] {
            headers.remove("x-request-id");
        }
        assert_eq!(
            headers_with_redirect
                .remove("vary")
                .as_ref()
                .map(|v| v.as_bytes()),
            Some(&b"Accept"[..]),
            "{path}"
        );
        assert_eq!(headers_with_redirect, headers_without_redirect, "{path}");
    }
}
