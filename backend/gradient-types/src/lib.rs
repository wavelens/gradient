/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;

pub mod cli;
pub mod config;
pub mod constants;
pub mod consts;
pub mod events;
pub mod flake_url;
pub mod git_host;
pub mod ids;
pub mod input;
pub mod log_api;
pub mod secret;
pub mod triggers;
pub mod waiting_reason;
pub mod wildcard;

mod entity_aliases;
mod io;
mod nix_cache;

pub use self::actions::{ActionConfig, ActionType, PatchGeneratorKind, PrGranularity, VerifyGate};
pub use self::cli::{
    BuildArgs, CacheArgs, CidrParseError, CreatePermission, DatabaseArgs, EmailArgs, EvalArgs,
    GcArgs, GitHubAppArgs, GradientCiArgs, HttpArgs, LogArgs, MetricsArgs, NarArgs, OidcArgs,
    PermissionsArgs, ProtoArgs, PullRequestsArgs, RegistrationArgs, S3Args, SchedulerArgs,
    ScimArgs, SecretsArgs, SentryArgs, ServerArgs, SshArgs, StateArgs, UploadArgs, in_any,
    parse_cidr_list,
};
pub use self::config::{
    ConfigError, EmailConfig, GitHubAppConfig, MetricsConfig, NetworkConfig, OidcConfig,
    RuntimeConfig, S3Config, ScimConfig,
};
pub use self::consts::*;
pub use self::entity_aliases::*;
pub use self::events::build::DownloadProgress;
pub use self::events::evaluation::EvaluationProgress;
pub use self::events::{Envelope, Event, EventBus, EventRx};
pub use self::flake_url::{NixFlakeUrl, RepositoryUrl};
pub use self::git_host::GitHostType;
pub use self::ids::*;
pub use self::input::*;
pub use self::io::*;
pub use self::log_api::{LogChunkIndex, LogChunkMeta, LogSearchDone, LogSearchHit};
pub use self::nix_cache::*;
pub use self::secret::{SecretBytes, SecretString};
pub use self::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerConfigError, TriggerType};
pub use self::waiting_reason::{EvalCapability, UnmetRequirement, WaitingReason};
pub use self::wildcard::*;

use chrono::NaiveDateTime;
use clap::Parser;
use serde::{Deserialize, Serialize};

#[inline]
pub fn now() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

#[derive(Parser, Debug, Clone, Default)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient-server", author = "Wavelens", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub server: ServerArgs,
    #[command(flatten)]
    pub secrets: SecretsArgs,
    #[command(flatten)]
    pub state: StateArgs,
    #[command(flatten)]
    pub permissions: PermissionsArgs,
    #[command(flatten)]
    pub registration: RegistrationArgs,
    #[command(flatten)]
    pub gradient_ci: GradientCiArgs,
    #[command(flatten)]
    pub sentry: SentryArgs,
    #[command(flatten)]
    pub pull_requests: PullRequestsArgs,
    #[command(flatten)]
    pub database: DatabaseArgs,
    #[command(flatten)]
    pub http: HttpArgs,
    #[command(flatten)]
    pub proto: ProtoArgs,
    #[command(flatten)]
    pub upload: UploadArgs,
    #[command(flatten)]
    pub nar: NarArgs,
    #[command(flatten)]
    pub cache: CacheArgs,
    #[command(flatten)]
    pub gc: GcArgs,
    #[command(flatten)]
    pub eval: EvalArgs,
    #[command(flatten)]
    pub build: BuildArgs,
    #[command(flatten)]
    pub scheduler: SchedulerArgs,
    #[command(flatten)]
    pub metrics: MetricsArgs,
    #[command(flatten)]
    pub log: LogArgs,
    #[command(flatten)]
    pub oidc: OidcArgs,
    #[command(flatten)]
    pub scim: ScimArgs,
    #[command(flatten)]
    pub email: EmailArgs,
    #[command(flatten)]
    pub s3: S3Args,
    #[command(flatten)]
    pub github_app: GitHubAppArgs,
    #[command(flatten)]
    pub ssh: SshArgs,
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

impl<T> Paginated<Vec<T>> {
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> Paginated<Vec<U>> {
        Paginated {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
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
