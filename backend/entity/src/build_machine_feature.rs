/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_machine_feature")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub build_machine: Uuid,
    pub feature: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    BuildMachine,
    Feature,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::BuildMachine => Entity::belongs_to(super::build_machine::Entity)
                .from(Column::BuildMachine)
                .to(super::build_machine::Column::Id)
                .into(),
            Self::Feature => Entity::belongs_to(super::feature::Entity)
                .from(Column::Feature)
                .to(super::feature::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
