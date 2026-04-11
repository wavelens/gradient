/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_machine_architecture")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub build_machine: Uuid,
    pub architecture: super::build_machine::Architecture,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::build_machine::Entity",
        from = "Column::BuildMachine",
        to = "super::build_machine::Column::Id"
    )]
    BuildMachine,
}

impl ActiveModelBehavior for ActiveModel {}
