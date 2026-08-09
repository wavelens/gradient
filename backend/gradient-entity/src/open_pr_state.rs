/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! PR lifecycle for the `OpenPr` action. One row per
//! `(task, action, branch)` tracks the open PR so updates reuse the branch
//! instead of opening duplicates.

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OpenPrStateId, TaskActionId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "open_pr_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: OpenPrStateId,
    pub task: TaskId,
    pub action: TaskActionId,
    pub branch: String,
    pub forge_pr_number: Option<i64>,
    pub head_commit: Option<String>,
    /// PR lifecycle: `open` | `merged` | `closed`.
    pub status: String,
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
        belongs_to = "super::task_action::Entity",
        from = "Column::Action",
        to = "super::task_action::Column::Id",
        on_delete = "Cascade"
    )]
    Action,
}

impl ActiveModelBehavior for ActiveModel {}
