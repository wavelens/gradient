/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_dependency")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub build: Uuid,
    pub dependency: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Build,
    Dependency,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Build => Entity::belongs_to(super::build::Entity)
                .from(Column::Build)
                .to(super::build::Column::Id)
                .into(),
            Self::Dependency => Entity::belongs_to(super::build::Entity)
                .from(Column::Dependency)
                .to(super::build::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
