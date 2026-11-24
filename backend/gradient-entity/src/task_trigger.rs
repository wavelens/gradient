/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskId, TaskTriggerId};
pub use gradient_api_types::TriggerType;

#[derive(Clone, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task_trigger")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: TaskTriggerId,
    pub task: TaskId,
    pub trigger_type: TriggerType,
    pub config: Json,
    pub active: bool,
    pub is_default: bool,
    pub last_fired_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub encrypted_secret: Option<String>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskTrigger")
            .field("id", &self.id)
            .field("task", &self.task)
            .field("trigger_type", &self.trigger_type)
            .field("config", &self.config)
            .field("active", &self.active)
            .field("is_default", &self.is_default)
            .field("last_fired_at", &self.last_fired_at)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field(
                "encrypted_secret",
                &self.encrypted_secret.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task::Entity",
        from = "Column::Task",
        to = "super::task::Column::Id"
    )]
    Task,
}

impl ActiveModelBehavior for ActiveModel {}
