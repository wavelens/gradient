/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{PullRequestSummaryId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "pull_request_summary")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: PullRequestSummaryId,
    pub task: TaskId,
    pub pr_number: i64,
    pub comment_id: Option<i64>,
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
}

impl ActiveModelBehavior for ActiveModel {}
