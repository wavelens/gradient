/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum_test::TestServer;
use chrono::{Duration, Utc};
use gradient_entity::api;
use gradient_entity::ids::{ApiId, ProjectId, SessionId, UserId};
use gradient_entity::user;
use gradient_fixtures::ids::user_id;
use gradient_fixtures::mock::{exec_ok, mock_db};
use gradient_fixtures::rows::{live_session, user};
use gradient_types::permissions::PermissionMask;
use gradient_web::authorization::hash_api_key;
use jsonwebtoken::{EncodingKey, Header, encode};
use sea_orm::{DatabaseConnection, MockDatabase};
use serde::Serialize;

use crate::state::{TEST_JWT_SECRET, TestState};

#[derive(Serialize)]
struct Claims {
    exp: i64,
    iat: i64,
    id: UserId,
    jti: SessionId,
}

pub fn make_token(session_id: SessionId) -> String {
    token_for_user(user_id(), session_id)
}

fn token_for_user(user: UserId, session_id: SessionId) -> String {
    let now = Utc::now();
    let claims = Claims {
        iat: now.timestamp(),
        exp: (now + Duration::hours(1)).timestamp(),
        id: user,
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

pub fn bearer_for_user(user: UserId, session_id: SessionId) -> String {
    format!("Bearer {}", token_for_user(user, session_id))
}

pub fn with_auth_as(db: MockDatabase, session_id: SessionId, actor: user::Model) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![actor]])
}

pub fn mock_db_and_auth_header_for_api_key_owned_by_user(
    actor: user::Model,
    permission: PermissionMask,
    project: Option<ProjectId>,
) -> (MockDatabase, String) {
    let raw = "test-key";
    let now = Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: actor.id,
        name: "key".into(),
        key: hash_api_key(raw),
        last_used_at: now,
        created_at: now,
        permission,
        project,
        ..Default::default()
    };
    let db = mock_db()
        .append_query_results([vec![key.clone()]])
        .append_exec_results([exec_ok()])
        .append_query_results([vec![key]])
        .append_query_results([vec![actor]]);
    (db, format!("Bearer GRAD{raw}"))
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

pub fn spawn_loopback_http_server_with_connection_counter(
    body: &'static str,
) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base_url, connections)
}
