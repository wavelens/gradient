/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Presence tracking: records which caches currently hold a complete copy of
/// a derivation's outputs (including the transitive closure). The cacher
/// maintains the invariant that a row exists iff every `derivation_output` of
/// the derivation is `is_cached = true` AND every transitive dependency also
/// has a `cache_derivation` row for the same cache. One row therefore answers
/// "is the full closure of this derivation available in this cache".
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_derivation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub cache: Uuid,
    pub derivation: Uuid,
    pub cached_at: NaiveDateTime,
    pub last_fetched_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cache::Entity",
        from = "Column::Cache",
        to = "super::cache::Column::Id"
    )]
    Cache,
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
