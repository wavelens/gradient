/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectActionId, ProjectId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_action")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: ProjectActionId,
    pub project: ProjectId,
    pub name: String,
    /// Discriminant matching `ActionType` enum: 0 = webhook, 1 = email.
    pub action_type: i16,
    pub config: serde_json::Value,
    pub events: serde_json::Value,
    pub active: bool,
    pub last_fired_at: Option<NaiveDateTime>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(has_many = "super::project_action_delivery::Entity")]
    Deliveries,
}

impl Related<super::project_action_delivery::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deliveries.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
