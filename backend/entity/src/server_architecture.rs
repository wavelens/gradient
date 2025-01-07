/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server_architecture")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub server: Uuid,
    pub architecture: super::server::Architecture,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::server::Entity",
        from = "Column::Server",
        to = "super::server::Column::Id"
    )]
    Server,
}

impl ActiveModelBehavior for ActiveModel {}
