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
use gradient_core::storage::NarStore;
use gradient_core::types::ServerState;
use sea_orm::DatabaseConnection;
use std::sync::Arc;

/// Wrap a `DatabaseConnection` + `test_cli()` into a `ServerState`.
pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    let cli = test_cli();
    let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");
    Arc::new(ServerState {
        db,
        cli,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
    })
}

/// Like `test_state` but uses a custom `crypt_secret_file` path and returns
/// the `RecordingWebhookClient` separately so tests can inspect deliveries.
pub fn test_state_recorded(
    db: DatabaseConnection,
    crypt_secret_file: String,
) -> (Arc<ServerState>, Arc<RecordingWebhookClient>) {
    let cli = test_cli_with_crypt(crypt_secret_file);
    let nar_storage = NarStore::local(&cli.base_path).expect("create test NarStore");
    let recorder = Arc::new(RecordingWebhookClient::new());
    let state = Arc::new(ServerState {
        db,
        cli,
        log_storage: Arc::new(NoopLogStorage),
        webhooks: Arc::clone(&recorder) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
    });
    (state, recorder)
}
