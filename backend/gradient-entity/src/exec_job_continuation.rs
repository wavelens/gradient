/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, ExecJobId};

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
pub enum ContinuationStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    Recorded = 0,
    #[sea_orm(num_value = 1)]
    Queued = 1,
    #[sea_orm(num_value = 2)]
    Done = 2,
    #[sea_orm(num_value = 3)]
    Failed = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "exec_job_continuation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub exec_job: ExecJobId,
    pub evaluation: EvaluationId,
    #[sea_orm(column_type = "JsonBinary")]
    pub attributes: Json,
    pub status: ContinuationStatus,
    pub message: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
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
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id",
        on_delete = "Cascade"
    )]
    Evaluation,
}

impl Related<super::exec_job::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ExecJob.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iterable;

    #[test]
    fn continuation_numbering_is_pinned() {
        for (status, n) in [
            (ContinuationStatus::Recorded, 0),
            (ContinuationStatus::Queued, 1),
            (ContinuationStatus::Done, 2),
            (ContinuationStatus::Failed, 3),
        ] {
            assert_eq!(i16::from(status), n);
        }
        assert_eq!(ContinuationStatus::iter().count(), 4);
    }
}
