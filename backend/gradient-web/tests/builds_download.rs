/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests: build-output downloads served from `nar_storage`.
//!
//! Uses `NarStore::local(tmpdir)` pre-populated with a compressed NAR that
//! contains `nix-support/hydra-build-products` and `image.iso`. The handlers
//! must serve the product listing and file bytes without touching the Nix
//! daemon or local filesystem.
//!
//! Uses manual Tokio runtime because `#[tokio::test]` expands to `::gradient_core::…`
//! references that clash with the local `core` crate name in the workspace.

use axum_test::TestServer;
use bytes::Bytes;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_storage::{EmailSender, NarStore};
use gradient_types::ids::*;
use gradient_core::ServerState;
use gradient_db::{WebDb, WorkerDb};
use harmonia_file_nar::archive::test_data::{TestNarEvent, TestNarEvents};
use harmonia_file_nar::archive::write_nar;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use gradient_test_support::fakes::email::InMemoryEmailSender;
use gradient_test_support::log_storage::NoopLogStorage;
use gradient_test_support::prelude::test_cli;
use uuid::Uuid;
use gradient_web::create_router;

// ── Fixture IDs ───────────────────────────────────────────────────────────────

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap())
}
fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000002").unwrap())
}
fn evaluation_id() -> EvaluationId {
    EvaluationId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000003").unwrap())
}
fn derivation_id() -> DerivationId {
    DerivationId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000004").unwrap())
}
fn build_id() -> BuildId {
    BuildId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000005").unwrap())
}
fn drv_output_id() -> DerivationOutputId {
    DerivationOutputId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000006").unwrap())
}
fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000007").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// A 32-char hash used as both the store-path hash component and nar_storage key.
const FIXTURE_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const STORE_PATH: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-pkg";

/// Known file contents for `image.iso`.
const ISO_BYTES: &[u8] = b"fake-iso-content";

/// Build a NAR containing:
///   nix-support/hydra-build-products  → "file iso /nix/store/<hash>-pkg/image.iso\n"
///   image.iso                          → ISO_BYTES
fn build_test_nar() -> Vec<u8> {
    let products_content = format!("file iso {}/image.iso\n", STORE_PATH);
    let products_bytes = Bytes::from(products_content.into_bytes());
    let iso_bytes = Bytes::from_static(ISO_BYTES);

    let events: TestNarEvents = vec![
        TestNarEvent::StartDirectory { name: Bytes::new() },
        TestNarEvent::StartDirectory {
            name: Bytes::from_static(b"nix-support"),
        },
        TestNarEvent::File {
            name: Bytes::from_static(b"hydra-build-products"),
            executable: false,
            size: products_bytes.len() as u64,
            reader: std::io::Cursor::new(products_bytes),
        },
        TestNarEvent::EndDirectory,
        TestNarEvent::File {
            name: Bytes::from_static(b"image.iso"),
            executable: false,
            size: iso_bytes.len() as u64,
            reader: std::io::Cursor::new(iso_bytes),
        },
        TestNarEvent::EndDirectory,
    ];
    write_nar(&events).to_vec()
}

fn zstd_compress(data: &[u8]) -> Vec<u8> {
    zstd::encode_all(std::io::Cursor::new(data), 3).unwrap()
}

// ── DB fixture rows ───────────────────────────────────────────────────────────

fn org_row() -> gradient_entity::organization::Model {
    gradient_entity::organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Org".into(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

fn project_row() -> gradient_entity::project::Model {
    gradient_entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        repository: "https://example.com/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: 3,
        sign_cache: true,
        ..Default::default()
    }
}

fn evaluation_row() -> gradient_entity::evaluation::Model {
    gradient_entity::evaluation::Model {
        id: evaluation_id(),
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: CommitId::now_v7(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn build_row() -> gradient_entity::build::Model {
    gradient_entity::build::Model {
        id: build_id(),
        evaluation: evaluation_id(),
        derivation: derivation_id(),
        status: BuildStatus::Completed,
        created_at: test_date(),
        updated_at: test_date(),
        ..Default::default()
    }
}

fn drv_output_row() -> gradient_entity::derivation_output::Model {
    gradient_entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: derivation_id(),
        name: "out".into(),
        hash: FIXTURE_HASH.into(),
        package: "pkg".into(),
        is_cached: true,
        created_at: test_date(),
        ..Default::default()
    }
}

fn build_product_id() -> BuildProductId {
    BuildProductId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000008").unwrap())
}

fn build_product_row() -> gradient_entity::build_product::Model {
    gradient_entity::build_product::Model {
        id: build_product_id(),
        derivation_output: drv_output_id(),
        file_type: "file".into(),
        subtype: "iso".into(),
        name: "image.iso".into(),
        path: format!("{}/image.iso", STORE_PATH),
        size: Some(ISO_BYTES.len() as i64),
        created_at: test_date(),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build mock DB with the standard query sequence for `BuildAccessContext::load`:
///   0. EBuild::find_by_id          → build_row
///   1. EEvaluation::find_by_id     → evaluation_row
///   2. EProject::find_by_id        → project_row
///   3. EOrganization::find_by_id   → org_row  (public=true, no member check)
///
/// Then the endpoint-specific queries:
///   4. EDerivationOutput::find()   → drv_output_row
///   5. EBuildProduct::find()       → build_product_row  (listing endpoint)
fn mock_db_for_downloads() -> sea_orm::DatabaseConnection {
    MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![build_row()]])
        .append_query_results([vec![evaluation_row()]])
        .append_query_results([vec![project_row()]])
        .append_query_results([vec![org_row()]])
        .append_query_results([vec![drv_output_row()]])
        .append_query_results([vec![build_product_row()]])
        .into_connection()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Listing endpoint returns products from `build_product` DB rows (no NAR access needed).
#[test]
fn listing_returns_products_from_db() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Set up state - nar_storage is not needed for listing.
        let cli = test_cli();
        let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");

        let db = mock_db_for_downloads();
        let state = Arc::new(ServerState {
            web_db: WebDb::new(db),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config: std::sync::Arc::new(
                gradient_types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
            ),
            log_storage: Arc::new(NoopLogStorage),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_util::http::build_client().expect("http client"),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
            board_events: tokio::sync::broadcast::channel(256).0,
            forge: gradient_forge::ForgeRegistry::with_builtin(),
            reactor: std::sync::Arc::new(gradient_db::NoReactor),
        });

        let router = create_router(state);
        let server = TestServer::new(router);

        let response = server
            .get(&format!("/api/v1/builds/{}/downloads", build_id()))
            .await;

        response.assert_status_ok();
        let body = response.text();

        // Response should list image.iso from the build_product row.
        assert!(
            body.contains("image.iso"),
            "listing did not contain image.iso:\n{body}"
        );
        assert!(
            body.contains("iso"),
            "listing did not contain file_type 'iso':\n{body}"
        );
    });
}

/// Download endpoint extracts `image.iso` from the NAR stored in `nar_storage`
/// and returns its contents.
#[test]
fn download_streams_file_from_nar() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Prepare NAR and compress it.
        let nar = build_test_nar();
        let compressed = zstd_compress(&nar);

        // Set up state with nar_storage populated.
        let cli = test_cli();
        let nar_storage = NarStore::local(&cli.storage.base_path).expect("create test NarStore");
        nar_storage
            .put(FIXTURE_HASH, compressed)
            .await
            .expect("put NAR");

        // For get_build_download, mock DB query sequence:
        //   0. EBuild::find_by_id          → build_row  (load_unguarded)
        //   1. EEvaluation::find_by_id     → evaluation_row
        //   2. EProject::find_by_id        → project_row
        //   3. EOrganization::find_by_id   → org_row  (public=true, skip member check)
        //   4. EDerivationOutput::find()   → drv_output_row
        //   5. EBuildProduct::find()       → build_product_row
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![build_row()]])
            .append_query_results([vec![evaluation_row()]])
            .append_query_results([vec![project_row()]])
            .append_query_results([vec![org_row()]])
            .append_query_results([vec![drv_output_row()]])
            .append_query_results([vec![build_product_row()]])
            .into_connection();

        let state = Arc::new(ServerState {
            web_db: WebDb::new(db),
            worker_db: WorkerDb::new(
                MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
            ),
            config: std::sync::Arc::new(
                gradient_types::RuntimeConfig::from_cli(&cli).expect("valid test config"),
            ),
            log_storage: Arc::new(NoopLogStorage),
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            http: gradient_util::http::build_client().expect("http client"),
            shutdown: gradient_util::shutdown::Shutdown::new(),
            jwt_secret: gradient_types::SecretString::new("test-jwt-secret".to_string()),
            started_at: chrono::Utc::now(),
            pending_org_memberships: std::sync::Arc::new(std::collections::HashMap::new()),
            oidc_group_roles: std::sync::Arc::new(std::collections::HashMap::new()),
            board_events: tokio::sync::broadcast::channel(256).0,
            forge: gradient_forge::ForgeRegistry::with_builtin(),
            reactor: std::sync::Arc::new(gradient_db::NoReactor),
        });

        let router = create_router(state);
        let server = TestServer::new(router);

        let response = server
            .get(&format!("/api/v1/builds/{}/download/image.iso", build_id()))
            .await;

        response.assert_status_ok();
        let body = response.as_bytes().to_vec();

        assert_eq!(
            body, ISO_BYTES,
            "downloaded bytes do not match expected ISO content"
        );
    });
}
