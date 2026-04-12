/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::email::InMemoryEmailSender;
use crate::fakes::nix_store::FakeNixStoreProvider;
use crate::fakes::webhooks::RecordingWebhookClient;
use crate::log_storage::NoopLogStorage;
use gradient_core::storage::EmailSender;
use gradient_core::storage::NarStore;
use gradient_core::executer::NixStoreProvider;
use gradient_core::types::ServerState;
use gradient_core::ci::WebhookClient;
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
        nix_store: Arc::new(FakeNixStoreProvider::new()) as Arc<dyn NixStoreProvider>,
        web_nix_store: Arc::new(FakeNixStoreProvider::new()) as Arc<dyn NixStoreProvider>,
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        nar_storage,
    })
}
