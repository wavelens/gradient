/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A derivation's `inputSrcs` - build-time source paths (e.g.
//! `builtins.toFile` configs) that have no producing derivation. Recorded per
//! derivation so the dispatch readiness gate can require every source to be in
//! the cache before a real build is dispatched.

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, DerivationInputSourceId};
use crate::store_path::StorePath;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_input_source")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationInputSourceId,
    pub derivation: DerivationId,
    pub hash: String,
    #[sea_orm(column_type = "Text")]
    pub store_path: StorePath,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id",
        on_delete = "Cascade"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
