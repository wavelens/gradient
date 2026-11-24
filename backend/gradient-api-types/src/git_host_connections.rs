/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;

use serde::{Deserialize, Serialize};

#[repr(i16)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryResult {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Queued = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Skipped = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    NoMatchingTask = 2,
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    Ignored = 3,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 4)
    )]
    Refused = 4,
}

#[repr(i16)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(
        sea_orm::DeriveActiveEnum,
        sea_orm::EnumIter,
        num_enum::IntoPrimitive,
        num_enum::TryFromPrimitive
    )
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryGrantSource {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Admin = 0,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Upgrade = 1,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    StateFile = 2,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GitHostConnectionOwner {
    Instance,
    Team { name: String },
    Project { name: String },
}

impl std::str::FromStr for GitHostConnectionOwner {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            None if s == "instance" => Ok(Self::Instance),
            Some(("team", name)) if !name.is_empty() => Ok(Self::Team { name: name.into() }),
            Some(("project", name)) if !name.is_empty() => Ok(Self::Project { name: name.into() }),
            _ => Err(format!(
                "Set owner `{s}` to `instance`, `team/<name>`, or `project/<name>`."
            )),
        }
    }
}

impl std::fmt::Display for GitHostConnectionOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Instance => f.write_str("instance"),
            Self::Team { name } => write!(f, "team/{name}"),
            Self::Project { name } => write!(f, "project/{name}"),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionResponse {
    #[serde(flatten)]
    pub connection: GitHostConnectionSummary,
    pub has_secret: bool,
    pub has_access_token: bool,
    pub allowed_ips: Vec<String>,
    pub installation_id: Option<i64>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub generated_secret: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct GitHostConnectionSummary {
    pub id: GitHostConnectionId,
    pub owner: GitHostConnectionOwner,
    pub name: String,
    pub display_name: String,
    pub git_host_type: String,
    pub base_url: Option<String>,
    pub receives_events: bool,
    pub reports_status: bool,
    #[serde(default)]
    #[schema(required = false, default = false)]
    pub can_manage: bool,
    #[serde(default)]
    #[schema(required = false, default = false)]
    pub managed: bool,
    pub account_login: Option<String>,
    pub github_installation_id: Option<i64>,
    pub webhook_url: Option<String>,
    pub access_token_expires_at: Option<chrono::NaiveDateTime>,
    #[serde(default)]
    #[schema(required = false, default = false)]
    pub access_token_refreshes_automatically: bool,
    pub last_delivery: Option<IncomingWebhookDeliveryResponse>,
    #[serde(default)]
    #[schema(required = false, default = 0)]
    pub refused_signature_count: i64,
    pub last_refused_signature_at: Option<chrono::NaiveDateTime>,
    pub last_status_post: Option<TaskCommitStatusPostAttempt>,
    #[serde(default)]
    #[schema(required = false, default = 0)]
    pub task_count: u64,
    pub repository_fit: Option<RepositoryFit>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, utoipa::ToSchema)]
pub struct RepositoryFit {
    pub fits: bool,
    pub can_be_selected: bool,
    pub reason: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionTaskResponse {
    pub id: TaskId,
    pub project: String,
    pub name: String,
    pub display_name: String,
    pub repository: String,
    pub report_status: bool,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct GitHostConnectionTestResponse {
    pub success: bool,
    pub account: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct IncomingWebhookDeliveryResponse {
    pub id: IncomingWebhookDeliveryId,
    pub event: String,
    pub status_code: i16,
    pub result: DeliveryResult,
    pub detail: Option<String>,
    pub repository: Option<String>,
    pub tasks_scanned: i32,
    pub queued: i32,
    pub created_at: chrono::NaiveDateTime,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateGitHostConnectionRequest {
    pub name: GitHostConnectionName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub git_host_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default = "default_true")]
    pub receives_events: bool,
    #[serde(default)]
    pub reports_status: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_ips: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<i64>,
}

#[derive(Deserialize, Serialize, Debug, Default, utoipa::ToSchema)]
pub struct PatchGitHostConnectionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<GitHostConnectionName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_host_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_ips: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receives_events: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reports_status: Option<bool>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateRepositoryGrantRequest {
    pub project: String,
    pub repository_owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_name: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct RepositoryGrantResponse {
    pub id: GitHostConnectionRepositoryGrantId,
    pub project: ProjectId,
    pub project_name: Option<String>,
    pub repository_owner: String,
    pub repository_name: Option<String>,
    #[schema(value_type = String)]
    pub source: RepositoryGrantSource,
    pub created_by: Option<UserId>,
    pub created_at: chrono::NaiveDateTime,
}
