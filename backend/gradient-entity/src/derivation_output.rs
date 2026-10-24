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

use crate::ids::{DerivationId, DerivationOutputId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_output")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationOutputId,
    pub derivation: DerivationId,
    pub name: String,
    pub hash: Option<String>,
    pub package: Option<String>,
    pub ca: Option<String>,
    pub nar_size: Option<i64>,
    pub external_url: Option<String>,
    pub nar_hash: Option<String>,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    #[sea_orm(column_name = "references_list")]
    pub references: Option<String>,
    pub deriver: Option<String>,
    pub path_known_after_build: bool,
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
    #[sea_orm(has_many = "super::build_product::Entity")]
    BuildProduct,
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn store_path(&self) -> Option<crate::store_path::StorePath> {
        let (hash, package) = (self.hash.as_ref()?, self.package.as_ref()?);
        Some(crate::store_path::StorePath::from_stored(hash, package))
    }
}
