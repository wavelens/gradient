/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod ci_reporter;
pub mod consts;
pub mod database;
pub mod evaluation_trigger;
pub mod github_app;
pub mod derivation;
pub mod email;
pub mod evaluator;
pub mod executer;
pub mod gc;
pub mod input;
pub mod log_storage;
pub mod nar_storage;
pub mod nix_flake;
pub mod nix_url;
pub mod permission;
pub mod pool;
pub mod sources;
pub mod state;
pub mod status;
pub mod types;
pub mod webhooks;
pub mod wildcard;

use database::connect_db;
use email::EmailService;
use executer::SshBuildExecutor;
use log_storage::{FileLogStorage, S3LogStorage};
use nar_storage::NarStore;
use pool::LocalNixStoreProvider;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, ActiveModelTrait, ActiveValue::Set, IntoActiveModel};
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

    // Cap keep_evaluations on all projects that exceed the configured maximum.
    if cli.keep_evaluations > 0 {
        let max = cli.keep_evaluations as i32;
        let over_limit = EProject::find()
            .filter(CProject::KeepEvaluations.gt(max))
            .all(&db)
            .await;

        match over_limit {
            Ok(projects) => {
                let count = projects.len();
                for project in projects {
                    let mut active = project.into_active_model();
                    active.keep_evaluations = Set(max);
                    if let Err(e) = active.update(&db).await {
                        eprintln!("Failed to cap keep_evaluations for project: {}", e);
                    }
                }
                if count > 0 {
                    println!(
                        "Capped keep_evaluations to {} on {} project(s)",
                        max, count
                    );
                }
            }
            Err(e) => eprintln!("Failed to query projects for keep_evaluations cap: {}", e),
        }
    }

    let local_log_storage = match FileLogStorage::new(Path::new(&cli.base_path)).await {
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

    let nar_storage = if let Some(ref bucket) = cli.s3_bucket {
        let secret = match cli.s3_secret_access_key_file.as_deref() {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(s) => Some(s.trim().to_string()),
                Err(e) => {
                    eprintln!("Failed to read S3 secret access key file '{}': {}", path, e);
                    std::process::exit(1);
                }
            },
            None => None,
        };

        let store = match NarStore::s3(
            bucket,
            &cli.s3_region,
            cli.s3_endpoint.as_deref(),
            cli.s3_access_key_id.as_deref(),
            secret.as_deref(),
            &cli.s3_prefix,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to initialize S3 NAR storage: {}", e);
                std::process::exit(1);
            }
        };
        if let Err(e) = store.ping().await {
            eprintln!("S3 NAR storage unreachable (bucket '{}'): {:#}", bucket, e);
            std::process::exit(1);
        }
        println!("NAR storage: S3 bucket '{}'", bucket);
        tracing::debug!(
            bucket = %bucket,
            access_key_id = ?cli.s3_access_key_id.as_deref(),
            secret_loaded = secret.is_some(),
            "S3 NAR storage initialized",
        );
        store
    } else {
        match NarStore::local(&cli.base_path) {
            Ok(store) => {
                println!("NAR storage: local ({})", cli.base_path);
                store
            }
            Err(e) => {
                eprintln!("Failed to initialize local NAR storage: {}", e);
                std::process::exit(1);
            }
        }
    };

    let log_storage: Arc<dyn log_storage::LogStorage> = if cli.s3_bucket.is_some() {
        println!("Log storage: S3 (with local cache)");
        Arc::new(S3LogStorage::new(
            local_log_storage,
            nar_storage.inner(),
            nar_storage.prefix(),
        ))
    } else {
        Arc::new(local_log_storage)
    };

    Arc::new(ServerState {
        db,
        cli,
        log_storage,
        nix_store,
        web_nix_store,
        webhooks,
        email,
        flake_prefetcher,
        derivation_resolver,
        build_executor,
        nar_storage,
    })
}
