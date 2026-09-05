/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, CacheInvitationId, RoleId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_invitation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CacheInvitationId,
    pub cache: CacheId,
    pub user: UserId,
    pub role: RoleId,
    pub invited_by: UserId,
    #[sea_orm(unique, indexed)]
    pub token: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Cache,
    User,
    Role,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Cache => Entity::belongs_to(super::cache::Entity)
                .from(Column::Cache)
                .to(super::cache::Column::Id)
                .into(),
            Self::User => Entity::belongs_to(super::user::Entity)
                .from(Column::User)
                .to(super::user::Column::Id)
                .into(),
            Self::Role => Entity::belongs_to(super::cache_role::Entity)
                .from(Column::Role)
                .to(super::cache_role::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
