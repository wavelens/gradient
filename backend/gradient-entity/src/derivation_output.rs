/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path` is authoritative for anything in our cache. The narinfo fields here are an
//! upstream snapshot only, written with `external_url` before any `cached_path` row exists. A
//! demote is clearing the whole upstream snapshot together.

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CachedPathId, DerivationId, DerivationOutputId};

/// This `hash` sentinel is written for an output store path the evaluator could not parse, e.g. a
/// floating CA output before its build. Nothing is rewriting it later.
pub const UNKNOWN_OUTPUT_HASH: &str = "unknown";

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_output")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationOutputId,
    pub derivation: DerivationId,
    pub name: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
    pub nar_size: Option<i64>,
    pub is_cached: bool,
    pub cached_path: Option<CachedPathId>,
    pub external_url: Option<String>,
    pub nar_hash: Option<String>,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    #[sea_orm(column_name = "references_list")]
    pub references: Option<String>,
    pub deriver: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
    #[sea_orm(
        belongs_to = "super::cached_path::Entity",
        from = "Column::CachedPath",
        to = "super::cached_path::Column::Id"
    )]
    CachedPath,
    #[sea_orm(has_many = "super::build_product::Entity")]
    BuildProduct,
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLink {
    NotCached,
    Cached { cached_path: CachedPathId },
}

impl Model {
    pub fn cache_link(&self) -> CacheLink {
        match (self.is_cached, self.cached_path) {
            (true, Some(id)) => CacheLink::Cached { cached_path: id },
            _ => CacheLink::NotCached,
        }
    }

    pub fn is_cached_anywhere(&self) -> bool {
        self.is_cached || self.external_url.is_some()
    }
}
