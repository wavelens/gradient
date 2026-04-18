/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod build_output_metadata;
pub mod cached_path_info;
pub mod config;
pub mod consts;
pub mod input;
pub mod proto;
pub mod secret;
pub mod wildcard;

mod entity_aliases;
mod io;
mod nix_cache;

pub use self::build_output_metadata::BuildOutputMetadata;
pub use self::cached_path_info::CachedPathInfo;
pub use self::config::{EmailConfig, GitHubAppConfig, OidcConfig, S3Config};
pub use self::consts::*;
pub use self::entity_aliases::*;
pub use self::input::*;
pub use self::io::*;
pub use self::nix_cache::*;
pub use self::secret::{SecretBytes, SecretString};
pub use self::wildcard::*;

use super::ci::webhook::WebhookClient;
use super::executer::pool::NixStoreProvider;
use super::storage::LogStorage;
use super::storage::NarStore;
use super::storage::email::EmailSender;
use clap::Parser;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient-server", author = "Wavelens", version, about, long_about = None)]
pub struct Cli {
    /// Default log level for the whole binary. Per-component overrides:
    /// `--builder-log-level`, `--cache-log-level`, `--web-log-level`.
    #[arg(long, env = "GRADIENT_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
    /// Log level for the `builder` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_BUILDER_LOG_LEVEL")]
    pub builder_log_level: Option<String>,
    /// Log level for the `cache` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_CACHE_LOG_LEVEL")]
    pub cache_log_level: Option<String>,
    /// Log level for the `web` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_WEB_LOG_LEVEL")]
    pub web_log_level: Option<String>,
    /// Log level for the `proto` crate. Defaults to `--log-level`.
    #[arg(long, env = "GRADIENT_PROTO_LOG_LEVEL")]
    pub proto_log_level: Option<String>,
    #[arg(long, env = "GRADIENT_IP", default_value = "127.0.0.1")]
    pub ip: String,
    #[arg(long, env = "GRADIENT_PORT", value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    #[arg(
        long,
        env = "GRADIENT_SERVE_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub serve_url: String,
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    pub database_url: Option<String>,
    #[arg(long, env = "GRADIENT_DATABASE_URL_FILE")]
    pub database_url_file: Option<String>,
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_EVALUATIONS", value_parser = greater_than_zero::<usize>, default_value = "10")]
    pub max_concurrent_evaluations: usize,
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_BUILDS", value_parser = greater_than_zero::<usize>, default_value = "1000")]
    pub max_concurrent_builds: usize,
    #[arg(long, env = "GRADIENT_EVALUATION_TIMEOUT", value_parser = greater_than_zero::<i64>, default_value = "10")]
    pub evaluation_timeout: i64,
    #[arg(long, env = "GRADIENT_STORE_PATH")]
    pub store_path: Option<String>,
    #[arg(long, env = "GRADIENT_BASE_PATH", default_value = ".")]
    pub base_path: String,
    #[arg(long, env = "GRADIENT_ENABLE_REGISTRATION", default_value = "true")]
    pub enable_registration: bool,
    #[arg(long, env = "GRADIENT_OIDC_ENABLED", default_value = "false")]
    pub oidc_enabled: bool,
    #[arg(long, env = "GRADIENT_OIDC_REQUIRED", default_value = "false")]
    pub oidc_required: bool,
    #[arg(long, env = "GRADIENT_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_CLIENT_SECRET_FILE")]
    pub oidc_client_secret_file: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_SCOPES")]
    pub oidc_scopes: Option<String>,
    #[arg(long, env = "GRADIENT_OIDC_DISCOVERY_URL")]
    pub oidc_discovery_url: Option<String>,
    #[arg(long, env = "GRADIENT_CRYPT_SECRET_FILE")]
    pub crypt_secret_file: String,
    #[arg(long, env = "GRADIENT_JWT_SECRET_FILE")]
    pub jwt_secret_file: String,
    #[arg(long, env = "GRADIENT_SERVE_CACHE", default_value = "false")]
    pub serve_cache: bool,
    #[arg(long, env = "GRADIENT_REPORT_ERRORS", default_value = "false")]
    pub report_errors: bool,
    #[arg(long, env = "GRADIENT_EMAIL_ENABLED", default_value = "false")]
    pub email_enabled: bool,
    #[arg(
        long,
        env = "GRADIENT_EMAIL_REQUIRE_VERIFICATION",
        default_value = "false"
    )]
    pub email_require_verification: bool,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_HOST")]
    pub email_smtp_host: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_PORT", default_value = "587")]
    pub email_smtp_port: u16,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_USERNAME")]
    pub email_smtp_username: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_SMTP_PASSWORD_FILE")]
    pub email_smtp_password_file: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_FROM_ADDRESS")]
    pub email_from_address: Option<String>,
    #[arg(long, env = "GRADIENT_EMAIL_FROM_NAME", default_value = "Gradient")]
    pub email_from_name: String,
    #[arg(long, env = "GRADIENT_EMAIL_ENABLE_TLS", default_value = "true")]
    pub email_enable_tls: bool,
    #[arg(long, env = "GRADIENT_STATE_FILE")]
    pub state_file: Option<String>,
    #[arg(long, env = "GRADIENT_DELETE_STATE", default_value = "true")]
    pub delete_state: bool,
    #[arg(long, env = "GRADIENT_KEEP_EVALUATIONS", default_value = "0")]
    pub keep_evaluations: usize,
    #[arg(long, env = "GRADIENT_MAX_NIXDAEMON_CONNECTIONS", value_parser = greater_than_zero::<usize>, default_value = "8")]
    pub max_nixdaemon_connections: usize,
    /// Number of long-lived Nix evaluator worker subprocesses to keep around.
    /// Each worker hosts one persistent embedded `NixEvaluator`, paying the
    /// libnix init cost only once. Must be at least `1`: in-process evaluation
    /// is unsafe because the Nix C API `EvalState` is not thread-safe and the
    /// embedded Boehm GC conflicts with Tokio's signal handling.
    #[arg(long, env = "GRADIENT_EVAL_WORKERS", value_parser = greater_than_zero::<usize>, default_value = "1")]
    pub eval_workers: usize,
    /// Recycle an eval-worker subprocess after it has served this many
    /// `list` / `resolve` calls. Nix's Boehm GC never releases memory
    /// back to the OS, so long-lived workers grow monotonically; this
    /// cap bounds RSS growth by forcing a respawn. Set to 0 to disable.
    #[arg(long, env = "GRADIENT_MAX_EVALUATIONS_PER_WORKER", default_value = "1")]
    pub max_evaluations_per_worker: usize,
    /// TTL in hours for cached NAR files that have not been fetched recently.
    /// When expired the NAR is removed from storage and its GC root is deleted.
    /// Set to 0 to disable (default).
    #[arg(long, env = "GRADIENT_NAR_TTL_HOURS", default_value_t = 0)]
    pub nar_ttl_hours: u64,
    /// Grace period in hours before the GC pass deletes a `derivation` row
    /// that no longer has any referencing `build` rows. The grace lets rapid
    /// re-evaluations reuse a freshly-orphaned derivation without
    /// re-inserting it. Set to 0 to GC immediately.
    #[arg(
        long,
        env = "GRADIENT_KEEP_ORPHAN_DERIVATIONS_HOURS",
        default_value_t = 24
    )]
    pub keep_orphan_derivations_hours: i64,

    // ── S3 / object-storage options ──────────────────────────────────────────
    /// S3 bucket name. When set, NARs are stored in S3 instead of local disk.
    #[arg(long, env = "GRADIENT_S3_BUCKET")]
    pub s3_bucket: Option<String>,
    /// AWS region for the S3 bucket.
    #[arg(long, env = "GRADIENT_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,
    /// Custom S3-compatible endpoint URL (MinIO, Cloudflare R2, …).
    #[arg(long, env = "GRADIENT_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,
    /// AWS access key ID. Falls back to instance credentials when absent.
    #[arg(long, env = "GRADIENT_S3_ACCESS_KEY_ID")]
    pub s3_access_key_id: Option<String>,
    /// File containing the AWS secret access key.
    #[arg(long, env = "GRADIENT_S3_SECRET_ACCESS_KEY_FILE")]
    pub s3_secret_access_key_file: Option<String>,
    /// Key prefix within the S3 bucket (e.g. "gradient/").
    #[arg(long, env = "GRADIENT_S3_PREFIX", default_value = "")]
    pub s3_prefix: String,
    /// Public URL of the Gradient frontend, used to build links in CI status
    /// reports (e.g. `https://gradient.example.com`). Defaults to `serve_url`.
    #[arg(
        long,
        env = "GRADIENT_FRONTEND_URL",
        default_value = "http://127.0.0.1:8000"
    )]
    pub frontend_url: String,

    // ── GitHub App options ────────────────────────────────────────────────────
    /// GitHub App ID. Required to enable GitHub App webhook and CI reporting.
    #[arg(long, env = "GRADIENT_GITHUB_APP_ID")]
    pub github_app_id: Option<u64>,
    /// Path to the GitHub App RS256 private key PEM file.
    #[arg(long, env = "GRADIENT_GITHUB_APP_PRIVATE_KEY_FILE")]
    pub github_app_private_key_file: Option<String>,
    /// Path to a file containing the shared secret used to verify incoming
    /// GitHub App webhook payloads (`X-Hub-Signature-256`). The file's
    /// contents must match the value configured on the GitHub App's webhook
    /// settings page.
    #[arg(long, env = "GRADIENT_GITHUB_APP_WEBHOOK_SECRET_FILE")]
    pub github_app_webhook_secret_file: Option<String>,

    // ── WebSocket protocol options ────────────────────────────────────────────
    /// Advertise HTTP/3 (QUIC) support to connecting clients.
    /// Enabling this does NOT change the backend transport — configure nginx
    /// with `listen 443 quic` and set the `Alt-Svc` header there.
    /// This flag is surfaced via `GET /api/v1/config` so clients can choose
    /// whether to attempt an HTTP/3 upgrade.
    #[arg(long, env = "GRADIENT_QUIC", default_value = "false")]
    pub quic: bool,

    /// Maximum number of simultaneous proto WebSocket connections.
    #[arg(long, env = "GRADIENT_MAX_PROTO_CONNECTIONS", default_value = "256")]
    pub max_proto_connections: usize,

    /// Accept incoming connections on `/proto` (workers and federated servers).
    /// Enabled by default — disable to reject all `/proto` connections.
    #[arg(long, env = "GRADIENT_DISCOVERABLE", default_value = "true")]
    pub discoverable: bool,

    /// Accept federated connections from other Gradient servers on `/proto`.
    /// Requires `discoverable` to be enabled.
    #[arg(long, env = "GRADIENT_FEDERATE_PROTO", default_value = "false")]
    pub federate_proto: bool,

    /// Expose `GET /api/v1/workers` and worker stats without authentication.
    /// When `false` (default), only superusers can access those endpoints.
    #[arg(long, env = "GRADIENT_GLOBAL_STATS_PUBLIC", default_value = "false")]
    pub global_stats_public: bool,

    /// Whether the server is served over TLS (HTTPS). Controls the `Secure`
    /// flag on session cookies. Set to `false` for plain HTTP deployments.
    #[arg(long, env = "GRADIENT_USE_TLS", default_value = "true")]
    pub use_tls: bool,
}

#[derive(Debug)]
pub struct ServerState {
    pub db: DatabaseConnection,
    pub cli: Cli,
    pub log_storage: Arc<dyn LogStorage>,
    pub nix_store: Arc<dyn NixStoreProvider>,
    pub web_nix_store: Arc<dyn NixStoreProvider>,
    pub webhooks: Arc<dyn WebhookClient>,
    pub email: Arc<dyn EmailSender>,
    pub nar_storage: NarStore,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Paginated<T> {
    pub items: T,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

#[derive(Deserialize, Debug, Default)]
pub struct PaginationParams {
    pub page: Option<u64>,
    pub per_page: Option<u64>,
}

impl PaginationParams {
    pub fn page(&self) -> u64 {
        self.page.unwrap_or(1).max(1)
    }
    pub fn per_page(&self) -> u64 {
        self.per_page.unwrap_or(50).clamp(1, 100)
    }
}
