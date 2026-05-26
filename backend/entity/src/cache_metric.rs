/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::{CacheId, CacheMetricId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "cache_metric")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CacheMetricId,
    pub cache: CacheId,
    pub bucket_time: NaiveDateTime,
    pub bytes_sent: i64,
    pub nar_count: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cache::Entity",
        from = "Column::Cache",
        to = "super::cache::Column::Id"
    )]
    Cache,
}

impl ActiveModelBehavior for ActiveModel {}
