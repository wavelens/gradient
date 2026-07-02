/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CommitId, EvaluationId, ProjectId, ProjectTriggerId, UserId};

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum EvaluationStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    Queued = 0,
    #[sea_orm(num_value = 1)]
    EvaluatingFlake = 1,
    #[sea_orm(num_value = 2)]
    EvaluatingDerivation = 2,
    #[sea_orm(num_value = 3)]
    Building = 3,
    #[sea_orm(num_value = 4)]
    Waiting = 4,
    #[sea_orm(num_value = 5)]
    Completed = 5,
    #[sea_orm(num_value = 6)]
    Failed = 6,
    #[sea_orm(num_value = 7)]
    Aborted = 7,
    #[sea_orm(num_value = 8)]
    Fetching = 8,
}

impl EvaluationStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::Fetching
                | Self::EvaluatingFlake
                | Self::EvaluatingDerivation
                | Self::Building
                | Self::Waiting
        )
    }

    pub const ACTIVE: [Self; 6] = [
        Self::Queued,
        Self::Fetching,
        Self::EvaluatingFlake,
        Self::EvaluatingDerivation,
        Self::Building,
        Self::Waiting,
    ];

    pub const TERMINAL: [Self; 3] = [Self::Completed, Self::Failed, Self::Aborted];
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use sea_orm::Iterable;

    /// Raw SQL composes fragments from these numbers; a renumber must fail CI
    /// (the m20260407 in-place renumber is exactly the hazard this pins).
    #[test]
    fn numbering_is_pinned() {
        for (status, n) in [
            (EvaluationStatus::Queued, 0),
            (EvaluationStatus::EvaluatingFlake, 1),
            (EvaluationStatus::EvaluatingDerivation, 2),
            (EvaluationStatus::Building, 3),
            (EvaluationStatus::Waiting, 4),
            (EvaluationStatus::Completed, 5),
            (EvaluationStatus::Failed, 6),
            (EvaluationStatus::Aborted, 7),
            (EvaluationStatus::Fetching, 8),
        ] {
            assert_eq!(i32::from(status), n);
        }
        assert_eq!(EvaluationStatus::iter().count(), 9);
    }

    #[test]
    fn active_and_terminal_partition_every_status() {
        for status in EvaluationStatus::iter() {
            let active = EvaluationStatus::ACTIVE.contains(&status);
            let terminal = EvaluationStatus::TERMINAL.contains(&status);
            assert!(active ^ terminal, "{status:?} must be exactly one of active/terminal");
            assert_eq!(status.is_active(), active);
        }
    }
}

/// What an evaluation is for: a normal CI run, or an `input_update` run that
/// bumps tracked flake inputs and feeds the `OpenPr` action.
#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum EvaluationKind {
    #[default]
    #[sea_orm(num_value = 0)]
    Normal = 0,
    #[sea_orm(num_value = 1)]
    InputUpdate = 1,
}

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum EvalCacheStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    None = 0,
    #[sea_orm(num_value = 1)]
    Miss = 1,
    #[sea_orm(num_value = 2)]
    Hit = 2,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvaluationId,
    pub project: Option<ProjectId>,
    pub repository: String,
    pub commit: CommitId,
    pub wildcard: String,
    pub status: EvaluationStatus,
    pub kind: EvaluationKind,
    pub cache_status: EvalCacheStatus,
    pub previous: Option<EvaluationId>,
    pub next: Option<EvaluationId>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub flake_source: Option<String>,
    pub check_run_ids: Option<Json>,
    pub waiting_reason: Option<serde_json::Value>,
    pub trigger: Option<ProjectTriggerId>,
    pub started_by: Option<UserId>,
    pub concurrent: bool,
    pub source_comment: Option<serde_json::Value>,
    pub fetch_started_at: Option<NaiveDateTime>,
    pub eval_flake_started_at: Option<NaiveDateTime>,
    pub eval_drv_started_at: Option<NaiveDateTime>,
    pub building_started_at: Option<NaiveDateTime>,
    pub finished_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::commit::Entity",
        from = "Column::Commit",
        to = "super::commit::Column::Id"
    )]
    Commit,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Previous",
        to = "super::evaluation::Column::Id"
    )]
    PreviousEvaluation,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Next",
        to = "super::evaluation::Column::Id"
    )]
    NextEvaluation,
    #[sea_orm(
        belongs_to = "super::project_trigger::Entity",
        from = "Column::Trigger",
        to = "super::project_trigger::Column::Id"
    )]
    Trigger,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::StartedBy",
        to = "super::user::Column::Id",
        on_delete = "SetNull"
    )]
    StartedBy,
}

impl ActiveModelBehavior for ActiveModel {}
