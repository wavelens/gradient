/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::Iterable;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskActionId, TaskId, UserId};

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
    GitHostStatusReport = 2,
    #[sea_orm(num_value = 3)]
    OpenPr = 3,
    #[sea_orm(num_value = 4)]
    SendMatrixMessage = 4,
    #[sea_orm(num_value = 5)]
    SendSlackMessage = 5,
}

impl ActionType {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionType::SendMail => "send_mail",
            ActionType::SendWebRequest => "send_web_request",
            ActionType::GitHostStatusReport => "git_host_status_report",
            ActionType::OpenPr => "open_pr",
            ActionType::SendMatrixMessage => "send_matrix_message",
            ActionType::SendSlackMessage => "send_slack_message",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::iter().find(|t| t.as_str() == name)
    }

    pub fn secret_field(self) -> Option<&'static str> {
        match self {
            ActionType::SendWebRequest => Some("token"),
            ActionType::SendMatrixMessage => Some("access_token"),
            ActionType::SendSlackMessage => Some("webhook_url"),
            _ => None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_round_trip() {
        for t in ActionType::iter() {
            assert_eq!(ActionType::from_name(t.as_str()), Some(t));
        }
    }
}
