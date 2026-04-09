/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::email::EmailSender;
use super::evaluator::DerivationResolver;
use super::executer::BuildExecutor;
use super::input::{greater_than_zero, port_in_range};
use super::log_storage::LogStorage;
use super::nar_storage::NarStore;
use super::pool::NixStoreProvider;
use super::sources::FlakePrefetcher;
use super::webhooks::WebhookClient;
use clap::Parser;
use entity::*;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use uuid::Uuid;

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
    #[arg(long, env = "GRADIENT_BINPATH_NIX", default_value = "nix")]
    pub binpath_nix: String,
    #[arg(long, env = "GRADIENT_BINPATH_SSH", default_value = "ssh")]
    pub binpath_ssh: String,
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
    #[arg(
        long,
        env = "GRADIENT_MAX_EVALUATIONS_PER_WORKER",
        default_value = "20"
    )]
    pub max_evaluations_per_worker: usize,
    /// Number of top-level derivations whose closure BFS runs in parallel
    /// during the `EvaluatingDerivation` phase. Each walker issues DB
    /// and Nix-store queries concurrently, so raising this reduces
    /// evaluation latency at the cost of DB pool / nix-daemon pressure.
    #[arg(long, env = "GRADIENT_EVAL_CLOSURE_PARALLELISM", value_parser = greater_than_zero::<usize>, default_value = "8")]
    pub eval_closure_parallelism: usize,
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
    pub flake_prefetcher: Arc<dyn FlakePrefetcher>,
    pub derivation_resolver: Arc<dyn DerivationResolver>,
    pub build_executor: Arc<dyn BuildExecutor>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub name: String,
    pub managed: bool,
}

/// Combined async I/O trait used for type-erasing Nix daemon connections.
pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}

/// Type-erased wrapper over any bidirectional async I/O stream.
/// Lets callers hold a `DaemonStore<BoxedIo>` regardless of whether the
/// underlying transport is a Unix socket, a stdio pipe, or a TCP channel.
pub struct BoxedIo(Box<dyn AsyncIo>);

impl std::fmt::Debug for BoxedIo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxedIo").finish_non_exhaustive()
    }
}

impl BoxedIo {
    pub fn new(io: impl AsyncIo + 'static) -> Self {
        Self(Box::new(io))
    }
}

impl AsyncRead for BoxedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NixCacheInfo {
    #[serde(rename = "WantMassQuery")]
    pub want_mass_query: bool,
    #[serde(rename = "StoreDir")]
    pub store_dir: String,
    #[serde(rename = "Priority")]
    pub priority: i32,
}

impl NixCacheInfo {
    pub fn to_nix_string(&self) -> String {
        format!(
            "WantMassQuery: {}\nStoreDir: {}\nPriority: {}",
            self.want_mass_query, self.store_dir, self.priority
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NixPathInfo {
    #[serde(rename = "StorePath")]
    pub store_path: String,
    #[serde(rename = "URL")]
    pub url: String,
    #[serde(rename = "Compression")]
    pub compression: String,
    #[serde(rename = "FileHash")]
    pub file_hash: String,
    #[serde(rename = "FileSize")]
    pub file_size: u32,
    #[serde(rename = "NarHash")]
    pub nar_hash: String,
    #[serde(rename = "NarSize")]
    pub nar_size: u64,
    #[serde(rename = "References")]
    pub references: Vec<String>,
    #[serde(rename = "Sig")]
    pub sig: String,
    #[serde(rename = "Deriver")]
    pub deriver: Option<String>,
    #[serde(rename = "CA")]
    pub ca: Option<String>,
}

impl NixPathInfo {
    pub fn to_nix_string(&self) -> String {
        format!(
            "StorePath: {}\nURL: {}\nCompression: {}\nFileHash: {}\nFileSize: {}\nNarHash: {}\nNarSize: {}\nReferences: {}{}\nSig: {}{}\n",
            self.store_path,
            self.url,
            self.compression,
            self.file_hash,
            self.file_size,
            self.nar_hash,
            self.nar_size,
            self.references.join(" "),
            self.deriver
                .as_ref()
                .map(|deriver| format!("\nDeriver: {}", deriver))
                .unwrap_or_default(),
            self.sig,
            self.ca
                .as_ref()
                .map(|ca| format!("\nCA: {}", ca))
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildOutputPath {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "outPath")]
    pub out_path: String,
    #[serde(rename = "signatures")]
    pub signatures: Vec<String>,
}

pub type ListResponse = Vec<ListItem>;

pub type EApi = api::Entity;
pub type EBuild = build::Entity;
pub type ECache = cache::Entity;
pub type ECacheDerivation = cache_derivation::Entity;
pub type ECacheMetric = cache_metric::Entity;
pub type ECacheUpstream = cache_upstream::Entity;
pub type ECommit = commit::Entity;
pub type EDerivation = derivation::Entity;
pub type EDerivationDependency = derivation_dependency::Entity;
pub type EDerivationFeature = derivation_feature::Entity;
pub type EDerivationOutput = derivation_output::Entity;
pub type EDerivationOutputSignature = derivation_output_signature::Entity;
pub type EDirectBuild = direct_build::Entity;
pub type EEntryPoint = entry_point::Entity;
pub type EEntryPointMessage = entry_point_message::Entity;
pub type EEvaluation = evaluation::Entity;
pub type EEvaluationMessage = evaluation_message::Entity;
pub type EFeature = feature::Entity;
pub type EOrganization = organization::Entity;
pub type EOrganizationCache = organization_cache::Entity;
pub type EOrganizationUser = organization_user::Entity;
pub type EProject = project::Entity;
pub type ERole = role::Entity;
pub type EServer = server::Entity;
pub type EServerArchitecture = server_architecture::Entity;
pub type EServerFeature = server_feature::Entity;
pub type EUser = user::Entity;
pub type EWebhook = webhook::Entity;

pub type MApi = api::Model;
pub type MBuild = build::Model;
pub type MCache = cache::Model;
pub type MCacheDerivation = cache_derivation::Model;
pub type MCacheMetric = cache_metric::Model;
pub type MCacheUpstream = cache_upstream::Model;
pub type MCommit = commit::Model;
pub type MDerivation = derivation::Model;
pub type MDerivationDependency = derivation_dependency::Model;
pub type MDerivationFeature = derivation_feature::Model;
pub type MDerivationOutput = derivation_output::Model;
pub type MDerivationOutputSignature = derivation_output_signature::Model;
pub type MDirectBuild = direct_build::Model;
pub type MEntryPoint = entry_point::Model;
pub type MEntryPointMessage = entry_point_message::Model;
pub type MEvaluation = evaluation::Model;
pub type MEvaluationMessage = evaluation_message::Model;
pub type MFeature = feature::Model;
pub type MOrganization = organization::Model;
pub type MOrganizationCache = organization_cache::Model;
pub type MOrganizationUser = organization_user::Model;
pub type MProject = project::Model;
pub type MRole = role::Model;
pub type MServer = server::Model;
pub type MServerArchitecture = server_architecture::Model;
pub type MServerFeature = server_feature::Model;
pub type MUser = user::Model;
pub type MWebhook = webhook::Model;

pub type AApi = api::ActiveModel;
pub type ABuild = build::ActiveModel;
pub type ACache = cache::ActiveModel;
pub type ACacheDerivation = cache_derivation::ActiveModel;
pub type ACacheMetric = cache_metric::ActiveModel;
pub type ACacheUpstream = cache_upstream::ActiveModel;
pub type ACommit = commit::ActiveModel;
pub type ADerivation = derivation::ActiveModel;
pub type ADerivationDependency = derivation_dependency::ActiveModel;
pub type ADerivationFeature = derivation_feature::ActiveModel;
pub type ADerivationOutput = derivation_output::ActiveModel;
pub type ADerivationOutputSignature = derivation_output_signature::ActiveModel;
pub type ADirectBuild = direct_build::ActiveModel;
pub type AEntryPoint = entry_point::ActiveModel;
pub type AEntryPointMessage = entry_point_message::ActiveModel;
pub type AEvaluation = evaluation::ActiveModel;
pub type AEvaluationMessage = evaluation_message::ActiveModel;
pub type AFeature = feature::ActiveModel;
pub type AOrganization = organization::ActiveModel;
pub type AOrganizationCache = organization_cache::ActiveModel;
pub type AOrganizationUser = organization_user::ActiveModel;
pub type AProject = project::ActiveModel;
pub type ARole = role::ActiveModel;
pub type AServer = server::ActiveModel;
pub type AServerArchitecture = server_architecture::ActiveModel;
pub type AServerFeature = server_feature::ActiveModel;
pub type AUser = user::ActiveModel;
pub type AWebhook = webhook::ActiveModel;

pub type CApi = api::Column;
pub type CBuild = build::Column;
pub type CCache = cache::Column;
pub type CCacheDerivation = cache_derivation::Column;
pub type CCacheMetric = cache_metric::Column;
pub type CCacheUpstream = cache_upstream::Column;
pub type CCommit = commit::Column;
pub type CDerivation = derivation::Column;
pub type CDerivationDependency = derivation_dependency::Column;
pub type CDerivationFeature = derivation_feature::Column;
pub type CDerivationOutput = derivation_output::Column;
pub type CDerivationOutputSignature = derivation_output_signature::Column;
pub type CDirectBuild = direct_build::Column;
pub type CEntryPoint = entry_point::Column;
pub type CEntryPointMessage = entry_point_message::Column;
pub type CEvaluation = evaluation::Column;
pub type CEvaluationMessage = evaluation_message::Column;
pub type CFeature = feature::Column;
pub type COrganization = organization::Column;
pub type COrganizationCache = organization_cache::Column;
pub type COrganizationUser = organization_user::Column;
pub type CProject = project::Column;
pub type CRole = role::Column;
pub type CServer = server::Column;
pub type CServerArchitecture = server_architecture::Column;
pub type CServerFeature = server_feature::Column;
pub type CUser = user::Column;
pub type CWebhook = webhook::Column;

pub type RApi = api::Relation;
pub type RBuild = build::Relation;
pub type RCache = cache::Relation;
pub type RCacheDerivation = cache_derivation::Relation;
pub type RCommit = commit::Relation;
pub type RDerivation = derivation::Relation;
pub type RDerivationDependency = derivation_dependency::Relation;
pub type RDerivationFeature = derivation_feature::Relation;
pub type RDerivationOutput = derivation_output::Relation;
pub type RDerivationOutputSignature = derivation_output_signature::Relation;
pub type RDirectBuild = direct_build::Relation;
pub type REntryPoint = entry_point::Relation;
pub type REntryPointMessage = entry_point_message::Relation;
pub type REvaluation = evaluation::Relation;
pub type REvaluationMessage = evaluation_message::Relation;
pub use evaluation_message::MessageLevel;
pub type RFeature = feature::Relation;
pub type ROrganization = organization::Relation;
pub type ROrganizationCache = organization_cache::Relation;
pub type ROrganizationUser = organization_user::Relation;
pub type RProject = project::Relation;
pub type RRole = role::Relation;
pub type RServer = server::Relation;
pub type RServerArchitecture = server_architecture::Relation;
pub type RServerFeature = server_feature::Relation;
pub type RUser = user::Relation;
pub type RWebhook = webhook::Relation;

/// Convenience bundle for code that needs the attempt fields (`MBuild`) and
/// the spec fields (`MDerivation`) together. Produced by joining `build` on
/// `derivation` at query time.
#[derive(Debug, Clone)]
pub struct BuildWithDerivation {
    pub build: MBuild,
    pub derivation: MDerivation,
}
