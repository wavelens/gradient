/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{FlakeInputOverrideId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task_flake_input_override")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: FlakeInputOverrideId,
    pub task: TaskId,
    pub input_name: String,
    pub url: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
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
