/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::organization_cache::CacheSubscriptionMode;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An upstream cache entry attached to a Gradient cache.
///
/// Exactly one of `upstream_cache` (internal) or `url`+`public_key` (external)
/// must be populated — enforced at the application level.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_upstream")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    /// The owning Gradient cache that has this upstream configured.
    pub cache: Uuid,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    /// Set when the upstream is another Gradient-managed cache.
    pub upstream_cache: Option<Uuid>,
    /// Set when the upstream is an external cache.
    pub url: Option<String>,
    /// Trusted public key for the external cache (Nix signing key format).
    pub public_key: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Cache,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Cache => Entity::belongs_to(super::cache::Entity)
                .from(Column::Cache)
                .to(super::cache::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Whether a [`cache_upstream`](Model) points to an internal Gradient cache
/// or an external Nix binary cache.
///
/// Exactly one variant is valid for a given row — the invariant is enforced
/// at the application level (the database allows invalid states).
///
/// Obtain via [`Model::as_source`].
#[derive(Debug, Clone, PartialEq)]
pub enum CacheUpstreamSource<'a> {
    /// Points to another Gradient-managed cache (referenced by ID).
    Internal { cache_id: Uuid },
    /// Points to an external Nix binary cache.
    External {
        /// URL of the binary cache (e.g. `https://cache.nixos.org`).
        url: &'a str,
        /// Trusted signing public key in Nix narinfo format
        /// (`<key-name>:<base64-public-key>`).
        public_key: &'a str,
    },
}

impl Model {
    /// Return the upstream source kind for this record.
    ///
    /// Returns `None` only when the row is in an inconsistent state (neither
    /// `upstream_cache` nor both `url` + `public_key` are set), which should
    /// not occur in a correctly maintained database.
    pub fn as_source(&self) -> Option<CacheUpstreamSource<'_>> {
        match (&self.upstream_cache, &self.url, &self.public_key) {
            (Some(id), _, _) => Some(CacheUpstreamSource::Internal { cache_id: *id }),
            (None, Some(url), Some(key)) => Some(CacheUpstreamSource::External {
                url: url.as_str(),
                public_key: key.as_str(),
            }),
            _ => None,
        }
    }
}
