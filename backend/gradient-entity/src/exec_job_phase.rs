/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ExecJobAttemptId, ExecJobId, ExecJobPhaseId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_phase")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ExecJobPhaseId,
    pub exec_job: ExecJobId,
    pub attempt: ExecJobAttemptId,
    pub name: String,
    pub payload: Vec<u8>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::exec_job::Entity",
        from = "Column::ExecJob",
        to = "super::exec_job::Column::Id",
        on_delete = "Cascade"
    )]
    ExecJob,
    #[sea_orm(
        belongs_to = "super::exec_job_attempt::Entity",
        from = "Column::Attempt",
        to = "super::exec_job_attempt::Column::Id",
        on_delete = "Cascade"
    )]
    Attempt,
}

impl Related<super::exec_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExecJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
