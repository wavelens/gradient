/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "integration")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    /// Human-readable display name for this integration.
    pub display_name: String,
    /// 0 = inbound, 1 = outbound
    pub kind: i16,
    /// 0 = gitea, 1 = forgejo, 2 = gitlab, 3 = github
    pub forge_type: i16,
    #[sea_orm(column_type = "Text", nullable)]
    pub secret: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub endpoint_url: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub access_token: Option<String>,
    pub created_by: Uuid,
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
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
