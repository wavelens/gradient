/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{AdminTaskId, UserId};

#[repr(i32)]
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
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AdminTaskKind {
    #[default]
    #[sea_orm(num_value = 0)]
    DeepGc = 0,
}

impl AdminTaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeepGc => "deep_gc",
        }
    }
}

#[repr(i32)]
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
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum AdminTaskStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    Pending = 0,
    #[sea_orm(num_value = 1)]
    Running = 1,
    #[sea_orm(num_value = 2)]
    Completed = 2,
    #[sea_orm(num_value = 3)]
    Failed = 3,
}

impl AdminTaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub const fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "admin_task")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: AdminTaskId,
    pub kind: AdminTaskKind,
    pub status: AdminTaskStatus,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
    pub progress: Option<Json>,
    pub error: Option<String>,
    pub created_by: Option<UserId>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id",
        on_delete = "SetNull"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
