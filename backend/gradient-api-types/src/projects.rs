/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use utoipa::ToSchema;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    derive(sea_orm::DeriveActiveEnum, sea_orm::EnumIter)
)]
#[cfg_attr(
    feature = "server-only-sea-orm-and-trait-derives",
    sea_orm(rs_type = "i32", db_type = "Integer")
)]
pub enum CacheSubscriptionMode {
    #[default]
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 0)
    )]
    ReadWrite,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 1)
    )]
    ReadOnly,
    #[cfg_attr(
        feature = "server-only-sea-orm-and-trait-derives",
        sea_orm(num_value = 2)
    )]
    WriteOnly,
}

impl CacheSubscriptionMode {
    pub fn writes_build_outputs_to_cache(&self) -> bool {
        matches!(self, Self::ReadWrite | Self::WriteOnly)
    }

    pub fn grants_no_access_beyond(&self, other: &Self) -> bool {
        self == other || *other == Self::ReadWrite
    }
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct MakeProjectRequest {
    pub name: ProjectName,
    pub display_name: ProjectDisplayName,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_build_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_team: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct PatchProjectRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<ProjectName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_build_requests: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_exec_in_build_requests: Option<bool>,
    /// Omitted fields leave the owner team unchanged. A team name can set the owner team. JSON null can clear the owner team.
    #[serde(
        default,
        deserialize_with = "crate::deserialize_optional_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_team: Option<Option<String>>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema, Debug)]
pub struct ProjectSummary {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub hide_build_requests: bool,
    pub allow_exec_in_build_requests: bool,
    pub managed: bool,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub running_evaluations: i64,
    pub waiting_evaluations: i64,
    pub role: Option<String>,
    pub permissions: Vec<Permission>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectResponse {
    pub id: ProjectId,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub public_key: Option<String>,
    pub public: bool,
    pub hide_build_requests: bool,
    pub allow_exec_in_build_requests: bool,
    pub managed: bool,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub created_by: UserId,
    pub created_at: chrono::NaiveDateTime,
    pub role: Option<String>,
    pub permissions: Vec<Permission>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectReadinessResponse {
    pub ready: bool,
    pub workers: ProjectReadinessWorkers,
    pub cache: ProjectSubscriptionReadinessCache,
    pub repository: GitAccessReadiness,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectReadinessWorkers {
    pub ready: bool,
    pub registered: usize,
    pub teams: Vec<TeamReadinessEntry>,
    pub teams_granted_without_workers: Vec<TeamReadinessEntry>,
    pub teams_you_can_grant: Vec<TeamReadinessEntry>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamReadinessEntry {
    pub name: String,
    pub display_name: String,
    pub workers: usize,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectSubscriptionReadinessCache {
    pub ready: bool,
    pub subscriptions: Vec<CacheSubscriptionItem>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct GitAccessReadiness {
    pub status: TaskRepositoryFetchStatus,
    pub tasks: Vec<ProjectTaskReadinessEntry>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProjectTaskReadinessEntry {
    pub name: String,
    pub display_name: String,
    pub status: TaskRepositoryFetchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(
    Deserialize, Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskRepositoryFetchStatus {
    Verified,
    Unchecked,
    Failing,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct ProjectSecretSummary {
    pub name: String,
    pub updated_at: chrono::NaiveDateTime,
    pub updated_by: Option<String>,
    pub managed: bool,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct PutProjectSecretRequest {
    pub value: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct SopsRecipientResponse {
    pub age: Option<String>,
    pub ssh: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SubscribeCacheRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<CacheSubscriptionMode>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionStatus {
    Active,
    Pending,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct CacheSubscriptionItem {
    pub id: CacheId,
    pub name: String,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    pub status: SubscriptionStatus,
    pub managed: bool,
    pub sign_outputs: bool,
    /// Usernames of the users that can approve a pending request. Empty for an active subscription.
    pub approvers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
pub struct Commit {
    pub id: CommitId,
    pub message: String,
    pub hash: Vec<u8>,
    pub author: Option<UserId>,
    pub author_name: String,
    pub project: Option<ProjectId>,
    pub fetched_at: Option<chrono::NaiveDateTime>,
}
