/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::input::{greater_than_zero, port_in_range};
use async_ssh2_lite::{AsyncChannel, TokioTcpStream};
use clap::Parser;
use entity::*;
use nix_daemon::nix::DaemonStore;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient-server", author = "Wavelens", version, about, long_about = None)]
pub struct Cli {
    #[arg(long, env = "GRADIENT_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
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
    #[arg(long, env = "GRADIENT_DISABLE_REGISTRATION", default_value = "false")]
    pub disable_registration: bool,
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
    #[arg(long, env = "GRADIENT_BINPATH_GIT", default_value = "git")]
    pub binpath_git: String,
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
    #[arg(long, env = "GRADIENT_EMAIL_DISABLE_TLS", default_value = "false")]
    pub email_disable_tls: bool,
    #[arg(long, env = "GRADIENT_STATE_FILE")]
    pub state_file: Option<String>,
    #[arg(long, env = "GRADIENT_DELETE_STATE", default_value = "true")]
    pub delete_state: bool,
}

#[derive(Debug)]
pub struct ServerState {
    pub db: DatabaseConnection,
    pub cli: Cli,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BaseResponse<T> {
    pub error: bool,
    pub message: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub name: String,
}

pub struct CommandDuplex {
    pub stdin: tokio::process::ChildStdin,
    pub stdout: tokio::process::ChildStdout,
}

impl AsyncRead for CommandDuplex {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for CommandDuplex {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stdin).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

pub enum LocalNixStore {
    UnixStream(DaemonStore<UnixStream>),
    CommandDuplex(DaemonStore<CommandDuplex>),
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
pub type NixStore = DaemonStore<AsyncChannel<TokioTcpStream>>;

pub type EApi = api::Entity;
pub type EBuild = build::Entity;
pub type EBuildDependency = build_dependency::Entity;
pub type EBuildFeature = build_feature::Entity;
pub type EBuildOutput = build_output::Entity;
pub type EBuildOutputSignature = build_output_signature::Entity;
pub type ECache = cache::Entity;
pub type ECommit = commit::Entity;
pub type EDirectBuild = direct_build::Entity;
pub type EEvaluation = evaluation::Entity;
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

pub type MApi = api::Model;
pub type MBuild = build::Model;
pub type MBuildDependency = build_dependency::Model;
pub type MBuildFeature = build_feature::Model;
pub type MBuildOutput = build_output::Model;
pub type MBuildOutputSignature = build_output_signature::Model;
pub type MCache = cache::Model;
pub type MCommit = commit::Model;
pub type MDirectBuild = direct_build::Model;
pub type MEvaluation = evaluation::Model;
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

pub type AApi = api::ActiveModel;
pub type ABuild = build::ActiveModel;
pub type ABuildDependency = build_dependency::ActiveModel;
pub type ABuildFeature = build_feature::ActiveModel;
pub type ABuildOutput = build_output::ActiveModel;
pub type ABuildOutputSignature = build_output_signature::ActiveModel;
pub type ACache = cache::ActiveModel;
pub type ACommit = commit::ActiveModel;
pub type ADirectBuild = direct_build::ActiveModel;
pub type AEvaluation = evaluation::ActiveModel;
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

pub type CApi = api::Column;
pub type CBuild = build::Column;
pub type CBuildDependency = build_dependency::Column;
pub type CBuildFeature = build_feature::Column;
pub type CBuildOutput = build_output::Column;
pub type CBuildOutputSignature = build_output_signature::Column;
pub type CCache = cache::Column;
pub type CCommit = commit::Column;
pub type CDirectBuild = direct_build::Column;
pub type CEvaluation = evaluation::Column;
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

pub type RApi = api::Relation;
pub type RBuild = build::Relation;
pub type RBuildDependency = build_dependency::Relation;
pub type RBuildFeature = build_feature::Relation;
pub type RBuildOutput = build_output::Relation;
pub type RBuildOutputSignature = build_output_signature::Relation;
pub type RCache = cache::Relation;
pub type RCommit = commit::Relation;
pub type RDirectBuild = direct_build::Relation;
pub type REvaluation = evaluation::Relation;
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
