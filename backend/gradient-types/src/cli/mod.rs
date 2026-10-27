/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Each module is one config group. Its NixOS option path, flag and env var share one name,
//! like `upload.bytesBudget`, `--upload-bytes-budget` and `GRADIENT_UPLOAD_BYTES_BUDGET`.

mod build;
mod cache;
mod database;
mod email;
mod eval;
mod exec;
mod gc;
mod github_app;
mod gradient_ci;
mod http;
mod log;
mod metrics;
mod nar;
mod oidc;
mod options;
mod permissions;
mod proto;
mod pull_requests;
mod registration;
mod s3;
mod scheduler;
mod scim;
mod secrets;
mod sentry;
mod server;
mod ssh;
mod state;
mod upload;

pub use build::BuildArgs;
pub use cache::CacheArgs;
pub use database::DatabaseArgs;
pub use email::EmailArgs;
pub use eval::{DEFAULT_KEEP_EVALUATIONS, EvalArgs};
pub use exec::ExecArgs;
pub use gc::GcArgs;
pub use github_app::GitHubAppArgs;
pub use gradient_ci::GradientCiArgs;
pub use http::{CidrParseError, HttpArgs, in_any, parse_cidr_list};
pub use log::LogArgs;
pub use metrics::MetricsArgs;
pub use nar::NarArgs;
pub use oidc::OidcArgs;
pub use options::{OptionSpec, options_json};
pub use permissions::{CreatePermission, PermissionsArgs};
pub use proto::ProtoArgs;
pub use pull_requests::PullRequestsArgs;
pub use registration::RegistrationArgs;
pub use s3::S3Args;
pub use scheduler::{SchedulerArgs, ScoringPolicy};
pub use scim::ScimArgs;
pub use secrets::SecretsArgs;
pub use sentry::{DEFAULT_SENTRY_DSN, SentryArgs, effective_sentry_dsn};
pub use server::ServerArgs;
pub use ssh::SshArgs;
pub use state::StateArgs;
pub use upload::UploadArgs;

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::CommandFactory;

    #[test]
    fn arguments_are_unique() {
        Cli::command().debug_assert();
    }
}
