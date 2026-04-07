/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod consts;
pub mod database;
pub mod derivation;
pub mod email;
pub mod evaluator;
pub mod executer;
pub mod gc;
pub mod input;
pub mod log_storage;
pub mod nix_flake;
pub mod permission;
pub mod pool;
pub mod sources;
pub mod state;
pub mod types;
pub mod webhooks;

use database::connect_db;
use email::EmailService;
use executer::SshBuildExecutor;
use log_storage::FileLogStorage;
use pool::LocalNixStoreProvider;
use sources::Libgit2Prefetcher;
use state::load_and_apply_state;
use std::path::Path;
use std::sync::Arc;
use types::*;
use webhooks::ReqwestWebhookClient;

pub async fn init_state(
    cli: Cli,
    derivation_resolver: Arc<dyn evaluator::DerivationResolver>,
) -> Arc<ServerState> {
    println!("Starting Gradient Server on {}:{}", cli.ip, cli.port);
    println!("State file configured: {:?}", cli.state_file);

    let db = match connect_db(&cli).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };

    // Load and apply state configuration if provided
    if let Err(e) = load_and_apply_state(
        &db,
        cli.state_file.as_deref(),
        &cli.crypt_secret_file,
        cli.delete_state,
    )
    .await
    {
        eprintln!("Failed to load state configuration: {}", e);
        std::process::exit(1);
    }

    let log_storage = match FileLogStorage::new(Path::new(&cli.base_path)).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize log storage: {}", e);
            std::process::exit(1);
        }
    };

    let nix_store: Arc<dyn pool::NixStoreProvider> =
        Arc::new(LocalNixStoreProvider::new(cli.max_nixdaemon_connections));
    let web_nix_store: Arc<dyn pool::NixStoreProvider> = Arc::new(LocalNixStoreProvider::new(1));

    let webhook_client = match ReqwestWebhookClient::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build webhook HTTP client: {}", e);
            std::process::exit(1);
        }
    };
    let webhooks: Arc<dyn webhooks::WebhookClient> = Arc::new(webhook_client);

    let email_service = match EmailService::new(&cli).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize email service: {}", e);
            std::process::exit(1);
        }
    };
    let email: Arc<dyn email::EmailSender> = Arc::new(email_service);

    let flake_prefetcher: Arc<dyn sources::FlakePrefetcher> = Arc::new(Libgit2Prefetcher::new());

    let build_executor: Arc<dyn executer::BuildExecutor> = Arc::new(SshBuildExecutor::new());

    Arc::new(ServerState {
        db,
        cli,
        log_storage: Arc::new(log_storage),
        nix_store,
        web_nix_store,
        webhooks,
        email,
        flake_prefetcher,
        derivation_resolver,
        build_executor,
    })
}
