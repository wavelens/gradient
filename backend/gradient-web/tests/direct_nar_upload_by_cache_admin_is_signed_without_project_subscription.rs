/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::extract::connect_info::MockConnectInfo;
use axum::http::StatusCode;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use gradient_entity::ids::*;
use gradient_entity::{cache_user, session};
use gradient_fixtures::rows::{cache_member_in, live_session};
use gradient_fixtures::shape::shape;
use gradient_graph::Graph;
use gradient_storage::NarStore;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use sha2::Digest as _;
use tempfile::TempDir;

const UPLOADED_STORE_PATH_HASH: &str = "1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f";

#[tokio::test]
async fn direct_nar_upload_by_cache_admin_is_signed_without_project_subscription() {
    let cache = CacheId::now_v7();
    let db = shape()
        .evaluation(EvaluationId::now_v7())
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
             public = true, active = true; \
         DELETE FROM project_cache;"
    ))
    .await;
    let conn = db.connection();
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

    let nar_dir = TempDir::new().expect("a NAR directory");
    let base_dir = TempDir::new().expect("a staging directory");
    let base_dir_path = base_dir.path().to_path_buf();
    let graph = Graph::new();
    let state = TestState::new()
        .web_db(conn.clone())
        .worker_db(conn.clone())
        .cache_db(conn)
        .nar_storage(NarStore::local(nar_dir.path()).expect("a local NAR store"))
        .graph(Arc::clone(&graph))
        .cli(move |cli| cli.secrets.crypt_file = crypt_file)
        .config(move |config| config.server.base_dir = base_dir_path)
        .build();
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    let server = TestServer::new(
        gradient_web::create_router(state)
            .expect("the router builds")
            .layer(MockConnectInfo(
                "127.0.0.1:0"
                    .parse::<std::net::SocketAddr>()
                    .expect("an address"),
            )),
    );
    let cache_name = format!("cache-{}", cache.into_inner().simple());

    let nar = b"nar of a direct upload".to_vec();
    let compressed = zstd::encode_all(nar.as_slice(), 3).expect("the NAR compresses");
    let narinfo = serde_json::json!({
        "store_path": format!("/nix/store/{UPLOADED_STORE_PATH_HASH}-direct"),
        "file_hash": gradient_storage::file_hash_sri(&compressed),
        "file_size": compressed.len(),
        "nar_size": nar.len(),
        "nar_hash": format!("sha256:{}", hex::encode(sha2::Sha256::digest(&nar))),
        "references": [],
    });
    server
        .post(&format!("/api/v1/caches/{cache_name}/nars"))
        .add_header("authorization", bearer(session_id))
        .multipart(
            MultipartForm::new()
                .add_part("narinfo", Part::text(narinfo.to_string()))
                .add_part("nar", Part::bytes(compressed)),
        )
        .await
        .assert_status(StatusCode::CREATED);

    let signatures = db.signatures(UPLOADED_STORE_PATH_HASH).await;
    let served = server
        .get(&format!(
            "/cache/{cache_name}/{UPLOADED_STORE_PATH_HASH}.narinfo"
        ))
        .await;
    let served_signature_lines = served
        .text()
        .lines()
        .filter(|line| line.starts_with("Sig: "))
        .count();
    assert_eq!(
        (
            signatures
                .iter()
                .filter(|row| row.cache == cache && row.signature.is_some())
                .count(),
            served.status_code(),
            served_signature_lines,
        ),
        (1, StatusCode::OK, 1),
        "Direct uploads to a cache with a private signing key and no project subscription for output signing get a signature for that cache. The cache can serve each upload with that signature."
    );
    drop(server);
    db.finish().await;
}
