/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::header;
use axum_test::TestResponse;
use chrono::{Duration, Utc};
use gradient_entity::ids::SessionId;
use gradient_entity::session;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::user;
use gradient_test_support::web::{bearer, make_test_server, make_token};

fn build_session_that_expires_after(id: SessionId, remaining: Duration) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + remaining,
        last_used_at: now,
        ..Default::default()
    }
}

fn response_sets_jwt_token_cookie(res: &TestResponse) -> bool {
    res.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|cookie| cookie.to_str().is_ok_and(|c| c.starts_with("jwt_token=")))
}

#[tokio::test]
async fn session_cookie_past_half_validity_gets_renewed_jwt_token() {
    let id = SessionId::now_v7();
    let stale = build_session_that_expires_after(id, Duration::hours(1));
    let extended = build_session_that_expires_after(id, Duration::hours(24));
    let db = mock_db()
        .append_query_results([vec![stale.clone()]])
        .append_query_results([vec![stale]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![extended]])
        .into_connection();

    let res = make_test_server(db)
        .get("/api/v1/user")
        .add_header(header::COOKIE, format!("jwt_token={}", make_token(id)))
        .await;

    res.assert_status_ok();
    assert!(response_sets_jwt_token_cookie(&res));
}

#[tokio::test]
async fn bearer_header_session_is_not_renewed_and_no_set_cookie_is_returned() {
    let id = SessionId::now_v7();
    let stale = build_session_that_expires_after(id, Duration::hours(1));
    let extended = build_session_that_expires_after(id, Duration::hours(24));
    let db = mock_db()
        .append_query_results([vec![stale.clone()]])
        .append_query_results([vec![stale]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![extended]])
        .into_connection();

    let res = make_test_server(db)
        .get("/api/v1/user")
        .add_header(header::AUTHORIZATION, bearer(id))
        .await;

    res.assert_status_ok();
    assert!(!response_sets_jwt_token_cookie(&res));
}

#[tokio::test]
async fn bearer_header_session_renew_returns_new_signed_jwt() {
    let id = SessionId::now_v7();
    let fresh = build_session_that_expires_after(id, Duration::hours(23));
    let extended = build_session_that_expires_after(id, Duration::hours(24));
    let db = mock_db()
        .append_query_results([vec![fresh.clone()]])
        .append_query_results([vec![fresh]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![extended]])
        .into_connection();

    let res = make_test_server(db)
        .post("/api/v1/auth/session/renew")
        .add_header(header::AUTHORIZATION, bearer(id))
        .await;

    res.assert_status_ok();
    let token = res.json::<serde_json::Value>()["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_default();
    assert_eq!(token.split('.').count(), 3, "token = {token}");
}

fn build_session_signed_in_ago(
    id: SessionId,
    age: Duration,
    remaining: Duration,
) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        created_at: now - age,
        ..build_session_that_expires_after(id, remaining)
    }
}

#[tokio::test]
async fn session_cookie_capped_at_absolute_end_is_not_renewed_again() {
    let id = SessionId::now_v7();
    let capped = build_session_signed_in_ago(
        id,
        Duration::days(90) - Duration::hours(1),
        Duration::hours(1),
    );
    let db = mock_db()
        .append_query_results([vec![capped.clone()]])
        .append_query_results([vec![capped.clone()]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![capped]])
        .into_connection();

    let res = make_test_server(db)
        .get("/api/v1/user")
        .add_header(header::COOKIE, format!("jwt_token={}", make_token(id)))
        .await;

    res.assert_status_ok();
    assert!(!response_sets_jwt_token_cookie(&res));
}

#[tokio::test]
async fn session_cookie_past_day_90_after_sign_in_is_refused() {
    let id = SessionId::now_v7();
    let too_old = build_session_signed_in_ago(id, Duration::days(91), Duration::hours(23));
    let db = mock_db()
        .append_query_results([vec![too_old.clone()]])
        .append_query_results([vec![too_old]])
        .append_query_results([vec![user()]])
        .into_connection();

    let res = make_test_server(db)
        .get("/api/v1/user")
        .add_header(header::COOKIE, format!("jwt_token={}", make_token(id)))
        .await;

    res.assert_status_unauthorized();
}

#[tokio::test]
async fn recently_renewed_session_cookie_is_not_renewed_again() {
    let id = SessionId::now_v7();
    let fresh = build_session_that_expires_after(id, Duration::hours(23));
    let extended = build_session_that_expires_after(id, Duration::hours(24));
    let db = mock_db()
        .append_query_results([vec![fresh.clone()]])
        .append_query_results([vec![fresh]])
        .append_query_results([vec![user()]])
        .append_query_results([vec![extended]])
        .into_connection();

    let res = make_test_server(db)
        .get("/api/v1/user")
        .add_header(header::COOKIE, format!("jwt_token={}", make_token(id)))
        .await;

    res.assert_status_ok();
    assert!(!response_sets_jwt_token_cookie(&res));
}
