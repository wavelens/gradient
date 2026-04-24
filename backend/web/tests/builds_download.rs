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
//! Uses manual Tokio runtime because `#[tokio::test]` expands to `::core::…`
//! references that clash with the local `core` crate name in the workspace.

use axum_test::TestServer;
use bytes::Bytes;
use core::ci::WebhookClient;
use core::storage::{EmailSender, NarStore};
use core::types::ServerState;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use harmonia_nar::archive::test_data::{TestNarEvent, TestNarEvents};
use harmonia_nar::archive::write_nar;
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use test_support::fakes::email::InMemoryEmailSender;
use test_support::fakes::webhooks::RecordingWebhookClient;
use test_support::log_storage::NoopLogStorage;
use test_support::prelude::test_cli;
use uuid::Uuid;
use web::create_router;

// ── Fixture IDs ───────────────────────────────────────────────────────────────

fn org_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap()
}
fn project_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000002").unwrap()
}
fn evaluation_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000003").unwrap()
}
fn derivation_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000004").unwrap()
}
fn build_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000005").unwrap()
}
fn drv_output_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000006").unwrap()
}
fn user_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000007").unwrap()
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

fn org_row() -> entity::organization::Model {
    entity::organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Org".into(),
        description: String::new(),
        public_key: "pub".into(),
        private_key: "priv".into(),
        public: true,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        github_installation_id: None,
        github_app_enabled: false,
    }
}

fn project_row() -> entity::project::Model {
    entity::project::Model {
        id: project_id(),
        organization: org_id(),
        name: "test-project".into(),
        active: true,
        display_name: "Test Project".into(),
        description: String::new(),
        repository: "https://example.com/repo".into(),
        evaluation_wildcard: "*".into(),
        last_evaluation: None,
        last_check_at: test_date(),
        force_evaluation: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
        keep_evaluations: 10,
    }
}

fn evaluation_row() -> entity::evaluation::Model {
    entity::evaluation::Model {
        id: evaluation_id(),
        project: Some(project_id()),
        repository: "https://example.com/repo".into(),
        commit: Uuid::new_v4(),
        wildcard: "*".into(),
        status: EvaluationStatus::Completed,
        previous: None,
        next: None,
        created_at: test_date(),
        updated_at: test_date(),
        flake_source: None,
    }
}

fn build_row() -> entity::build::Model {
    entity::build::Model {
        id: build_id(),
        evaluation: evaluation_id(),
        derivation: derivation_id(),
        status: BuildStatus::Completed,
        log_id: None,
        build_time_ms: None,
        worker: None,
        created_at: test_date(),
        updated_at: test_date(),
    }
}

fn drv_output_row() -> entity::derivation_output::Model {
    entity::derivation_output::Model {
        id: drv_output_id(),
        derivation: derivation_id(),
        name: "out".into(),
        output: STORE_PATH.into(),
        hash: FIXTURE_HASH.into(),
        package: "pkg".into(),
        ca: None,
        file_hash: None,
        file_size: None,
        nar_size: None,
        is_cached: true,
        cached_path: None,
        created_at: test_date(),
    }
}

fn build_product_id() -> Uuid {
    Uuid::parse_str("20000000-0000-0000-0000-000000000008").unwrap()
}

fn build_product_row() -> entity::build_product::Model {
    entity::build_product::Model {
        id: build_product_id(),
        derivation_output: drv_output_id(),
        file_type: "iso".into(),
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
        // Set up state — nar_storage is not needed for listing.
        let cli = test_cli();
        let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");

        let db = mock_db_for_downloads();
        let state = Arc::new(ServerState {
            db,
            cli,
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
        let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");
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
            db,
            cli,
            log_storage: Arc::new(NoopLogStorage),
            webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
            email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
            nar_storage,
            manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
