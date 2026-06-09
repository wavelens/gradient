/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CliDeviceAuthorizationId, UserId};

#[derive(Clone, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cli_device_authorization")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CliDeviceAuthorizationId,
    pub device_code_hash: String,
    pub user_code: String,
    pub user_id: Option<UserId>,
    pub token: Option<String>,
    pub denied_at: Option<NaiveDateTime>,
    pub authorized_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
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
        f.debug_struct("CliDeviceAuthorization")
            .field("id", &self.id)
            .field("device_code_hash", &"[redacted]")
            .field("user_code", &self.user_code)
            .field("user_id", &self.user_id)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .field("denied_at", &self.denied_at)
            .field("authorized_at", &self.authorized_at)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("user_agent", &self.user_agent)
            .field("ip", &self.ip)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
