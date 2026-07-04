/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::UpstreamMetricId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "upstream_metric")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: UpstreamMetricId,
    pub upstream_url: String,
    pub bucket_time: NaiveDateTime,
    pub latency_ms_sum: f64,
    pub request_count: i32,
    pub narinfo_hits: i32,
    pub narinfo_misses: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
