/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DispatchedJobId, ExecJobAttemptId, ExecJobId};

#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum AttemptEndReason {
    #[sea_orm(num_value = 0)]
    Completed = 0,
    #[sea_orm(num_value = 1)]
    Failed = 1,
    #[sea_orm(num_value = 2)]
    Timeout = 2,
    #[sea_orm(num_value = 3)]
    Lost = 3,
    #[sea_orm(num_value = 4)]
    Aborted = 4,
}

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
    pub ended_at: Option<NaiveDateTime>,
    pub end_reason: Option<AttemptEndReason>,
    pub message: Option<String>,
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
