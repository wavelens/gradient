/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::*;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, utoipa::ToSchema)]
pub struct InviteTokenRequest {
    pub token: String,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct PendingUserDecisionsResponse {
    pub invites: Vec<InviteItem>,
    pub subscription_requests: Vec<SubscriptionRequestItem>,
    pub team_requests: Vec<TeamGrantRequestInboxEntry>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct TeamGrantRequestInboxEntry {
    pub team: String,
    pub team_display_name: String,
    #[serde(flatten)]
    pub request: TeamRequestItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InviteKind {
    Project,
    Cache,
    Team,
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct InviteItem {
    pub kind: InviteKind,
    pub token: String,
    pub scope: String,
    pub scope_display_name: String,
    /// The role name for project and cache invitations. For team invitations this is the team role value, `admin` or `member`.
    pub role: String,
    pub invited_by: String,
    pub invited_by_display_name: Option<String>,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}
