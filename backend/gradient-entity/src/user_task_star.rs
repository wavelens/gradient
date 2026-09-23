/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TaskId, UserId};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "user_task_star")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user: UserId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub task: TaskId,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::User",
        to = "super::user::Column::Id"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::task::Entity",
        from = "Column::Task",
        to = "super::task::Column::Id"
    )]
    Task,
}

impl ActiveModelBehavior for ActiveModel {}
