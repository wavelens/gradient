/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{RunnerJobAttemptId, RunnerJobId, RunnerJobPhaseId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "runner_job_phase")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: RunnerJobPhaseId,
    pub runner_job: RunnerJobId,
    pub attempt: RunnerJobAttemptId,
    pub name: String,
    pub payload: Vec<u8>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::runner_job::Entity",
        from = "Column::RunnerJob",
        to = "super::runner_job::Column::Id",
        on_delete = "Cascade"
    )]
    RunnerJob,
    #[sea_orm(
        belongs_to = "super::runner_job_attempt::Entity",
        from = "Column::Attempt",
        to = "super::runner_job_attempt::Column::Id",
        on_delete = "Cascade"
    )]
    Attempt,
}

impl Related<super::runner_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::RunnerJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
