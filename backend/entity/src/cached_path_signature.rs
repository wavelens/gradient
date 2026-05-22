/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, CachedPathId, CachedPathSignatureId};

/// Associates a `cached_path` with a cache, optionally carrying a signature.
///
/// This is the many-to-many join between store paths and caches. A single
/// NAR (one `cached_path` row) can be served from multiple caches, each
/// with its own signature. Rows are created when NARs are pushed; the
/// `signature` starts as `None` and is filled by a signing job.
///
/// `signature` stores the raw 64-byte Ed25519 signature. The narinfo wire
/// form (`<key-name>:<base64>`) is reconstructed at read time from
/// `cache.name` + the deployment's `serve_url`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cached_path_signature")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CachedPathSignatureId,
    pub cached_path: CachedPathId,
    pub cache: CacheId,
    pub signature: Option<Vec<u8>>,
    pub last_fetched_at: Option<NaiveDateTime>,
    #[sea_orm(default_value = "0")]
    pub fetch_count: i64,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cached_path::Entity",
        from = "Column::CachedPath",
        to = "super::cached_path::Column::Id"
    )]
    CachedPath,
    #[sea_orm(
        belongs_to = "super::cache::Entity",
        from = "Column::Cache",
        to = "super::cache::Column::Id"
    )]
    Cache,
}

impl ActiveModelBehavior for ActiveModel {}
