/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::EvalCacheStoreId;

/// A fleet-shared Nix eval-cache blob, keyed by flake fingerprint.
///
/// `storage_path` is the object-store key for the serialized eval-cache blob.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "eval_cache_store")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: EvalCacheStoreId,
    #[sea_orm(unique)]
    pub fingerprint: String,
    pub storage_path: String,
    pub size_bytes: i64,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
