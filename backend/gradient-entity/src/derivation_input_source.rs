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

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_input_source")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationInputSourceId,
    pub derivation: DerivationId,
    pub hash: String,
    pub store_path: String,
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
