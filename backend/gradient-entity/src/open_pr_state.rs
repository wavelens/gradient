/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! PR lifecycle for the `OpenPr` action. One row per
//! `(project, action, branch)` tracks the open PR so updates reuse the branch
//! instead of opening duplicates.

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OpenPrStateId, ProjectActionId, ProjectId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "open_pr_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: OpenPrStateId,
    pub project: ProjectId,
    pub action: ProjectActionId,
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
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::project_action::Entity",
        from = "Column::Action",
        to = "super::project_action::Column::Id",
        on_delete = "Cascade"
    )]
    Action,
}

impl ActiveModelBehavior for ActiveModel {}
