/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ExecJobAttemptId, ExecJobTokenId, ProjectId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_token")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ExecJobTokenId,
    pub attempt: ExecJobAttemptId,
    #[sea_orm(unique)]
    pub token_hash: String,
    pub project: ProjectId,
    pub task: TaskId,
    pub expires_at: NaiveDateTime,
    pub revoked: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::exec_job_attempt::Entity",
        from = "Column::Attempt",
        to = "super::exec_job_attempt::Column::Id",
        on_delete = "Cascade"
    )]
    Attempt,
}

impl Related<super::exec_job_attempt::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Attempt.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
