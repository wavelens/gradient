/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{AuditLogId, UserId};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "audit_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: AuditLogId,
    pub user_id: Option<UserId>,
    pub event: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    #[sea_orm(column_type = "Json", nullable)]
    pub metadata: Option<serde_json::Value>,
    pub created_at: NaiveDateTime,
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

impl ActiveModelBehavior for ActiveModel {}
