/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GitHostConnectionId, TaskCommitStatusPostAttemptId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "task_commit_status_post_attempt")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: TaskCommitStatusPostAttemptId,
    pub task: TaskId,
    pub git_host_connection: Option<GitHostConnectionId>,
    pub event: String,
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
        belongs_to = "super::task::Entity",
        from = "Column::Task",
        to = "super::task::Column::Id",
        on_delete = "Cascade"
    )]
    Task,
    #[sea_orm(
        belongs_to = "super::git_host_connection::Entity",
        from = "Column::GitHostConnection",
        to = "super::git_host_connection::Column::Id",
        on_delete = "SetNull"
    )]
    GitHostConnection,
}

impl ActiveModelBehavior for ActiveModel {}
