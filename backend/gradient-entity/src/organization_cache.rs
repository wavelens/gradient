/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, OrganizationCacheId, OrganizationId};

#[derive(
    Debug, Clone, Default, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum CacheSubscriptionMode {
    /// Read from and write to this cache (default).
    #[default]
    #[sea_orm(num_value = 0)]
    ReadWrite,
    /// Only read (use as binary cache substituter, never push to it).
    #[sea_orm(num_value = 1)]
    ReadOnly,
    /// Only write (push build outputs, never use as substituter).
    #[sea_orm(num_value = 2)]
    WriteOnly,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "organization_cache")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: OrganizationCacheId,
    pub organization: OrganizationId,
    pub cache: CacheId,
    pub mode: CacheSubscriptionMode,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Organization,
    Cache,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Organization => Entity::belongs_to(super::organization::Entity)
                .from(Column::Organization)
                .to(super::organization::Column::Id)
                .into(),
            Self::Cache => Entity::belongs_to(super::cache::Entity)
                .from(Column::Cache)
                .to(super::cache::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
