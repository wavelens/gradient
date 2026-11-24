/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskActionDeliveryId, TaskActionId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task_action_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: TaskActionDeliveryId,
    pub action_id: TaskActionId,
    pub event: String,
    #[sea_orm(column_type = "Text")]
    pub request_body: String,
    pub response_status: Option<i32>,
    #[sea_orm(column_type = "Text", nullable)]
    pub response_body: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub error_message: Option<String>,
    pub success: bool,
    pub duration_ms: i32,
    pub delivered_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task_action::Entity",
        from = "Column::ActionId",
        to = "super::task_action::Column::Id",
        on_delete = "Cascade"
    )]
    Action,
}

impl Related<super::task_action::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Action.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for gradient_api_types::DeliveryListItem<TaskActionDeliveryId> {
    fn from(r: Model) -> Self {
        Self {
            id: r.id,
            event: r.event,
            success: r.success,
            response_status: r.response_status,
            error_message: r.error_message,
            duration_ms: r.duration_ms,
            delivered_at: r.delivered_at,
        }
    }
}

impl From<Model> for gradient_api_types::DeliveryDetail<TaskActionDeliveryId> {
    fn from(r: Model) -> Self {
        Self {
            request_body: r.request_body.clone(),
            response_body: r.response_body.clone(),
            item: r.into(),
        }
    }
}
