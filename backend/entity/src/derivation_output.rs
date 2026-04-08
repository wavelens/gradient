/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_output")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub derivation: Uuid,
    pub name: String,
    pub output: String,
    pub hash: String,
    pub package: String,
    pub ca: Option<String>,
    pub file_hash: Option<String>,
    pub file_size: Option<i64>,
    pub nar_size: Option<i64>,
    pub is_cached: bool,
    pub has_artefacts: bool,
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
}

impl ActiveModelBehavior for ActiveModel {}
