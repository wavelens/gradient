/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod build_output_metadata;
pub mod cached_path_info;
pub mod cli;
pub mod config;
pub mod consts;
pub mod db;
pub mod ids;
pub mod input;
pub mod proto;
pub mod secret;
pub mod triggers;
pub mod waiting_reason;
pub mod wildcard;

mod entity_aliases;
mod io;
mod nix_cache;

pub use self::build_output_metadata::BuildOutputMetadata;
pub use self::cached_path_info::CachedPathInfo;
pub use self::cli::{
    DatabaseArgs, EmailArgs, EvalArgs, GitHubAppArgs, LimitsArgs, LoggingArgs, MetricsArgs,
    OidcArgs, ProtoArgs, RegistrationArgs, S3Args, SecretsArgs, ServerArgs, StorageArgs,
};
pub use self::config::{
    EmailConfig, GitHubAppConfig, MetricsConfig, OidcConfig, RuntimeConfig, S3Config,
};
pub use self::consts::*;
pub use self::db::{WebDb, WorkerDb};
pub use self::entity_aliases::*;
pub use self::ids::*;
pub use self::input::*;
pub use self::io::*;
pub use self::nix_cache::*;
pub use self::secret::{SecretBytes, SecretString};
pub use self::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerConfigError, TriggerType};
pub use self::waiting_reason::{UnmetRequirement, WaitingReason};
pub use self::wildcard::*;

use super::ci::webhook::WebhookClient;
use super::shutdown::Shutdown;
use super::storage::LogStorage;
use super::storage::NarStore;
use super::storage::email::EmailSender;
use chrono::NaiveDateTime;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Current UTC wall-clock as a naive `NaiveDateTime`, the timestamp shape
/// every persisted column expects. Single source of truth for code that
/// would otherwise spell `chrono::Utc::now().naive_utc()`.
#[inline]
pub fn now() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

#[derive(Parser, Debug, Clone)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient-server", author = "Wavelens", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub logging: LoggingArgs,
    #[command(flatten)]
    pub server: ServerArgs,
    #[command(flatten)]
    pub database: DatabaseArgs,
    #[command(flatten)]
    pub eval: EvalArgs,
    #[command(flatten)]
    pub storage: StorageArgs,
    #[command(flatten)]
    pub secrets: SecretsArgs,
    #[command(flatten)]
    pub limits: LimitsArgs,
    #[command(flatten)]
    pub registration: RegistrationArgs,
    #[command(flatten)]
    pub proto: ProtoArgs,
    #[command(flatten)]
    pub oidc: OidcArgs,
    #[command(flatten)]
    pub email: EmailArgs,
    #[command(flatten)]
    pub s3: S3Args,
    #[command(flatten)]
    pub github_app: GitHubAppArgs,
    #[command(flatten)]
    pub metrics: MetricsArgs,
}

#[derive(Debug)]
pub struct ServerState {
    /// Pool used by the proto handler, scheduler, cache GC, and any
    /// fire-and-forget background task spawned from a web handler that
    /// should not contend with foreground HTTP requests.
    pub worker_db: WorkerDb,
    /// Dedicated DB pool used by the axum/web layer so HTTP requests are
    /// not starved by the busy proto/scheduler pool under heavy NarPush load.
    pub web_db: WebDb,
    /// Resolved runtime configuration. Built once at startup from the parsed
    /// [`Cli`]. Replaces the prior `cli: Cli` field so handlers depend on the
    /// slice they need (`state.config.<group>.<field>`) instead of the full
    /// 65-field parser DTO.
    pub config: Arc<RuntimeConfig>,
    pub log_storage: Arc<dyn LogStorage>,
    pub webhooks: Arc<dyn WebhookClient>,
    pub email: Arc<dyn EmailSender>,
    pub nar_storage: NarStore,
    /// Shared outbound HTTP client. Reuse this for any outbound request
    /// made from a handler or background task — never construct a fresh
    /// `reqwest::Client` per call.
    pub http: reqwest::Client,
    /// Issued-but-unconsumed manifest CSRF state tokens with their issuance time.
    pub manifest_state: Arc<crate::ci::manifest_state::ManifestStateStore>,
    /// Manifest results awaiting one-shot pickup by the superuser's browser.
    pub pending_credentials: Arc<crate::ci::manifest_state::PendingCredentialsStore>,
    /// Graceful-shutdown coordination for all long-lived background tasks
    /// (dispatch loops, outbound, cache loops, webhook deliveries, etc.).
    pub shutdown: Shutdown,
    /// JWT signing/verification secret loaded once at startup. Holding it in
    /// memory avoids reading `secrets.jwt_secret_file` on every request and
    /// makes the auth path resilient to transient filesystem errors.
    pub jwt_secret: SecretString,
    /// Wall-clock time the process bootstrapped. Used to derive
    /// `gradient_uptime_seconds` for the metrics endpoint.
    pub started_at: chrono::DateTime<chrono::Utc>,
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
