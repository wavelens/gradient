/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, UserId};

#[derive(Clone, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: SessionId,
    pub user_id: UserId,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub last_used_at: NaiveDateTime,
    pub revoked_at: Option<NaiveDateTime>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub remember_me: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("last_used_at", &self.last_used_at)
            .field("revoked_at", &self.revoked_at)
            .field("user_agent", &self.user_agent)
            .field("ip", &self.ip)
            .field("remember_me", &self.remember_me)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
