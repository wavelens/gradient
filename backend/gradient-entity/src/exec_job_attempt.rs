/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DispatchedJobId, ExecJobAttemptId, ExecJobId};
pub use gradient_api_types::AttemptEndReason;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_attempt")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ExecJobAttemptId,
    pub exec_job: ExecJobId,
    pub assignment_id: DispatchedJobId,
    pub worker: String,
    pub number: i32,
    pub started_at: NaiveDateTime,
    pub program_started_at: Option<NaiveDateTime>,
    pub ended_at: Option<NaiveDateTime>,
    pub end_reason: Option<AttemptEndReason>,
    pub message: Option<String>,
    pub master_port: Option<i32>,
    pub guest_address: Option<String>,
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
}

impl Related<super::exec_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExecJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
