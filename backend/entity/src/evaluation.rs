/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CommitId, EvaluationId, ProjectId, ProjectTriggerId};

#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum EvaluationStatus {
    #[sea_orm(num_value = 0)]
    Queued,
    #[sea_orm(num_value = 1)]
    EvaluatingFlake,
    #[sea_orm(num_value = 2)]
    EvaluatingDerivation,
    #[sea_orm(num_value = 3)]
    Building,
    #[sea_orm(num_value = 4)]
    Waiting,
    #[sea_orm(num_value = 5)]
    Completed,
    #[sea_orm(num_value = 6)]
    Failed,
    #[sea_orm(num_value = 7)]
    Aborted,
    #[sea_orm(num_value = 8)]
    Fetching,
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

    pub const fn num_value(&self) -> i32 {
        match self {
            Self::Queued => 0,
            Self::EvaluatingFlake => 1,
            Self::EvaluatingDerivation => 2,
            Self::Building => 3,
            Self::Waiting => 4,
            Self::Completed => 5,
            Self::Failed => 6,
            Self::Aborted => 7,
            Self::Fetching => 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvaluationId,
    pub project: Option<ProjectId>,
    pub repository: String,
    pub commit: CommitId,
    pub wildcard: String,
    pub status: EvaluationStatus,
    pub previous: Option<EvaluationId>,
    pub next: Option<EvaluationId>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub flake_source: Option<String>,
    pub repo_check_id: Option<i64>,
    pub waiting_reason: Option<serde_json::Value>,
    pub trigger: Option<ProjectTriggerId>,
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
}

impl ActiveModelBehavior for ActiveModel {}
