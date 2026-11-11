/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GitHostConnectionId, ProjectId, TeamId, UserId};

#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "lowercase")]
pub enum GitHostType {
    #[default]
    #[sea_orm(num_value = 0)]
    Gitea = 0,
    #[sea_orm(num_value = 1)]
    Forgejo = 1,
    #[sea_orm(num_value = 2)]
    GitLab = 2,
    #[sea_orm(num_value = 3)]
    GitHub = 3,
}

impl GitHostType {
    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "gitea" => Some(Self::Gitea),
            "forgejo" => Some(Self::Forgejo),
            "gitlab" => Some(Self::GitLab),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }

    pub const fn as_path_segment(self) -> &'static str {
        match self {
            Self::Gitea => "gitea",
            Self::Forgejo => "forgejo",
            Self::GitLab => "gitlab",
            Self::GitHub => "github",
        }
    }
}

#[derive(Clone, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "git_host_connection")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: GitHostConnectionId,
    pub project: Option<ProjectId>,
    pub owner_team: Option<TeamId>,
    pub name: String,
    pub display_name: String,
    pub git_host_type: GitHostType,
    #[sea_orm(column_type = "Text", nullable)]
    pub secret: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub base_url: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub access_token: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub encrypted_refresh_token: Option<String>,
    pub access_token_expires_at: Option<NaiveDateTime>,
    /// Source CIDRs allowed for inbound webhooks. `None` or an empty list is allowing any source.
    #[sea_orm(column_type = "Array(std::sync::Arc::new(ColumnType::Text))", nullable)]
    pub allowed_ips: Option<Vec<String>>,
    pub receives_events: bool,
    pub reports_status: bool,
    pub github_installation_id: Option<i64>,
    #[sea_orm(column_type = "Text", nullable)]
    pub account_login: Option<String>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(
        belongs_to = "super::team::Entity",
        from = "Column::OwnerTeam",
        to = "super::team::Column::Id"
    )]
    OwnerTeam,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHostConnection")
            .field("id", &self.id)
            .field("project", &self.project)
            .field("owner_team", &self.owner_team)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("git_host_type", &self.git_host_type)
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .field("base_url", &self.base_url)
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "encrypted_refresh_token",
                &self.encrypted_refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("access_token_expires_at", &self.access_token_expires_at)
            .field("allowed_ips", &self.allowed_ips)
            .field("receives_events", &self.receives_events)
            .field("reports_status", &self.reports_status)
            .field("github_installation_id", &self.github_installation_id)
            .field("account_login", &self.account_login)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("managed", &self.managed)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
