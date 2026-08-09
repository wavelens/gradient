/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, EntryPointId, EvaluationId, TaskId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "entry_point")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EntryPointId,
    pub task: TaskId,
    pub evaluation: EvaluationId,
    pub derivation: DerivationId,
    pub eval: String,
    pub created_at: NaiveDateTime,
    pub repo_check_id: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::task::Entity",
        from = "Column::Task",
        to = "super::task::Column::Id"
    )]
    Task,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id"
    )]
    Evaluation,
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
