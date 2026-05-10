/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ApiId, OrganizationId, UserId};

#[derive(Clone, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "api")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ApiId,
    pub owned_by: UserId,
    pub name: String,
    pub key: String,
    pub last_used_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub expires_at: Option<NaiveDateTime>,
    pub revoked_at: Option<NaiveDateTime>,
    /// Bitmask over `gradient_core::permissions::Permission` capabilities;
    /// caps the key's effective authority on every authenticated request.
    pub permission: i64,
    /// Optional org pin. `None` = key works in any org the owning user is a
    /// member of (legacy behavior). `Some(id)` = key is rejected for any
    /// other org.
    pub organization: Option<OrganizationId>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnedBy",
        to = "super::user::Column::Id"
    )]
    OwnedBy,
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id",
        on_delete = "SetNull",
        on_update = "Cascade"
    )]
    Organization,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("id", &self.id)
            .field("owned_by", &self.owned_by)
            .field("name", &self.name)
            .field("key", &"[redacted]")
            .field("last_used_at", &self.last_used_at)
            .field("created_at", &self.created_at)
            .field("managed", &self.managed)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .field("permission", &self.permission)
            .field("organization", &self.organization)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
