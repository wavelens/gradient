/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PendingInvitationItem {
    pub user: String,
    pub name: String,
    pub role: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct RevokeInvitationRequest {
    pub user: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MemberItem {
    pub id: String,
    pub user_display_name: Option<String>,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct AddMemberRequest {
    pub user: String,
    pub role: String,
    #[serde(default)]
    pub add_as_member_immediately: bool,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct ChangeMemberRoleRequest {
    pub user: String,
    pub role: String,
}

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct RemoveMemberRequest {
    pub user: String,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct RoleResponse<F> {
    pub id: RoleId,
    pub name: String,
    #[serde(flatten)]
    pub owner: F,
    pub builtin: bool,
    pub managed: bool,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct RoleListResponse<F> {
    pub roles: Vec<RoleResponse<F>>,
    pub available_permissions: Vec<PermissionEntry>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct CreateRoleRequest {
    pub name: RoleName,
    pub permissions: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, utoipa::ToSchema)]
pub struct PatchRoleRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<RoleName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct ProjectRoleOwner {
    pub project: Option<ProjectId>,
}

impl From<Option<ProjectId>> for ProjectRoleOwner {
    fn from(project: Option<ProjectId>) -> Self {
        Self { project }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, utoipa::ToSchema)]
pub struct CacheRoleOwner {
    pub cache: Option<CacheId>,
}

impl From<Option<CacheId>> for CacheRoleOwner {
    fn from(cache: Option<CacheId>) -> Self {
        Self { cache }
    }
}
