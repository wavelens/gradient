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
#[sea_orm(table_name = "webhook")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub url: String,
    #[sea_orm(column_type = "Text")]
    pub secret: String,
    #[sea_orm(column_type = "Json")]
    pub events: serde_json::Value,
    pub active: bool,
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
        f.debug_struct("Webhook")
            .field("id", &self.id)
            .field("organization", &self.organization)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("secret", &"[redacted]")
            .field("events", &self.events)
            .field("active", &self.active)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
