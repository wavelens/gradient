/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum EvaluationStatus {
    #[sea_orm(num_value = 0)]
    Queued,
    #[sea_orm(num_value = 1)]
    Evaluating,
    #[sea_orm(num_value = 2)]
    Building,
    #[sea_orm(num_value = 3)]
    Completed,
    #[sea_orm(num_value = 4)]
    Failed,
    #[sea_orm(num_value = 5)]
    Aborted,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub project: Uuid,
    pub repository: String,
    pub commit: Uuid,
    pub evaluation_wildcard: String,
    pub status: EvaluationStatus,
    pub previous: Option<Uuid>,
    pub next: Option<Uuid>,
    pub created_at: NaiveDateTime,
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
}

impl ActiveModelBehavior for ActiveModel {}
