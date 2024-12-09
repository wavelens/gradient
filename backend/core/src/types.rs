/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use super::input::{greater_than_zero, port_in_range};
use async_ssh2_lite::{AsyncChannel, TokioTcpStream};
use clap::Parser;
use entity::*;
use nix_daemon::nix::DaemonStore;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient-server", author = "Wavelens", version, about, long_about = None)]
pub struct Cli {
    #[arg(long, env = "GRADIENT_DEBUG", default_value = "false")]
    pub debug: bool,
    #[arg(long, env = "GRADIENT_IP", default_value = "127.0.0.1")]
    pub ip: String,
    #[arg(long, env = "GRADIENT_PORT", value_parser = port_in_range, default_value_t = 3000)]
    pub port: u16,
    #[arg(long, env = "GRADIENT_DATABASE_URL")]
    pub database_uri: String,
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_EVALUATIONS", value_parser = greater_than_zero::<usize>, default_value = "10")]
    pub max_concurrent_evaluations: usize,
    #[arg(long, env = "GRADIENT_MAX_CONCURRENT_BUILDS", value_parser = greater_than_zero::<usize>, default_value = "1000")]
    pub max_concurrent_builds: usize,
    #[arg(long, env = "GRADIENT_EVALUATION_TIMEOUT", value_parser = greater_than_zero::<i64>, default_value = "10")]
    pub evaluation_timeout: i64,
    #[arg(long, env = "GRADIENT_STORE_PATH")]
    pub store_path: Option<String>,
    #[arg(long, env = "GRADIENT_OAUTH_ENABLED", default_value = "false")]
    pub oauth_enabled: bool,
    #[arg(long, env = "GRADIENT_OAUTH_CLIENT_ID")]
    pub oauth_client_id: Option<String>,
    #[arg(long, env = "GRADIENT_OAUTH_CLIENT_SECRET")]
    pub oauth_client_secret: Option<String>,
    #[arg(long, env = "GRADIENT_OAUTH_AUTH_URL")]
    pub oauth_auth_url: Option<String>,
    #[arg(long, env = "GRADIENT_OAUTH_TOKEN_URL")]
    pub oauth_token_url: Option<String>,
    #[arg(long, env = "GRADIENT_OAUTH_REDIRECT_URL")]
    pub oauth_redirect_url: Option<String>,
    #[arg(long, env = "GRADIENT_JWT_SECRET")]
    pub jwt_secret: String,
    #[arg(long, env = "GRADIENT_CRYPT_SECRET")]
    pub crypt_secret: String,
    #[arg(long, env = "GRADIENT_BINPATH_NIX", default_value = "nix")]
    pub binpath_nix: String,
    #[arg(long, env = "GRADIENT_BINPATH_GIT", default_value = "git")]
    pub binpath_git: String,
}

#[derive(Debug)]
pub struct ServerState {
    pub db: DatabaseConnection,
    pub cli: Cli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListItem {
    pub id: Uuid,
    pub name: String,
}

pub type ListResponse = Vec<ListItem>;
pub type NixStore = DaemonStore<AsyncChannel<TokioTcpStream>>;

pub type EApi = api::Entity;
pub type EBuild = build::Entity;
pub type EBuildDependency = build_dependency::Entity;
pub type EBuildFeature = build_feature::Entity;
pub type ECommit = commit::Entity;
pub type EEvaluation = evaluation::Entity;
pub type EFeature = feature::Entity;
pub type EOrganization = organization::Entity;
pub type EProject = project::Entity;
pub type EServer = server::Entity;
pub type EServerArchitecture = server_architecture::Entity;
pub type EServerFeature = server_feature::Entity;
pub type EUser = user::Entity;

pub type MApi = api::Model;
pub type MBuild = build::Model;
pub type MBuildDependency = build_dependency::Model;
pub type MBuildFeature = build_feature::Model;
pub type MCommit = commit::Model;
pub type MEvaluation = evaluation::Model;
pub type MFeature = feature::Model;
pub type MOrganization = organization::Model;
pub type MProject = project::Model;
pub type MServer = server::Model;
pub type MServerArchitecture = server_architecture::Model;
pub type MServerFeature = server_feature::Model;
pub type MUser = user::Model;

pub type AApi = api::ActiveModel;
pub type ABuild = build::ActiveModel;
pub type ABuildDependency = build_dependency::ActiveModel;
pub type ABuildFeature = build_feature::ActiveModel;
pub type ACommit = commit::ActiveModel;
pub type AEvaluation = evaluation::ActiveModel;
pub type AFeature = feature::ActiveModel;
pub type AOrganization = organization::ActiveModel;
pub type AProject = project::ActiveModel;
pub type AServer = server::ActiveModel;
pub type AServerArchitecture = server_architecture::ActiveModel;
pub type AServerFeature = server_feature::ActiveModel;
pub type AUser = user::ActiveModel;

pub type CApi = api::Column;
pub type CBuild = build::Column;
pub type CBuildDependency = build_dependency::Column;
pub type CBuildFeature = build_feature::Column;
pub type CCommit = commit::Column;
pub type CEvaluation = evaluation::Column;
pub type CFeature = feature::Column;
pub type COrganization = organization::Column;
pub type CProject = project::Column;
pub type CServer = server::Column;
pub type CServerArchitecture = server_architecture::Column;
pub type CServerFeature = server_feature::Column;
pub type CUser = user::Column;

pub type RApi = api::Relation;
pub type RBuild = build::Relation;
pub type RBuildDependency = build_dependency::Relation;
pub type RBuildFeature = build_feature::Relation;
pub type RCommit = commit::Relation;
pub type REvaluation = evaluation::Relation;
pub type RFeature = feature::Relation;
pub type ROrganization = organization::Relation;
pub type RProject = project::Relation;
pub type RServer = server::Relation;
pub type RServerArchitecture = server_architecture::Relation;
pub type RServerFeature = server_feature::Relation;
pub type RUser = user::Relation;