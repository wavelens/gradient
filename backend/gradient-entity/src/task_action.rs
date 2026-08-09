/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskActionId, TaskId, UserId};

/// What an action does when its trigger events fire. Tags the polymorphic
/// `config` jsonb column.
#[repr(i16)]
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
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum ActionType {
    #[default]
    #[sea_orm(num_value = 0)]
    SendMail = 0,
    #[sea_orm(num_value = 1)]
    SendWebRequest = 1,
    #[sea_orm(num_value = 2)]
    ForgeStatusReport = 2,
    #[sea_orm(num_value = 3)]
    OpenPr = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task_action")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: TaskActionId,
    pub task: TaskId,
    pub name: String,
    pub action_type: ActionType,
    pub config: Json,
    pub events: Json,
    pub active: bool,
    pub last_fired_at: Option<NaiveDateTime>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task::Entity",
        from = "Column::Task",
        to = "super::task::Column::Id",
        on_delete = "Cascade"
    )]
    Task,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(has_many = "super::task_action_delivery::Entity")]
    Deliveries,
}

impl Related<super::task_action_delivery::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deliveries.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
