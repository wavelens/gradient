/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::{CacheUpstreamId, UpstreamMetricId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "upstream_metric")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: UpstreamMetricId,
    pub upstream: CacheUpstreamId,
    pub bucket_time: NaiveDateTime,
    pub latency_ms_sum: f64,
    pub request_count: i32,
    pub narinfo_hits: i32,
    pub narinfo_misses: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cache_upstream::Entity",
        from = "Column::Upstream",
        to = "super::cache_upstream::Column::Id"
    )]
    Upstream,
}

impl ActiveModelBehavior for ActiveModel {}
