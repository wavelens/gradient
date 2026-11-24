/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use uuid::Uuid;

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::EnumIter, sea_orm::DeriveActiveEnum)
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Admin,
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    Member,
}

impl TeamRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::EnumIter, sea_orm::DeriveActiveEnum)
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i16", db_type = "SmallInteger")
)]
#[serde(rename_all = "lowercase")]
pub enum TeamMemberSource {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    Api,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    State,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    #[serde(rename = "group")]
    ScimGroup,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 3)
    )]
    #[serde(rename = "sign_in_group")]
    SignInGroupSync,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamDemand {
    pub builds: Vec<UnmetRequirement>,
    pub network: u32,
    pub secrets: u32,
    pub eval: u32,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamGrantEntry {
    pub team: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
    pub pending: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct GrantProjectRequest {
    pub team: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub users: bool,
    #[serde(default)]
    pub workers: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct PatchProjectGrantRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub users: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<bool>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct GrantCacheRequest {
    pub team: String,
    pub role: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct PatchCacheGrantRequest {
    pub role: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamSummary {
    pub name: String,
    pub display_name: String,
    pub role: TeamRole,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamResponse {
    pub id: TeamId,
    pub name: String,
    pub display_name: String,
    pub managed: bool,
    pub role: Option<TeamRole>,
    pub oidc_group: Option<String>,
    pub scim_group: Option<String>,
    pub new_project_users: bool,
    pub new_project_workers: bool,
    pub new_project_role: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct MakeTeamRequest {
    pub name: TeamName,
    pub display_name: TeamDisplayName,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema, Default, PartialEq)]
pub struct PatchTeamRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<TeamDisplayName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_project_users: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_project_workers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_project_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oidc_group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scim_group: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamMemberItem {
    pub user: String,
    pub name: String,
    pub role: TeamRole,
    pub source: TeamMemberSource,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AddTeamMemberRequest {
    pub user: String,
    pub role: TeamRole,
    #[serde(default)]
    pub add_as_member_immediately: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamMemberRequest {
    pub user: String,
    pub role: TeamRole,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct RemoveTeamMemberRequest {
    pub user: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamProjectGrant {
    pub project: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamCacheGrant {
    pub cache: String,
    pub display_name: String,
    pub role: Option<String>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamGrants {
    pub projects: Vec<TeamProjectGrant>,
    pub caches: Vec<TeamCacheGrant>,
}

#[derive(Deserialize, Serialize, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RequestKind {
    Project,
    Cache,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamRequestItem {
    pub id: Uuid,
    pub kind: RequestKind,
    pub target: String,
    pub display_name: String,
    pub role: Option<String>,
    pub users: bool,
    pub workers: bool,
    pub requested_by: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamWorkerEntry {
    pub worker_id: String,
    pub display_name: String,
    pub registered_at: NaiveDateTime,
    pub active: bool,
    pub managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub gradient_ci: bool,
    pub enable_network: bool,
    pub enable_secrets: bool,
    pub enable_exec: bool,
    pub enable_eval: bool,
    pub enable_build: bool,
    #[serde(flatten)]
    pub connection: WorkerConnection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<WorkerLiveInfo>,
    pub requested_by_admin: bool,
}
