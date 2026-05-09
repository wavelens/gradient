/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::{test_cli, test_cli_with_crypt};
use crate::fakes::email::InMemoryEmailSender;
use crate::fakes::webhooks::RecordingWebhookClient;
use crate::log_storage::NoopLogStorage;
use gradient_core::ci::WebhookClient;
use gradient_core::storage::EmailSender;
use gradient_core::storage::LogStorage;
use gradient_core::storage::NarStore;
use gradient_core::types::{RuntimeConfig, SecretString, ServerState, WebDb, WorkerDb};
use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
use std::sync::Arc;

/// Wrap a `DatabaseConnection` + `test_cli()` into a `ServerState`.
///
/// Tests that don't exercise the web layer get an empty mock for `web_db`.
/// Tests that need a populated web pool should construct `ServerState`
/// directly so they can supply their own mock query results.
pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(db),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("build test HTTP client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

/// Like `test_state` but plumbs through a caller-supplied [`LogStorage`].
///
/// Use with `RecordingLogStorage` to assert what handlers append to the
/// build log.
pub fn test_state_with_log_storage(
    db: DatabaseConnection,
    log_storage: Arc<dyn LogStorage>,
) -> Arc<ServerState> {
    let cli = test_cli();
    let config = Arc::new(RuntimeConfig::from_cli(&cli));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(db),
        config,
        log_storage,
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("build test HTTP client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    })
}

/// Like `test_state` but uses a custom `crypt_secret_file` path and returns
/// the `RecordingWebhookClient` separately so tests can inspect deliveries.
pub fn test_state_recorded(
    db: DatabaseConnection,
    crypt_secret_file: String,
) -> (Arc<ServerState>, Arc<RecordingWebhookClient>) {
    let cli = test_cli_with_crypt(crypt_secret_file);
    let config = Arc::new(RuntimeConfig::from_cli(&cli));
    let nar_storage = NarStore::local(&config.storage.base_path).expect("create test NarStore");
    let recorder = Arc::new(RecordingWebhookClient::new());
    let state = Arc::new(ServerState {
        web_db: WebDb::new(MockDatabase::new(DatabaseBackend::Postgres).into_connection()),
        worker_db: WorkerDb::new(db),
        config,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::clone(&recorder) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http: gradient_core::http::build_client().expect("build test HTTP client"),
        shutdown: gradient_core::shutdown::Shutdown::new(),
        jwt_secret: SecretString::new("test-jwt-secret".to_string()),
        started_at: chrono::Utc::now(),
    });
    (state, recorder)
}
