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
#[sea_orm(table_name = "organization")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    #[sea_orm(unique, indexed)]
    pub name: String,
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub public_key: String,
    pub private_key: String,
    pub public: bool,
    pub created_by: Uuid,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    /// GitHub App installation ID for this organization.
    /// Set automatically when the GitHub App is installed on the org's GitHub account.
    pub github_installation_id: Option<i64>,
    /// Whether this org accepts GitHub App-delivered events. Only meaningful
    /// when the server has a GitHub App configured. Defaults to `false` so
    /// admins must explicitly opt in.
    pub github_app_enabled: bool,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Organization")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("description", &self.description)
            .field("public_key", &format!("{} {}", self.public_key, self.id))
            .field("private_key", &"[redacted]")
            .field("public", &self.public)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("github_installation_id", &self.github_installation_id)
            .field("github_app_enabled", &self.github_app_enabled)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
}

impl ActiveModelBehavior for ActiveModel {}
