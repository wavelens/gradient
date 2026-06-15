/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, EvaluationMetricId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation_metric")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvaluationMetricId,
    pub evaluation: EvaluationId,
    pub total_thunks: i64,
    pub fn_calls: i64,
    pub primop_calls: i64,
    pub lookups: i64,
    pub alloc_bytes: i64,
    pub peak_heap_mb: i64,
    pub peak_rss_mb: i64,
    pub fetch_ms: i64,
    pub eval_flake_ms: i64,
    pub eval_drv_ms: i64,
    pub total_eval_ms: i64,
    pub worker_id: String,
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
