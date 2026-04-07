/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::cli::test_cli;
use crate::fakes::build_executor::FakeBuildExecutor;
use crate::fakes::derivation_resolver::FakeDerivationResolver;
use crate::fakes::email::InMemoryEmailSender;
use crate::fakes::flake_prefetcher::FakeFlakePrefetcher;
use crate::fakes::nix_store::FakeNixStoreProvider;
use crate::fakes::webhooks::RecordingWebhookClient;
use crate::log_storage::NoopLogStorage;
use gradient_core::email::EmailSender;
use gradient_core::evaluator::DerivationResolver;
use gradient_core::executer::BuildExecutor;
use gradient_core::pool::NixStoreProvider;
use gradient_core::sources::FlakePrefetcher;
use gradient_core::types::ServerState;
use gradient_core::webhooks::WebhookClient;
use sea_orm::DatabaseConnection;
use std::sync::Arc;

/// Wrap a `DatabaseConnection` + `test_cli()` into a `ServerState`.
pub fn test_state(db: DatabaseConnection) -> Arc<ServerState> {
    Arc::new(ServerState {
        db,
        cli: test_cli(),
        log_storage: Arc::new(NoopLogStorage),
        nix_store: Arc::new(FakeNixStoreProvider::new()) as Arc<dyn NixStoreProvider>,
        web_nix_store: Arc::new(FakeNixStoreProvider::new()) as Arc<dyn NixStoreProvider>,
        webhooks: Arc::new(RecordingWebhookClient::new()) as Arc<dyn WebhookClient>,
        email: Arc::new(InMemoryEmailSender::new()) as Arc<dyn EmailSender>,
        flake_prefetcher: Arc::new(FakeFlakePrefetcher::new()) as Arc<dyn FlakePrefetcher>,
        derivation_resolver: Arc::new(FakeDerivationResolver::new())
            as Arc<dyn DerivationResolver>,
        build_executor: Arc::new(FakeBuildExecutor::new()) as Arc<dyn BuildExecutor>,
    })
}
