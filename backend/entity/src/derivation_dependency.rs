/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_dependency")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub derivation: Uuid,
    pub dependency: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Derivation,
    Dependency,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Derivation => Entity::belongs_to(super::derivation::Entity)
                .from(Column::Derivation)
                .to(super::derivation::Column::Id)
                .into(),
            Self::Dependency => Entity::belongs_to(super::derivation::Entity)
                .from(Column::Dependency)
                .to(super::derivation::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
