/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod actions;

extern crate self as gradient_types;

pub mod branch_key;
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
pub mod permissions;
pub mod runner;
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
    PermissionsArgs, ProtoArgs, PullRequestsArgs, RegistrationArgs, RunnerArgs, S3Args,
    SchedulerArgs, ScimArgs, ScoringPolicy, SecretsArgs, SentryArgs, ServerArgs, SshArgs,
    StateArgs, UploadArgs, in_any, parse_cidr_list,
};
pub use self::config::{
    ConfigError, EmailConfig, GitHubAppConfig, MetricsConfig, NetworkConfig, OidcConfig,
    RuntimeConfig, S3Config, ScimConfig,
};
pub use self::consts::*;
pub use self::entity_aliases::*;
pub use self::events::build::{BuildProgress, BuildProgressPhase};
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
pub use self::waiting_reason::{EvalCapability, RunnerUnmet, UnmetRequirement, WaitingReason};
pub use self::wildcard::*;

use chrono::NaiveDateTime;
use clap::Parser;
use gradient_derive::config_group;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use utoipa::openapi::{ArrayBuilder, ObjectBuilder, Ref, RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

#[inline]
pub fn now() -> NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

#[config_group(no_default)]
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
    #[command(flatten)]
    pub runner: RunnerArgs,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u64,
    pub per_page: u64,
}

impl<T: ToSchema> PartialSchema for Paginated<T> {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .property(
                "items",
                ArrayBuilder::new().items(Ref::from_schema_name(T::name())),
            )
            .required("items")
            .property("total", u64::schema())
            .required("total")
            .property("page", u64::schema())
            .required("page")
            .property("per_page", u64::schema())
            .required("per_page")
            .into()
    }
}

impl<T: ToSchema> ToSchema for Paginated<T> {
    fn name() -> Cow<'static, str> {
        Cow::Owned(format!("Paginated_{}", T::name()))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((T::name().into_owned(), T::schema()));
        T::schemas(schemas);
    }
}

impl<T> Paginated<T> {
    pub fn map<U, F: FnMut(T) -> U>(self, f: F) -> Paginated<U> {
        Paginated {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
}

#[derive(Deserialize, Debug, Default, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
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
