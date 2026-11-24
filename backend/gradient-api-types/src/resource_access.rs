/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamNameAndDisplayName {
    pub name: String,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessReason {
    Direct,
    Team,
    Owner,
    Superuser,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PersonAccessReason {
    pub kind: AccessReason,
    pub team: Option<TeamNameAndDisplayName>,
    pub role: Option<String>,
    pub applies: bool,
    pub managed: bool,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct PersonWithAccessToProjectOrCache {
    pub user: String,
    pub user_display_name: String,
    pub role: Option<String>,
    pub permissions: Vec<String>,
    pub reasons: Vec<PersonAccessReason>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct UserAccessibleProjectOrCacheRow {
    pub name: String,
    pub display_name: String,
    pub owner_team: Option<TeamNameAndDisplayName>,
    pub role: Option<String>,
    pub reasons: Vec<PersonAccessReason>,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct UserTeamMembership {
    pub name: String,
    pub display_name: String,
    pub role: TeamRole,
    pub source: TeamMemberSource,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct UserAccessSummary {
    pub superuser: bool,
    pub projects: Vec<UserAccessibleProjectOrCacheRow>,
    pub caches: Vec<UserAccessibleProjectOrCacheRow>,
    pub teams: Vec<UserTeamMembership>,
}
