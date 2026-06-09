/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::MetricRollupId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "metric_rollup")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: MetricRollupId,
    pub metric: String,
    pub granularity: i16,
    pub bucket_start: NaiveDateTime,
    pub scope: Json,
    pub scope_hash: i64,
    pub count: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub sum_sq: f64,
    pub histogram: Option<Json>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
