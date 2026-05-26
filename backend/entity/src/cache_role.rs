/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, RoleId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_role")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: RoleId,
    #[sea_orm(indexed)]
    pub name: String,
    pub cache: Option<CacheId>,
    pub permission: i64,
    pub managed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cache::Entity",
        from = "Column::Cache",
        to = "super::cache::Column::Id"
    )]
    Cache,
}

impl ActiveModelBehavior for ActiveModel {}
