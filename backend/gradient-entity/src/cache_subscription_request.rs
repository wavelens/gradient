/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, CacheSubscriptionRequestId, ProjectId, UserId};
use crate::project_cache::CacheSubscriptionMode;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_subscription_request")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CacheSubscriptionRequestId,
    pub project: ProjectId,
    pub cache: CacheId,
    pub mode: CacheSubscriptionMode,
    pub requested_by: UserId,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Project,
    Cache,
    RequestedBy,
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
            Self::RequestedBy => Entity::belongs_to(super::user::Entity)
                .from(Column::RequestedBy)
                .to(super::user::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
