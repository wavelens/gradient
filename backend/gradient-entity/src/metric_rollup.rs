/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::MetricRollupId;

/// Time-bucket width of a rollup row. Minute buckets aggregate from the fact
/// tables; each coarser level cascades from the one below it.
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
pub enum RollupGranularity {
    #[default]
    #[sea_orm(num_value = 0)]
    Minute = 0,
    #[sea_orm(num_value = 1)]
    Hour = 1,
    #[sea_orm(num_value = 2)]
    Day = 2,
    #[sea_orm(num_value = 3)]
    Week = 3,
}

impl RollupGranularity {
    /// The matching Postgres `date_trunc` unit.
    pub const fn trunc_unit(self) -> &'static str {
        match self {
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
        }
    }

    pub fn from_trunc_unit(unit: &str) -> Option<Self> {
        match unit {
            "minute" => Some(Self::Minute),
            "hour" => Some(Self::Hour),
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            _ => None,
        }
    }

    /// Query-parameter decode with the API's historical default of `day`.
    pub fn from_query_param(param: Option<&str>) -> Self {
        param
            .and_then(Self::from_trunc_unit)
            .unwrap_or(Self::Day)
    }
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "metric_rollup")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: MetricRollupId,
    pub metric: String,
    pub granularity: RollupGranularity,
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
