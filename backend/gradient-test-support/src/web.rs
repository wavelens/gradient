/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::ids::{SessionId, UserId};
use gradient_entity::user;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::mock_db;
use gradient_fixtures::rows::{live_session, user};
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseConnection, MockDatabase};
use serde::Serialize;

use crate::state::{TEST_JWT_SECRET, TestState};

#[derive(Serialize)]
struct Claims {
    exp: usize,
    iat: usize,
    id: UserId,
    jti: SessionId,
}

pub fn make_token(session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp() as usize,
        exp: (now + Duration::hours(1)).timestamp() as usize,
        id: user_id(),
        jti: session_id,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("sign jwt")
}

pub fn bearer(session_id: SessionId) -> String {
    format!("Bearer {}", make_token(session_id))
}

pub fn with_auth_as(db: MockDatabase, session_id: SessionId, actor: user::Model) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![actor]])
}

pub fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    with_auth_as(db, session_id, user())
}

pub fn signed_in_as(actor: user::Model) -> (MockDatabase, String) {
    let session_id = SessionId::now_v7();
    (
        with_auth_as(mock_db(), session_id, actor),
        bearer(session_id),
    )
}

pub fn signed_in() -> (MockDatabase, String) {
    signed_in_as(user())
}

pub fn make_test_server(web_db: DatabaseConnection) -> TestServer {
    TestState::new().web_db(web_db).server()
}
