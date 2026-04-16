/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Associates a `cached_path` with a cache, optionally carrying a signature.
///
/// This is the many-to-many join between store paths and caches. A single
/// NAR (one `cached_path` row) can be served from multiple caches, each
/// with its own signature. Rows are created when NARs are pushed; the
/// `signature` starts as `None` and is filled by a signing job.
///
/// Signature format (when set): `<key-name>:<base64>` (standard Nix narinfo).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cached_path_signature")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub cached_path: Uuid,
    pub cache: Uuid,
    pub signature: Option<String>,
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
