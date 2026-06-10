/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;
pub mod board_events;
pub mod build_output_metadata;
pub mod cached_path_info;
pub mod cli;
pub mod config;
pub mod constants;
pub mod consts;
pub mod forge;
pub mod ids;
pub mod input;
pub mod log_api;
pub mod proto;
pub mod secret;
pub mod triggers;
pub mod waiting_reason;
pub mod wildcard;

mod entity_aliases;
mod io;
mod nix_cache;

pub use self::actions::{ActionConfig, ActionType};
pub use self::board_events::BoardEvent;
pub use self::build_output_metadata::BuildOutputMetadata;
pub use self::cached_path_info::CachedPathInfo;
pub use self::cli::{
    CidrParseError, DatabaseArgs, EmailArgs, EvalArgs, GitHubAppArgs, LimitsArgs, LoggingArgs,
    MetricsArgs, NetworkArgs, OidcArgs, ProtoArgs, RegistrationArgs, S3Args, SecretsArgs,
    ServerArgs, StorageArgs, in_any, parse_cidr_list,
};
pub use self::config::{
    ConfigError, EmailConfig, GitHubAppConfig, MetricsConfig, NetworkConfig, OidcConfig,
    RuntimeConfig, S3Config,
};
pub use self::consts::*;
pub use self::entity_aliases::*;
pub use self::forge::ForgeType;
pub use self::ids::*;
pub use self::input::*;
pub use self::io::*;
pub use self::log_api::{LogChunkIndex, LogChunkMeta, LogSearchDone, LogSearchHit};
pub use self::nix_cache::*;
pub use self::secret::{SecretBytes, SecretString};
pub use self::triggers::{ConcurrencyPolicy, TriggerConfig, TriggerConfigError, TriggerType};
pub use self::waiting_reason::{UnmetRequirement, WaitingReason};
pub use self::wildcard::*;

use chrono::NaiveDateTime;
use clap::Parser;
use serde::{Deserialize, Serialize};

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
    #[command(flatten)]
    pub network: NetworkArgs,
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
