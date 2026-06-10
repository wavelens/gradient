/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod ci;
pub mod constants;
pub mod db;
pub mod executor;
pub mod forge;
pub mod nix;
pub mod permissions;
pub mod sources;
pub mod state;
pub mod state_machine;
pub mod state_root;
pub mod storage;

pub use state_root::{AppState, ServerState};

use db::{WebDb, WorkerDb, connect_db, connect_web_db};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use gradient_util::shutdown::Shutdown;
use state::load_and_apply_state;
use std::path::Path;
use std::sync::Arc;
use storage::EmailService;
use storage::NarStore;
use storage::{FileLogStorage, S3LogStorage};
use gradient_types::*;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("missing required secret files (--crypt-secret-file / --jwt-secret-file)")]
    MissingSecrets,
    #[error("invalid network config: {0}")]
    NetworkConfig(String),
    #[error("database connection failed: {0}")]
    Database(#[source] anyhow::Error),
    #[error("web database pool failed: {0}")]
    WebDatabase(#[source] anyhow::Error),
    #[error("failed to load state configuration: {0}")]
    StateLoad(String),
    #[error("log storage init failed: {0}")]
    LogStorage(#[source] anyhow::Error),
    #[error("http client build failed: {0}")]
    HttpClient(#[source] anyhow::Error),
    #[error("failed to load JWT secret: {0}")]
    JwtSecret(#[source] anyhow::Error),
    #[error("email service init failed: {0}")]
    Email(#[source] anyhow::Error),
    #[error("S3 NAR storage error: {0}")]
    S3Storage(String),
    #[error("local NAR storage error: {0}")]
    LocalStorage(String),
}

pub async fn init_state(cli: Cli) -> Result<Arc<ServerState>, InitError> {
    if cli.secrets.crypt_secret_file.is_empty() || cli.secrets.jwt_secret_file.is_empty() {
        return Err(InitError::MissingSecrets);
    }

    tracing::info!(
        ip = %cli.server.ip,
        port = cli.server.port,
        state_file = ?cli.storage.state_file,
        "Starting Gradient server bootstrap",
    );

    let config = Arc::new(
        RuntimeConfig::from_cli(&cli).map_err(|e| InitError::NetworkConfig(e.to_string()))?,
    );

    let db = connect_db(&cli).await.map_err(InitError::Database)?;
    let web_db = connect_web_db(&cli).await.map_err(InitError::WebDatabase)?;

    let state_result = load_and_apply_state(
        &db,
        cli.storage.state_file.as_deref(),
        &cli.secrets.crypt_secret_file,
        cli.storage.delete_state,
        cli.email.email_enabled,
    )
    .await
    .map_err(|e| InitError::StateLoad(e.to_string()))?;
    let pending_org_memberships = Arc::new(state_result.pending);
    let oidc_group_roles = Arc::new(state_result.oidc_group_roles);

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

    let local_log_storage = FileLogStorage::new(Path::new(&cli.storage.base_path))
        .await
        .map_err(InitError::LogStorage)?;

    let http = gradient_util::http::build_client().map_err(|e| InitError::HttpClient(e.into()))?;

    let jwt_secret =
        gradient_types::input::load_secret(&cli.secrets.jwt_secret_file).map_err(InitError::JwtSecret)?;

    let email_service = EmailService::new(cli.email_config())
        .await
        .map_err(InitError::Email)?;
    let email: Arc<dyn storage::EmailSender> = Arc::new(email_service);

    let nar_storage = if let Some(s3) = cli.s3_config() {
        let secret = match s3.secret_access_key_file.as_deref() {
            Some(path) => Some(
                std::fs::read_to_string(path)
                    .map_err(|e| {
                        InitError::S3Storage(format!(
                            "failed to read S3 secret access key file '{path}': {e}"
                        ))
                    })?
                    .trim()
                    .to_string(),
            ),
            None => None,
        };

        let store = NarStore::s3(
            &s3.bucket,
            &s3.region,
            s3.endpoint.as_deref(),
            s3.access_key_id.as_deref(),
            secret.as_deref(),
            &s3.prefix,
            s3.virtual_hosted_style,
        )
        .map_err(|e| InitError::S3Storage(e.to_string()))?;

        store.ping().await.map_err(|e| {
            InitError::S3Storage(format!("bucket '{}' unreachable: {e:#}", s3.bucket))
        })?;

        tracing::info!(
            bucket = %s3.bucket,
            access_key_id = ?s3.access_key_id.as_deref(),
            secret_loaded = secret.is_some(),
            "NAR storage: S3",
        );
        store
    } else {
        let store = NarStore::local(&cli.storage.base_path)
            .map_err(|e| InitError::LocalStorage(e.to_string()))?;
        tracing::info!(path = %cli.storage.base_path, "NAR storage: local");
        store
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

    let reactor: Arc<dyn db::StatusReactor> =
        Arc::new(crate::ci::CiStatusReactor::new(http.clone()));

    Ok(Arc::new(ServerState {
        worker_db: WorkerDb::new(db),
        web_db: WebDb::new(web_db),
        config,
        log_storage,
        email,
        nar_storage,
        manifest_state: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        pending_credentials: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        http,
        forge: forge::ForgeRegistry::with_builtin(),
        shutdown: Shutdown::new(),
        jwt_secret,
        started_at: chrono::Utc::now(),
        pending_org_memberships,
        oidc_group_roles,
        board_events: tokio::sync::broadcast::channel(256).0,
        reactor,
    }))
}
