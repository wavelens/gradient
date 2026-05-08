/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed `clap::Args` clusters that compose the top-level [`super::Cli`].
//!
//! Each module groups the flags of one feature so handlers and tests can
//! depend on a narrow slice instead of the full 65-field god struct. Field
//! names, env vars, defaults and doc comments are preserved verbatim — only
//! the Rust access path changes (e.g. `cli.port` → `cli.server.port`).

mod database;
mod email;
mod eval;
mod github_app;
mod limits;
mod logging;
mod metrics;
mod oidc;
mod proto;
mod registration;
mod s3;
mod secrets;
mod server;
mod storage;

pub use database::DatabaseArgs;
pub use email::EmailArgs;
pub use eval::EvalArgs;
pub use github_app::GitHubAppArgs;
pub use limits::LimitsArgs;
pub use logging::LoggingArgs;
pub use metrics::MetricsArgs;
pub use oidc::OidcArgs;
pub use proto::ProtoArgs;
pub use registration::RegistrationArgs;
pub use s3::S3Args;
pub use secrets::SecretsArgs;
pub use server::ServerArgs;
pub use storage::StorageArgs;
