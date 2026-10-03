/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, EvaluationMessageId};

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Hash, DeriveActiveEnum, EnumIter, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum MessageLevel {
    #[default]
    #[sea_orm(num_value = 0)]
    Error,
    #[sea_orm(num_value = 1)]
    Warning,
    #[sea_orm(num_value = 2)]
    Notice,
}

/// Messages without `entry_point_message` rows are evaluation-scoped. `source` is one of
/// `flake-prefetch`, `nix-eval`, `nix-eval:<attr>`, `dep-graph`, `db-insert`, `scheduler` or
/// `upstream-probe`.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation_message")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvaluationMessageId,
    pub evaluation: EvaluationId,
    pub level: MessageLevel,
    pub message: String,
    pub source: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id"
    )]
    Evaluation,
}

impl ActiveModelBehavior for ActiveModel {}
