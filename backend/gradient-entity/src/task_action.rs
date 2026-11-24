/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskActionId, TaskId, UserId};
pub use gradient_api_types::ActionType;

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

impl From<Model> for gradient_api_types::ActionResponse {
    fn from(m: Model) -> Self {
        let at = m.action_type;
        let mut config = m.config;
        if let Some(field) = at.secret_field()
            && let Some(obj) = config.as_object_mut()
        {
            obj.remove(field);
        }
        let events = m
            .events
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            id: m.id,
            name: m.name,
            action_type: at.as_str().into(),
            config,
            events,
            active: m.active,
            last_fired_at: m.last_fired_at,
            last_delivery: None,
            created_by: m.created_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iterable;

    #[test]
    fn type_names_round_trip() {
        for t in ActionType::iter() {
            assert_eq!(ActionType::from_name(t.as_str()), Some(t));
        }
    }
}
