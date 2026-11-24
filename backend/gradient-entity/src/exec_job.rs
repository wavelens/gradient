/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, EntryPointId, EvaluationId, ExecJobId};
pub use gradient_api_types::ExecJobStatus;

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
    pub node_index: i16,
    pub keep_vm_running: bool,
    pub program_exit_code: Option<i32>,
    pub waits_for_manual_start: bool,
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
