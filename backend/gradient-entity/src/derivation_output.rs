/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CachedPathId, DerivationId, DerivationOutputId};

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
    /// Upstream NAR URL resolved once via the org's upstream-cache narinfo
    /// lookup. Set (with `is_cached` false) when the output is available upstream
    /// but not yet pulled into the gradient cache.
    pub external_url: Option<String>,
    /// NAR hash from the upstream narinfo, needed to import the path.
    pub nar_hash: Option<String>,
    /// Compressed-NAR hash from the upstream narinfo, in `sha256:<nix32>` form.
    /// Lets the worker relay a verbatim NAR without recomputing the file hash.
    pub file_hash: Option<String>,
    /// Compressed NAR size from the upstream narinfo.
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
    Cached { cached_path: CachedPathId },
}

impl Model {
    /// Return the cache link state for this derivation output.
    pub fn cache_link(&self) -> CacheLink {
        match (self.is_cached, self.cached_path) {
            (true, Some(id)) => CacheLink::Cached { cached_path: id },
            _ => CacheLink::NotCached,
        }
    }

    /// Whether this output is available anywhere: in the gradient cache
    /// (`is_cached`) or resolved at an org upstream (`external_url`). A
    /// derivation is only substitutable when every output is cached somewhere.
    pub fn is_cached_anywhere(&self) -> bool {
        self.is_cached || self.external_url.is_some()
    }
}
