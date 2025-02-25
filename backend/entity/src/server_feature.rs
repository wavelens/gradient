/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server_feature")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub server: Uuid,
    pub feature: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Server,
    Feature,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Server => Entity::belongs_to(super::server::Entity)
                .from(Column::Server)
                .to(super::server::Column::Id)
                .into(),
            Self::Feature => Entity::belongs_to(super::feature::Entity)
                .from(Column::Feature)
                .to(super::feature::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
