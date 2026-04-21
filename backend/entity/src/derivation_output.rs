/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_output")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub derivation: Uuid,
    pub name: String,
    pub output: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    pub nar_size: Option<i64>,
    pub is_cached: bool,
    /// Link to the `cached_path` row when this output is cached.
    /// Replaces the old `derivation_output_signature` join — the signature
    /// lives on `cached_path` directly.
    pub cached_path: Option<Uuid>,
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

/// Whether a [`derivation_output`](Model) has been uploaded to a Gradient
/// cache.
///
/// The `is_cached` flag and `cached_path` UUID together encode this state.
/// [`CacheLink`] makes the pairing explicit and prevents reading
/// `cached_path` when the output is not yet cached.
///
/// Obtain via [`Model::cache_link`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLink {
    /// The output has not been uploaded to a cache yet.
    NotCached,
    /// The output is cached; `cached_path` is the ID of the
    /// `cached_path` row that holds the NAR metadata.
    Cached { cached_path: uuid::Uuid },
}

impl Model {
    /// Return the cache link state for this derivation output.
    pub fn cache_link(&self) -> CacheLink {
        match (self.is_cached, self.cached_path) {
            (true, Some(id)) => CacheLink::Cached { cached_path: id },
            _ => CacheLink::NotCached,
        }
    }
}
