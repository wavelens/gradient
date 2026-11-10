/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use axum::extract::connect_info::MockConnectInfo;
use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::ids::*;
use gradient_entity::{cache_user, cached_path, cached_path_signature, session};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows::{cache_member_in, live_session};
use gradient_fixtures::shape::shape;
use gradient_fixtures::test_date;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sea_orm::EntityTrait;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::Notify;

const LIB: &str = "dddddddddddddddddddddddddddddddd-lib.drv";
const LIB_OUT: &str = "7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a7a";

struct SignedCache {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    name: String,
    run_signature_sweep_now: Arc<Notify>,
    _secrets: TempDir,
}

async fn signed_cache(managed: bool) -> SignedCache {
    let cache = CacheId::now_v7();
    let db = shape()
        .evaluation(EvaluationId::now_v7())
        .derivation(LIB)
        .output(LIB_OUT)
        .cached_output(LIB)
        .into_cache(cache)
        .into_database()
        .await;

    let secrets = TempDir::new().expect("a secrets directory");
    let crypt_file = secrets.path().join("crypt");
    std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").expect("the secret is written");
    let crypt_file = crypt_file.to_string_lossy().into_owned();
    let (private_key, public_key) =
        gradient_sources::generate_signing_key(&crypt_file).expect("a key pair");
    db.execute(&format!(
        "UPDATE cache SET private_key = '{private_key}', public_key = '{public_key}', \
             public = true, active = true, managed = {managed}"
    ))
    .await;

    let conn = db.connection();
    let cached_path = cached_path::Entity::find()
        .one(&conn)
        .await
        .expect("the cached path reads")
        .expect("a cached path");
    gradient_db::rows::insert::<cached_path_signature::Entity, _>(
        &conn,
        cached_path_signature::Model {
            id: CachedPathSignatureId::now_v7(),
            cached_path: cached_path.id,
            cache,
            signature: Some(vec![0x11; 64]),
            created_at: test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("the signature is written");
    gradient_db::rows::insert::<cache_user::Entity, _>(
        &conn,
        cache_member_in(cache, BASE_CACHE_ROLE_ADMIN_ID),
    )
    .await
    .expect("the cache admin is written");
    let session_id = SessionId::now_v7();
    gradient_db::rows::insert::<session::Entity, _>(&conn, live_session(session_id))
        .await
        .expect("the session is written");

    let state = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .cli(move |cli| cli.secrets.crypt_file = crypt_file)
        .build();
    let run_signature_sweep_now = Arc::clone(&state.wake.run_signature_sweep_now);
    let server = TestServer::new(
        gradient_web::create_router(state)
            .expect("the router builds")
            .layer(MockConnectInfo(
                "127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .expect("an address"),
            )),
    );
    SignedCache {
        db,
        server,
        auth: bearer(session_id),
        name: format!("cache-{}", cache.into_inner().simple()),
        run_signature_sweep_now,
        _secrets: secrets,
    }
}

impl SignedCache {
    async fn signature_lines(&self) -> Vec<String> {
        let res = self
            .server
            .get(&format!("/cache/{}/{LIB_OUT}.narinfo", self.name))
            .await;
        res.assert_status_ok();
        res.text()
            .lines()
            .filter_map(|line| line.strip_prefix("Sig: "))
            .map(str::to_owned)
            .collect()
    }

    async fn rotate(&self) -> axum_test::TestResponse {
        self.server
            .post(&format!("/api/v1/caches/{}/key/rotate", self.name))
            .add_header("authorization", &self.auth)
            .await
    }

    async fn keys(&self) -> Value {
        let res = self
            .server
            .get(&format!("/api/v1/caches/{}/keys", self.name))
            .await;
        res.assert_status_ok();
        res.json::<Value>()["message"].clone()
    }
}

#[tokio::test]
async fn a_rotated_cache_keeps_serving_the_old_signature_until_paths_are_signed_again() {
    let cache = signed_cache(false).await;
    let before = cache.signature_lines().await;
    assert_eq!(before.len(), 1);
    let old_public_key = cache.keys().await["public_key"].clone();

    let res = cache.rotate().await;
    res.assert_status_ok();
    let new_public_key = res.json::<Value>()["message"].clone();
    assert_ne!(new_public_key, old_public_key);
    tokio::time::timeout(
        Duration::from_secs(1),
        cache.run_signature_sweep_now.notified(),
    )
    .await
    .expect("the signature sweep is woken");

    assert_eq!(cache.signature_lines().await, before);
    let keys = cache.keys().await;
    assert_eq!(keys["public_key"], new_public_key);
    assert_eq!(keys["retired"][0]["public_key"], old_public_key);

    cache
        .db
        .execute("UPDATE cached_path_signature SET signature = decode(repeat('22', 64), 'hex')")
        .await;
    let after = cache.signature_lines().await;
    assert_eq!(after.len(), 2);
    assert_eq!(after[1], before[0]);
    cache.db.finish().await;
}

#[tokio::test]
async fn a_managed_cache_refuses_key_rotation() {
    let cache = signed_cache(true).await;
    let old_public_key = cache.keys().await["public_key"].clone();

    cache.rotate().await.assert_status(StatusCode::FORBIDDEN);

    let keys = cache.keys().await;
    assert_eq!(keys["public_key"], old_public_key);
    assert_eq!(keys["retired"], Value::Array(Vec::new()));
    cache.db.finish().await;
}
