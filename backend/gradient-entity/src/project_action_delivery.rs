/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectActionDeliveryId, ProjectActionId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_action_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: ProjectActionDeliveryId,
    pub action_id: ProjectActionId,
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
        belongs_to = "super::project_action::Entity",
        from = "Column::ActionId",
        to = "super::project_action::Column::Id",
        on_delete = "Cascade"
    )]
    Action,
}

impl Related<super::project_action::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Action.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
