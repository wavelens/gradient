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
    pub hash: String,
    pub nar_object_storage_key: String,
    pub package: String,
    pub file_hash: String,
    pub file_size: Option<i64>,
    pub nar_size: i64,
    pub nar_hash: String,
    pub ca: Option<String>,
    pub references: Option<String>,
    pub deriver: Option<String>,
    pub debug_info_indexed: bool,
    pub server_computed_hashes_and_sizes: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn as_store_path(&self) -> crate::StorePath {
        crate::StorePath::from_hash_and_name_without_validation(
            self.hash.clone(),
            self.package.clone(),
        )
    }

    pub fn store_path(&self) -> String {
        self.as_store_path().full()
    }

    pub fn is_server_verified_and_matches_nar_hash_references_text_and_content_address(
        &self,
        nar_hash: &str,
        references_text: &str,
        ca: Option<&str>,
    ) -> bool {
        self.server_computed_hashes_and_sizes
            && self.nar_hash == nar_hash
            && self.references.as_deref().unwrap_or_default() == references_text
            && self.ca.as_deref() == ca
    }
}

pub fn join_sorted_deduplicated_references_with_spaces(references: &[String]) -> String {
    let mut sorted: Vec<&str> = references.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.join(" ")
}
