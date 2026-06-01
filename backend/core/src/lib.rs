/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cache;
pub mod ci;
pub mod constants;
pub mod db;
pub mod executer;
pub mod http;
pub mod hydra;
pub mod ip_allowlist;
pub mod nix;
pub mod nix_hash;
pub mod permissions;
pub mod repo;
pub mod shutdown;
pub mod sources;
pub mod state;
pub mod state_machine;
pub mod storage;
pub mod types;

use db::{connect_db, connect_web_db};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use shutdown::Shutdown;
use state::load_and_apply_state;
use std::path::Path;
use std::sync::Arc;
use storage::EmailService;
use storage::NarStore;
use storage::{FileLogStorage, S3LogStorage};
use types::*;

pub async fn init_state(cli: Cli) -> Arc<ServerState> {
    tracing::info!(
        ip = %cli.server.ip,
        port = cli.server.port,
        state_file = ?cli.storage.state_file,
        "Starting Gradient server bootstrap",
    );

    let config = Arc::new(RuntimeConfig::from_cli(&cli).unwrap_or_else(|e| {
        tracing::error!(error = %e, "invalid network config");
        std::process::exit(1);
    }));

    let db = match connect_db(&cli).await {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "Failed to connect to database");
            std::process::exit(1);
        }
    };

    let web_db = match connect_web_db(&cli).await {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "Failed to connect web database pool");
            std::process::exit(1);
        }
    };

    let pending_org_memberships = match load_and_apply_state(
        &db,
        cli.storage.state_file.as_deref(),
        &cli.secrets.crypt_secret_file,
        cli.storage.delete_state,
        cli.email.email_enabled,
    )
    .await
    {
        Ok(p) => Arc::new(p),
        Err(e) => {
            tracing::error!(error = %e, "Failed to load state configuration");
            std::process::exit(1);
        }
    };

    if cli.storage.keep_evaluations > 0 {
        let max = cli.storage.keep_evaluations as i32;
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
                        tracing::error!(error = %e, "Failed to cap keep_evaluations for project");
                    }
                }
                if count > 0 {
                    tracing::info!(max, count, "Capped keep_evaluations on projects");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to query projects for keep_evaluations cap");
            }
        }
    }

    let local_log_storage = match FileLogStorage::new(Path::new(&cli.storage.base_path)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize log storage");
            std::process::exit(1);
        }
    };

    let http = match http::build_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to build shared HTTP client");
            std::process::exit(1);
        }
    };

    let jwt_secret = match types::input::load_secret(&cli.secrets.jwt_secret_file) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to load JWT secret");
            std::process::exit(1);
        }
    };

    let email_service = match EmailService::new(cli.email_config()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialize email service");
            std::process::exit(1);
        }
    };
    let email: Arc<dyn storage::EmailSender> = Arc::new(email_service);

    let nar_storage = if let Some(s3) = cli.s3_config() {
        let secret = match s3.secret_access_key_file.as_deref() {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(s) => Some(s.trim().to_string()),
                Err(e) => {
                    tracing::error!(
                        path,
                        error = %e,
                        "Failed to read S3 secret access key file",
                    );
                    std::process::exit(1);
                }
            },
            None => None,
        };

        let store = match NarStore::s3(
            &s3.bucket,
            &s3.region,
            s3.endpoint.as_deref(),
            s3.access_key_id.as_deref(),
            secret.as_deref(),
            &s3.prefix,
            s3.virtual_hosted_style,
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "Failed to initialize S3 NAR storage");
                std::process::exit(1);
            }
        };
        if let Err(e) = store.ping().await {
            tracing::error!(
                bucket = %s3.bucket,
                error = format!("{e:#}"),
                "S3 NAR storage unreachable",
            );
            std::process::exit(1);
        }
        tracing::info!(
            bucket = %s3.bucket,
            access_key_id = ?s3.access_key_id.as_deref(),
            secret_loaded = secret.is_some(),
            "NAR storage: S3",
        );
        store
    } else {
        match NarStore::local(&cli.storage.base_path) {
            Ok(store) => {
                tracing::info!(path = %cli.storage.base_path, "NAR storage: local");
                store
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to initialize local NAR storage");
                std::process::exit(1);
            }
        }
    };

    let log_storage: Arc<dyn storage::LogStorage> = if cli.s3_config().is_some() {
        tracing::info!("Log storage: S3 (with local cache)");
        Arc::new(S3LogStorage::new(
            local_log_storage,
            nar_storage.inner(),
            nar_storage.prefix(),
        ))
    } else {
        Arc::new(local_log_storage)
    };

    Arc::new(ServerState {
        worker_db: WorkerDb::new(db),
        web_db: WebDb::new(web_db),
        config,
        log_storage,
        email,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http,
        shutdown: Shutdown::new(),
        jwt_secret,
        started_at: chrono::Utc::now(),
        pending_org_memberships,
    })
}
