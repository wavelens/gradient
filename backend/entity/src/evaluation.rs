/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
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

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub project: Option<Uuid>,
    pub repository: String,
    pub commit: Uuid,
    pub wildcard: String,
    pub status: EvaluationStatus,
    pub previous: Option<Uuid>,
    pub next: Option<Uuid>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
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
