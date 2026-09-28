/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed `clap::Args` clusters that compose the top-level [`super::Cli`].
//!
//! Each module is one config group: its NixOS option path, flag and env var
//! share one name (`upload.bytesBudget`, `--upload-bytes-budget`,
//! `GRADIENT_UPLOAD_BYTES_BUDGET`).

mod build;
mod cache;
mod database;
mod email;
mod eval;
mod gc;
mod github_app;
mod http;
mod log;
mod metrics;
mod nar;
mod oidc;
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
mod state;
mod upload;

pub use build::BuildArgs;
pub use cache::CacheArgs;
pub use database::DatabaseArgs;
pub use email::EmailArgs;
pub use eval::{DEFAULT_KEEP_EVALUATIONS, EvalArgs};
pub use gc::GcArgs;
pub use github_app::GitHubAppArgs;
pub use http::{CidrParseError, HttpArgs, in_any, parse_cidr_list};
pub use log::LogArgs;
pub use metrics::MetricsArgs;
pub use nar::NarArgs;
pub use oidc::OidcArgs;
pub use permissions::{CreatePermission, PermissionsArgs};
pub use proto::ProtoArgs;
pub use pull_requests::PullRequestsArgs;
pub use registration::RegistrationArgs;
pub use s3::S3Args;
pub use scheduler::SchedulerArgs;
pub use scim::ScimArgs;
pub use secrets::SecretsArgs;
pub use sentry::{DEFAULT_SENTRY_DSN, SentryArgs, effective_sentry_dsn};
pub use server::ServerArgs;
pub use state::StateArgs;
pub use upload::UploadArgs;

/// The values a group takes with no flag and no environment: its clap defaults.
fn clap_defaults<T: clap::Args + clap::FromArgMatches>() -> T {
    let command = T::augment_args(clap::Command::new("defaults")).mut_args(|arg| arg.env(None));
    T::from_arg_matches(&command.get_matches_from(["defaults"]))
        .expect("every option of the group has a default")
}

#[cfg(test)]
mod tests {
    use crate::Cli;
    use clap::CommandFactory;

    #[test]
    fn arguments_are_unique() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_env_is_the_prefixed_flag() {
        let mismatched: Vec<String> = Cli::command()
            .get_arguments()
            .filter_map(|arg| {
                let long = arg.get_long()?;
                if matches!(long, "help" | "version" | "state-validate") {
                    return None;
                }

                let expected = format!("GRADIENT_{}", long.to_uppercase().replace('-', "_"));
                let env = arg.get_env().map(|e| e.to_string_lossy().into_owned());
                (env.as_deref() != Some(expected.as_str())).then(|| format!("--{long}: {env:?}"))
            })
            .collect();

        assert!(mismatched.is_empty(), "{mismatched:#?}");
    }
}
