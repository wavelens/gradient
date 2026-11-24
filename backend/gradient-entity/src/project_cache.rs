/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, ProjectCacheId, ProjectId};
pub use gradient_api_types::CacheSubscriptionMode;

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
    /// Only the trigger `set_sign_backfill_requested_at_on_sign_outputs_change` can write this column. The background pass for signature backfill can clear this column.
    pub unsigned_signature_backfill_requested_at: Option<DateTime>,
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
            unsigned_signature_backfill_requested_at: None,
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
