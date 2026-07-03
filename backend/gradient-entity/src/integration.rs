/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GithubInstallationId, IntegrationId, OrganizationId, UserId};

/// Webhook direction of an integration: receives forge events (inbound) or
/// reports statuses / opens PRs on the forge (outbound).
#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum IntegrationKind {
    #[default]
    #[sea_orm(num_value = 0)]
    Inbound = 0,
    #[sea_orm(num_value = 1)]
    Outbound = 1,
}

/// The forge identity shared by `gradient-forge` providers, `ci` integration
/// lookups, and state export.
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
pub enum ForgeType {
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

impl ForgeType {
    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "gitea" => Some(Self::Gitea),
            "forgejo" => Some(Self::Forgejo),
            "gitlab" => Some(Self::GitLab),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }

    /// Inverse of [`from_path_segment`](Self::from_path_segment): the canonical
    /// path/state segment naming this forge.
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
#[sea_orm(table_name = "integration")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: IntegrationId,
    pub organization: OrganizationId,
    pub name: String,
    /// Human-readable display name for this integration.
    pub display_name: String,
    pub kind: IntegrationKind,
    pub forge_type: ForgeType,
    #[sea_orm(column_type = "Text", nullable)]
    pub secret: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub endpoint_url: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub access_token: Option<String>,
    /// Source CIDRs allowed for inbound webhooks. `None`/empty = any source.
    #[sea_orm(column_type = "Array(std::sync::Arc::new(ColumnType::Text))", nullable)]
    pub allowed_ips: Option<Vec<String>>,
    pub github_installation: Option<GithubInstallationId>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id"
    )]
    Organization,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(
        belongs_to = "super::github_installation::Entity",
        from = "Column::GithubInstallation",
        to = "super::github_installation::Column::Id"
    )]
    GithubInstallation,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integration")
            .field("id", &self.id)
            .field("organization", &self.organization)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("kind", &self.kind)
            .field("forge_type", &self.forge_type)
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .field("endpoint_url", &self.endpoint_url)
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[redacted]"),
            )
            .field("allowed_ips", &self.allowed_ips)
            .field("github_installation", &self.github_installation)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
