/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use axum_test::multipart::{MultipartForm, Part};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::store_path_content_producer::StoredContentVariantProducerKind;
use gradient_entity::{api, cache_user, session, store_path_content_producer, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_fixtures::shape::shape;
use gradient_graph::Graph;
use gradient_storage::NarStore;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer_for_user;
use gradient_types::SessionId;
use gradient_types::consts::BASE_CACHE_ROLE_ADMIN_ID;
use gradient_types::ids::*;
use gradient_types::permissions::{CachePermission, cache_mask_from};
use gradient_web::authorization::hash_api_key;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use sha2::Digest as _;
use tempfile::TempDir;

const DEPENDENCY_DERIVATION_STORE_PATH: &str = "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b-dependency.drv";
const DEPENDENCY_OUTPUT_STORE_PATH_HASH: &str = "1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c";
const PARENT_BUILD_DERIVATION_PATH: &str = "1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a-parent.drv";
const PARENT_BUILD_OUTPUT_STORE_PATH_HASH: &str = "1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d";

struct NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup {
    db: TestDatabase,
    server: TestServer,
    uploader: user::Model,
    session_id: SessionId,
    cache: CacheId,
    _nar_dir: TempDir,
    _base_dir: TempDir,
}

async fn seed_waiting_parent_build_on_completed_dependency_with_cache_admin_and_start_test_server()
-> NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup {
    let (evaluation, cache) = (EvaluationId::now_v7(), CacheId::now_v7());
    let db = shape()
        .evaluation(evaluation)
        .evaluation_status(EvaluationStatus::Building)
        .into_cache(cache)
        .derivation(PARENT_BUILD_DERIVATION_PATH)
        .walked()
        .entry_point("pkg.parent")
        .job()
        .probed()
        .drv_cached()
        .output(PARENT_BUILD_OUTPUT_STORE_PATH_HASH)
        .depends_on(DEPENDENCY_DERIVATION_STORE_PATH)
        .derivation(DEPENDENCY_DERIVATION_STORE_PATH)
        .walked()
        .job()
        .drv_cached()
        .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
        .build_status(BuildStatus::Completed)
        .into_database()
        .await;
    let uploader = user::Model {
        id: UserId::now_v7(),
        username: "uploader".into(),
        name: "uploader".into(),
        email: "uploader@example.com".into(),
        ..rows::user()
    }
    .into_active_model()
    .insert(&db.connection())
    .await
    .expect("the user is written");
    let session_id = SessionId::now_v7();
    session::Model {
        user_id: uploader.id,
        ..rows::live_session(session_id)
    }
    .into_active_model()
    .insert(&db.connection())
    .await
    .expect("the session is written");
    cache_user::Model {
        id: CacheUserId::now_v7(),
        cache,
        user: uploader.id,
        role: BASE_CACHE_ROLE_ADMIN_ID,
    }
    .into_active_model()
    .insert(&db.connection())
    .await
    .expect("the cache membership is written");

    let nar_dir = TempDir::new().expect("a NAR directory");
    let base_dir = TempDir::new().expect("a staging directory");
    let base_dir_path = base_dir.path().to_path_buf();
    let graph = Graph::new();
    let state = TestState::new()
        .web_db(db.connection())
        .worker_db(db.connection())
        .cache_db(db.connection())
        .nar_storage(NarStore::local(nar_dir.path()).expect("a local NAR store"))
        .graph(Arc::clone(&graph))
        .config(move |config| config.server.base_dir = base_dir_path)
        .build();
    gradient_db::graph::consistency::graph_consistency_report(&state.db())
        .await
        .expect("the seed settles");
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    let server = TestServer::new(gradient_web::create_router(state).expect("the router builds"));
    NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup {
        db,
        server,
        uploader,
        session_id,
        cache,
        _nar_dir: nar_dir,
        _base_dir: base_dir,
    }
}

async fn upload_compressed_dependency_output_nar_with_authorization_and_check_created_status(
    setup: &NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup,
    authorization: String,
) {
    let nar = b"nar of the dependency output".to_vec();
    let compressed = zstd::encode_all(nar.as_slice(), 3).expect("the NAR compresses");
    let narinfo = serde_json::json!({
        "store_path": format!("/nix/store/{DEPENDENCY_OUTPUT_STORE_PATH_HASH}-dependency"),
        "file_hash": gradient_storage::file_hash_sri(&compressed),
        "file_size": compressed.len(),
        "nar_size": nar.len(),
        "nar_hash": format!("sha256:{}", hex::encode(sha2::Sha256::digest(&nar))),
        "references": [],
    });
    let response = setup
        .server
        .post(&format!(
            "/api/v1/caches/cache-{}/nars",
            setup.cache.into_inner().simple()
        ))
        .add_header("authorization", authorization)
        .multipart(
            MultipartForm::new()
                .add_part("narinfo", Part::text(narinfo.to_string()))
                .add_part("nar", Part::bytes(compressed)),
        )
        .await;
    response.assert_status(StatusCode::CREATED);
}

async fn read_user_and_api_key_of_upload_producer_row_for_stored_copy(
    setup: &NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup,
) -> (Option<UserId>, Option<ApiId>) {
    let producers = setup.db.rows::<store_path_content_producer::Entity>().await;
    let uploads: Vec<_> = producers
        .iter()
        .filter(|p| p.kind == StoredContentVariantProducerKind::UploadedByUserOrApiKey)
        .collect();
    assert_eq!(uploads.len(), 1, "{producers:?}");
    (uploads[0].uploaded_by_user, uploads[0].uploaded_by_api_key)
}

#[tokio::test]
async fn completed_dependency_output_upload_queues_waiting_parent_build_in_same_request() {
    let setup =
        seed_waiting_parent_build_on_completed_dependency_with_cache_admin_and_start_test_server()
            .await;
    let db = &setup.db;
    let parent = db.shared_build(PARENT_BUILD_DERIVATION_PATH).await;
    assert_eq!(
        (parent.status, parent.blocking_deps),
        (BuildStatus::Created, 1),
        "before the upload: parent status, blocking_deps"
    );

    upload_compressed_dependency_output_nar_with_authorization_and_check_created_status(
        &setup,
        bearer_for_user(setup.uploader.id, setup.session_id),
    )
    .await;

    let dependency = db.shared_build(DEPENDENCY_DERIVATION_STORE_PATH).await;
    assert_eq!(
        (dependency.status, dependency.fetchable),
        (BuildStatus::Completed, true),
        "after the upload: dependency status, fetchable"
    );
    let parent = db.shared_build(PARENT_BUILD_DERIVATION_PATH).await;
    assert_eq!(
        (parent.status, parent.blocking_deps),
        (BuildStatus::Queued, 0),
        "after the upload: parent status, blocking_deps"
    );
    let NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup { db, server, .. } = setup;
    drop(server);
    db.finish().await;
}

#[tokio::test]
async fn session_upload_records_signed_in_user_as_uploader_and_no_api_key() {
    let setup =
        seed_waiting_parent_build_on_completed_dependency_with_cache_admin_and_start_test_server()
            .await;

    upload_compressed_dependency_output_nar_with_authorization_and_check_created_status(
        &setup,
        bearer_for_user(setup.uploader.id, setup.session_id),
    )
    .await;

    assert_eq!(
        read_user_and_api_key_of_upload_producer_row_for_stored_copy(&setup).await,
        (Some(setup.uploader.id), None),
        "uploaded_by_user, uploaded_by_api_key"
    );
    let NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup { db, server, .. } = setup;
    drop(server);
    db.finish().await;
}

#[tokio::test]
async fn api_key_upload_records_key_and_owning_user_as_uploader() {
    let setup =
        seed_waiting_parent_build_on_completed_dependency_with_cache_admin_and_start_test_server()
            .await;
    let raw = "k".repeat(64);
    let now = chrono::Utc::now().naive_utc();
    let key = api::Model {
        id: ApiId::now_v7(),
        owned_by: setup.uploader.id,
        name: "uploader-key".into(),
        key: hash_api_key(&raw),
        last_used_at: now,
        created_at: now,
        cache_permission: cache_mask_from(&[
            CachePermission::ViewCache,
            CachePermission::ReadStore,
            CachePermission::WriteStore,
        ]),
        ..Default::default()
    }
    .into_active_model()
    .insert(&setup.db.connection())
    .await
    .expect("The API key is written");

    upload_compressed_dependency_output_nar_with_authorization_and_check_created_status(
        &setup,
        format!("Bearer GRAD{raw}"),
    )
    .await;

    assert_eq!(
        read_user_and_api_key_of_upload_producer_row_for_stored_copy(&setup).await,
        (Some(setup.uploader.id), Some(key.id)),
        "uploaded_by_user, uploaded_by_api_key"
    );
    let NarUploadTestDatabaseServerUserSessionCacheAndDirectoriesSetup { db, server, .. } = setup;
    drop(server);
    db.finish().await;
}
