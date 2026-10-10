/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, UserId};

#[derive(Clone, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CacheId,
    #[sea_orm(unique, indexed)]
    pub name: String,
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub active: bool,
    pub priority: i32,
    pub local_priority: Option<i32>,
    pub public_key: String,
    #[serde(skip_serializing)]
    pub private_key: String,
    pub public: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    #[sea_orm(default_value = "0")]
    pub max_storage_gb: i32,
    pub pull_through: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            id: Default::default(),
            name: Default::default(),
            display_name: Default::default(),
            description: Default::default(),
            active: false,
            priority: 0,
            local_priority: None,
            public_key: Default::default(),
            private_key: Default::default(),
            public: false,
            created_by: Default::default(),
            created_at: Default::default(),
            managed: false,
            max_storage_gb: 0,
            pull_through: true,
        }
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("description", &self.description)
            .field("active", &self.active)
            .field("priority", &self.priority)
            .field("local_priority", &self.local_priority)
            .field("public_key", &self.public_key)
            .field("private_key", &"[redacted]")
            .field("public", &self.public)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("managed", &self.managed)
            .field("max_storage_gb", &self.max_storage_gb)
            .field("pull_through", &self.pull_through)
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
