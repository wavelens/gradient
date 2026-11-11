/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, ProjectCacheId, ProjectId};

#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum CacheSubscriptionMode {
    #[default]
    #[sea_orm(num_value = 0)]
    ReadWrite,
    #[sea_orm(num_value = 1)]
    ReadOnly,
    #[sea_orm(num_value = 2)]
    WriteOnly,
}

impl CacheSubscriptionMode {
    pub fn writes_build_outputs_to_cache(&self) -> bool {
        matches!(self, Self::ReadWrite | Self::WriteOnly)
    }

    pub fn grants_no_access_beyond(&self, other: &Self) -> bool {
        self == other || *other == Self::ReadWrite
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_cache")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectCacheId,
    pub project: ProjectId,
    pub cache: CacheId,
    pub mode: CacheSubscriptionMode,
    pub managed: bool,
    pub sign_outputs: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            id: ProjectCacheId::now_v7(),
            project: ProjectId::default(),
            cache: CacheId::default(),
            mode: CacheSubscriptionMode::default(),
            managed: false,
            sign_outputs: true,
        }
    }
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Project,
    Cache,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Project => Entity::belongs_to(super::project::Entity)
                .from(Column::Project)
                .to(super::project::Column::Id)
                .into(),
            Self::Cache => Entity::belongs_to(super::cache::Entity)
                .from(Column::Cache)
                .to(super::cache::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
