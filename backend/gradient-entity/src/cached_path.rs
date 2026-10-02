/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::CachedPathId;

/// This row is the authoritative narinfo source for anything in our cache. The narinfo fields on
/// `derivation_output` are only an upstream snapshot for paths not yet pulled.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cached_path")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CachedPathId,
    #[sea_orm(unique)]
    pub hash: String,
    pub package: String,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    pub nar_size: Option<i64>,
    pub nar_hash: Option<String>,
    pub ca: Option<String>,
    pub references: Option<String>,
    pub deriver: Option<String>,
    pub debug_info_indexed: bool,
    /// Every commit is writing `true`. A `false` row is older than upload admission and is healed
    /// once served.
    pub confirmed: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn as_store_path(&self) -> crate::StorePath {
        crate::StorePath::from_parts(self.hash.clone(), self.package.clone())
    }

    pub fn store_path(&self) -> String {
        self.as_store_path().full()
    }

    pub fn is_fully_cached(&self) -> bool {
        self.file_hash.is_some()
    }
}
