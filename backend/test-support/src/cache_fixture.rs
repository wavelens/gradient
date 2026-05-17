/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::fakes::webhooks::RecordingWebhookClient;
use crate::log_storage::NoopLogStorage;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::{EmailSender, NarStore};
use gradient_core::types::ids::*;
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, MockDatabase};
use std::sync::Arc;
use uuid::Uuid;

pub const FIXTURE_CACHE_NAME: &str = "test-cache";

fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap())
}

fn org_id() -> OrganizationId {
    OrganizationId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
}

fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

/// Build a `ServerState` with a single public, active cache named [`FIXTURE_CACHE_NAME`].
/// No `cached_path` or `derivation_output` rows are seeded — suitable for endpoint-level
/// tests that don't exercise store-path resolution (e.g. `nix-cache-info`).
pub async fn public_cache_state() -> Arc<ServerState> {
    let cache_row = entity::cache::Model {
        id: cache_id(),
        name: FIXTURE_CACHE_NAME.into(),
        display_name: "Test Cache".into(),
        description: String::new(),
        active: true,
        priority: 30,
        local_priority: None,
        public_key: "test-pub-key".into(),
        private_key: "test-priv-key".into(),
        public: true,
        created_by: UserId::new(org_id().into_inner()),
        created_at: test_date(),
        managed: false,
    };

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![cache_row]])
        .into_connection();

    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli).expect("valid test config"));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");

    Arc::new(ServerState {
        web_db: WebDb::new(db),
        worker_db: WorkerDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("http client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}
