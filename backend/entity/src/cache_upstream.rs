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
