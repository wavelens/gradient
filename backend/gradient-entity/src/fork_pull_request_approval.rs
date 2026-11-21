/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{PullRequestApprovalId, TaskId, TaskTriggerId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "fork_pull_request_approval")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: PullRequestApprovalId,
    pub task: TaskId,
    pub trigger: TaskTriggerId,
    pub pr_number: i64,
    pub pr_author: String,
    pub commit_hash: Vec<u8>,
    pub head_repo_clone_url: Option<String>,
    pub pr_base_branch_name: Option<String>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    pub check_run_id: Option<i64>,
    pub approving_maintainer_login: Option<String>,
    pub approved_at: Option<DateTime>,
    pub created_at: DateTime,
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
        belongs_to = "super::task_trigger::Entity",
        from = "Column::Trigger",
        to = "super::task_trigger::Column::Id",
        on_delete = "Cascade"
    )]
    Trigger,
}

impl ActiveModelBehavior for ActiveModel {}
