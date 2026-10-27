/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, EntryPointId, EvaluationId, ExecJobId};

#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
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
pub enum ExecJobStatus {
    #[sea_orm(num_value = 0)]
    Queued = 0,
    #[sea_orm(num_value = 1)]
    Starting = 1,
    #[sea_orm(num_value = 2)]
    Running = 2,
    #[sea_orm(num_value = 3)]
    Completed = 3,
    #[sea_orm(num_value = 4)]
    Failed = 4,
    #[sea_orm(num_value = 5)]
    Aborted = 5,
    #[default]
    #[sea_orm(num_value = 6)]
    Created = 6,
}

impl ExecJobStatus {
    pub const OPEN: [Self; 4] = [Self::Created, Self::Queued, Self::Starting, Self::Running];

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ExecJobId,
    pub evaluation: EvaluationId,
    pub entry_point: EntryPointId,
    pub program_derivation: DerivationId,
    pub image_derivation: Option<DerivationId>,
    #[sea_orm(column_type = "JsonBinary")]
    pub requirement: Json,
    #[sea_orm(column_type = "JsonBinary")]
    pub spec: Json,
    pub retries_left: i32,
    pub status: ExecJobStatus,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub waiting_reason: Option<Json>,
    pub message: Option<String>,
    pub created_at: NaiveDateTime,
    pub started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id",
        on_delete = "Cascade"
    )]
    Evaluation,
    #[sea_orm(
        belongs_to = "super::entry_point::Entity",
        from = "Column::EntryPoint",
        to = "super::entry_point::Column::Id",
        on_delete = "Cascade"
    )]
    EntryPoint,
    #[sea_orm(has_many = "super::exec_job_attempt::Entity")]
    Attempt,
    #[sea_orm(has_many = "super::exec_job_phase::Entity")]
    Phase,
}

impl Related<super::exec_job_attempt::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Attempt.def()
    }
}

impl Related<super::exec_job_phase::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Phase.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iterable;

    #[test]
    fn only_completed_failed_and_aborted_are_terminal() {
        for status in ExecJobStatus::iter() {
            assert_eq!(
                status.is_terminal(),
                !ExecJobStatus::OPEN.contains(&status),
                "{status:?}"
            );
        }
    }
}
