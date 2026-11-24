/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildProductId, DerivationOutputId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_product")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildProductId,
    pub derivation_output: DerivationOutputId,
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<i64>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation_output::Entity",
        from = "Column::DerivationOutput",
        to = "super::derivation_output::Column::Id"
    )]
    DerivationOutput,
}

impl ActiveModelBehavior for ActiveModel {}

impl sea_orm::Related<super::derivation_output::Entity> for Entity {
    fn to() -> sea_orm::RelationDef {
        Relation::DerivationOutput.def()
    }
}

impl From<Model> for gradient_api_types::BuildProduct {
    fn from(product: Model) -> Self {
        Self {
            file_type: product.file_type,
            subtype: product.subtype,
            name: product.name,
            path: product.path,
            size: product.size.map(|s| s as u64),
        }
    }
}

impl From<Model> for gradient_api_types::ProductArtefact {
    fn from(p: Model) -> Self {
        Self {
            id: p.id,
            file_type: p.file_type,
            subtype: p.subtype,
            name: p.name,
            path: p.path,
            size: p.size,
        }
    }
}
