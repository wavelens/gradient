/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvalShardId, EvaluationId};

#[repr(i16)]
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
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum EvalShardState {
    #[default]
    #[sea_orm(num_value = 0)]
    Queued = 0,
    #[sea_orm(num_value = 1)]
    Running = 1,
    #[sea_orm(num_value = 2)]
    Done = 2,
    #[sea_orm(num_value = 3)]
    Failed = 3,
    #[sea_orm(num_value = 4)]
    Split = 4,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "eval_shard")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvalShardId,
    pub evaluation: EvaluationId,
    pub seq: i32,
    pub pattern: String,
    pub children: Option<Vec<String>>,
    pub predicted_ms: f64,
    pub state: EvalShardState,
    pub attempts: i16,
    pub reported_by_job: Option<String>,
    pub taken_job: Option<String>,
    pub created_at: NaiveDateTime,
    pub taken_at: Option<NaiveDateTime>,
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
}

impl ActiveModelBehavior for ActiveModel {}
