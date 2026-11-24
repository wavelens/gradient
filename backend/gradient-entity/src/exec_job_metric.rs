/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ExecJobAttemptId, ExecJobId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_metric")]
pub struct Model {
    pub exec_job: ExecJobId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub attempt: ExecJobAttemptId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub sequence: i32,
    pub received_at: NaiveDateTime,
    pub window_ms: i32,
    pub cpu_avg: f32,
    pub cpu_max: f32,
    pub ram_avg_bytes: i64,
    pub ram_max_bytes: i64,
    pub ram_total_bytes: i64,
    pub disk_max_bytes: i64,
    pub disk_total_bytes: i64,
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
